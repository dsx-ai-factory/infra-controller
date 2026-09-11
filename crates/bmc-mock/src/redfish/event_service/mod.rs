// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Bounded per-BMC SSE publication, replay, and deterministic stream faults.

mod routes;
mod state;
mod stream;

pub(crate) use routes::{add_routes, resource, with_lifetime};
pub use state::{
    EventServiceConfig, EventServiceError, EventServiceLimits, EventServiceState,
    EventServiceStats, StreamStep,
};

const ROOT: &str = "/redfish/v1/EventService";
const SSE: &str = "/redfish/v1/EventService/SSE";
const SUBSCRIPTIONS: &str = "/redfish/v1/EventService/Subscriptions";
const MAX_SCRIPTS: usize = 8;
const MAX_SCRIPT_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 4 * MAX_SCRIPT_BYTES;
const MAX_SCRIPT_STEPS: usize = 256;
const MAX_SCRIPT_DELAY_MS: u64 = 60_000;

/// Payload and state fixtures shared by the state, stream, and router tests.
#[cfg(test)]
mod fixtures {
    use std::sync::Arc;

    use serde_json::{Value, json};

    use super::{EventServiceConfig, EventServiceLimits, EventServiceState};

    pub(super) fn event() -> Value {
        json!({"@odata.id": "/redfish/v1/EventService/SSE#/Event1",
            "@odata.type": "#Event.v1_6_0.Event", "Id": "1", "Name": "Test event",
            "Events": [{"@odata.id": "/redfish/v1/EventService/SSE#/Events/1",
                "MemberId": "1", "EventId": "application-id", "EventType": "Alert",
                "MessageId": "ResourceEvent.1.2.ResourceRemoved", "Message": "Resource removed",
                "EventTimestamp": "2026-09-10T12:00:00Z", "MessageSeverity": "OK"}]})
    }

    pub(super) fn metric() -> Value {
        json!({"@odata.id": "/redfish/v1/TelemetryService/MetricReports/Power",
            "@odata.type": "#MetricReport.v1_3_0.MetricReport", "Id": "Power", "Name": "Power",
            "MetricReportDefinition": {"@odata.id": "/redfish/v1/TelemetryService/MetricReportDefinitions/Power"},
            "MetricValues": [{"MetricId": "Watts", "MetricValue": "100",
                "Timestamp": "2026-09-10T12:00:00Z",
                "MetricProperty": "/redfish/v1/Chassis/1/Power#/PowerControl/0/PowerConsumedWatts"}]})
    }

    /// Heartbeats off; other limits default.
    pub(super) fn limits(
        max_subscribers: usize,
        max_frame_bytes: usize,
        max_frames: usize,
        max_history_bytes: usize,
    ) -> EventServiceConfig {
        EventServiceConfig::try_from(EventServiceLimits {
            max_subscribers,
            max_frame_bytes,
            max_frames,
            max_history_bytes,
            heartbeat: None,
            ..Default::default()
        })
        .unwrap()
    }

    pub(super) fn state(frames: usize) -> Arc<EventServiceState> {
        EventServiceState::new(limits(2, 4096, frames, 8192))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use axum::response::Response;
    use bytes::Bytes;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use http_body_util::BodyExt;
    use nv_redfish::event_service::EventStreamPayload;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::fixtures::{event, limits, metric};
    use super::*;
    use crate::test_support::{NoopCallbacks, host_info, serve_https};
    use crate::{
        BmcEvent, BmcState, EventServiceOverride, HardwareType, MachineRouterOptions,
        machine_router,
    };

    fn router(auth: bool) -> (Router, BmcState) {
        machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "sse-test".into(),
            auth,
            MachineRouterOptions {
                bmc_reset_duration: Some(Duration::from_millis(10)),
                ..Default::default()
            },
        )
    }

    fn router_with(limits: EventServiceConfig) -> (Router, BmcState) {
        machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "sse-limits-test".into(),
            false,
            MachineRouterOptions {
                event_service: EventServiceOverride::Limits(limits),
                ..Default::default()
            },
        )
    }

    async fn request(router: &Router, method: &str, uri: &str, payload: Option<Value>) -> Response {
        let body = payload
            .map(|v| Body::from(v.to_string()))
            .unwrap_or_default();
        tokio::time::timeout(
            Duration::from_secs(2),
            router.clone().oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(body)
                    .unwrap(),
            ),
        )
        .await
        .expect("response headers must not wait for EOF")
        .unwrap()
    }

    async fn resume(router: &Router, last_event_id: &str) -> Response {
        router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(SSE)
                    .header("last-event-id", last_event_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json_body(response: Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    async fn frame(body: &mut Body) -> Bytes {
        tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap()
    }

    /// The JSON document of one `data:` SSE frame.
    fn payload(frame: &Bytes) -> Value {
        let text = std::str::from_utf8(frame).unwrap();
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("frame carries a data line");
        serde_json::from_str(data).unwrap()
    }

    async fn manager_reset_target(router: &Router) -> String {
        let managers = json_body(request(router, "GET", "/redfish/v1/Managers", None).await).await;
        let manager_path = managers["Members"][0]["@odata.id"].as_str().unwrap();
        let manager = json_body(request(router, "GET", manager_path, None).await).await;
        manager["Actions"]["#Manager.Reset"]["target"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn hardware_profiles_control_discovery_and_routes() {
        check_cases_async(
            [
                Case {
                    scenario: "Dell R750 host profile",
                    input: (
                        HardwareType::DellPowerEdgeR750,
                        false,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "BlueField-3 DPU profile",
                    input: (
                        HardwareType::DellPowerEdgeR750,
                        true,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "BlueField-4 DPU profile",
                    input: (
                        HardwareType::DellPowerEdgeR760Bf4,
                        true,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "power shelf profile",
                    input: (
                        HardwareType::DeltaPowerShelf,
                        false,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "switch profile",
                    input: (
                        HardwareType::NvidiaSwitchNd5200Ld,
                        false,
                        EventServiceOverride::Profile,
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
                Case {
                    scenario: "generic profile with limit overrides",
                    input: (
                        HardwareType::GenericAmi,
                        false,
                        EventServiceOverride::Limits(EventServiceConfig::default()),
                    ),
                    expect: Yields((true, true, StatusCode::OK)),
                },
            ],
            |(hardware, dpu, event_service)| async move {
                let info = if dpu {
                    use crate::mac_address_pool::{Config, MacAddressPool, PoolConfig};
                    let mut pool = MacAddressPool::new(Config {
                        ranges: None,
                        pool: Some(
                            PoolConfig::new(mac_address::MacAddress::new([2, 0, 0, 0, 0, 0]), 16)
                                .unwrap(),
                        ),
                    });
                    crate::MachineInfo::Dpu(crate::DpuMachineInfo::new(
                        hardware,
                        &mut pool,
                        crate::machine_info::DpuSettings::default(),
                    ))
                } else {
                    host_info(hardware)
                };
                let (router, state) = machine_router(
                    &info,
                    Arc::new(NoopCallbacks),
                    "hardware-event-service".into(),
                    false,
                    MachineRouterOptions {
                        event_service,
                        ..Default::default()
                    },
                );
                let root = json_body(request(&router, "GET", "/redfish/v1", None).await).await;
                let response = request(&router, "GET", SSE, None).await;
                Ok::<_, std::convert::Infallible>((
                    state.event_service.is_some(),
                    root.get("EventService").is_some(),
                    response.status(),
                ))
            },
        )
        .await;
    }

    #[tokio::test]
    async fn publication_uses_consumer_payload_discriminator() {
        let (router, bmc) = router(false);
        check_cases_async(
            [
                Case {
                    scenario: "OEM namespace",
                    input: ("#Oem.Nvidia.Event", event()),
                    expect: Yields(StatusCode::OK),
                },
                Case {
                    scenario: "unqualified type accepted by consumer",
                    input: ("#Event", event()),
                    expect: Yields(StatusCode::OK),
                },
                Case {
                    scenario: "final type segment controls decoding",
                    input: ("#Event.v1_0_0.MetricReport", metric()),
                    expect: Yields(StatusCode::OK),
                },
            ],
            |(kind, mut payload)| {
                let router = router.clone();
                async move {
                    payload["@odata.type"] = json!(kind);
                    assert!(serde_json::from_value::<EventStreamPayload>(payload.clone()).is_ok());
                    Ok::<_, std::convert::Infallible>(
                        request(&router, "POST", "/Mock/EventService/events", Some(payload))
                            .await
                            .status(),
                    )
                }
            },
        )
        .await;
        assert_eq!(
            bmc.event_service.as_ref().unwrap().stats().retained_frames,
            3
        );
    }

    #[tokio::test]
    async fn unknown_subscriptions_return_redfish_errors() {
        let (router, _) = router(false);
        check_cases_async(
            [
                Case {
                    scenario: "missing member",
                    input: ("GET", "999999"),
                    expect: Yields((StatusCode::NOT_FOUND, true)),
                },
                Case {
                    scenario: "nonnumeric member",
                    input: ("GET", "garbage"),
                    expect: Yields((StatusCode::NOT_FOUND, true)),
                },
                Case {
                    scenario: "deleting missing member",
                    input: ("DELETE", "garbage"),
                    expect: Yields((StatusCode::NOT_FOUND, true)),
                },
            ],
            |(method, id)| {
                let router = router.clone();
                async move {
                    let response =
                        request(&router, method, &format!("{SUBSCRIPTIONS}/{id}"), None).await;
                    let status = response.status();
                    let error = json_body(response).await;
                    Ok::<_, std::convert::Infallible>((status, error["error"]["code"].is_string()))
                }
            },
        )
        .await;
    }

    #[tokio::test]
    async fn closing_unpolled_bodies_holds_admission_until_drop() {
        check_cases_async(
            [
                Case {
                    scenario: "explicit close",
                    input: "close",
                    expect: Yields(()),
                },
                Case {
                    scenario: "BMC reset",
                    input: "reset",
                    expect: Yields(()),
                },
                Case {
                    scenario: "subscription deletion",
                    input: "delete",
                    expect: Yields(()),
                },
            ],
            |operation| async move {
                let (router, bmc) = router_with(limits(1, 4096, 2, 8192));
                let state = bmc.event_service.as_ref().unwrap();
                state
                    .queue_script(vec![StreamStep::Bytes {
                        data: vec![0; MAX_SCRIPT_BYTES],
                    }])
                    .unwrap();
                let response = request(&router, "GET", SSE, None).await;
                assert_eq!(response.status(), StatusCode::OK);
                match operation {
                    "close" => state.close_subscribers(),
                    "reset" => {
                        bmc.reset();
                    }
                    "delete" => {
                        let member = json_body(request(&router, "GET", SUBSCRIPTIONS, None).await)
                            .await["Members"][0]["@odata.id"]
                            .as_str()
                            .unwrap()
                            .to_owned();
                        assert_eq!(
                            request(&router, "DELETE", &member, None).await.status(),
                            StatusCode::NO_CONTENT
                        );
                    }
                    _ => unreachable!(),
                }
                // Removing the registry entry releases the unsent script, but the
                // held body keeps its admission slot.
                assert_eq!((state.stats().subscribers, state.stats().streams), (0, 1));
                assert_eq!(
                    request(&router, "GET", SSE, None).await.status(),
                    StatusCode::SERVICE_UNAVAILABLE
                );
                drop(response);
                assert_eq!(state.stats().streams, 0);
                assert_eq!(
                    request(&router, "GET", SSE, None).await.status(),
                    StatusCode::OK
                );
                Ok::<_, std::convert::Infallible>(())
            },
        )
        .await;
    }

    #[tokio::test]
    async fn stale_cursor_precedes_capacity_and_replay_leaves_scripts_queued() {
        let (router, bmc) = router_with(limits(1, 4096, 2, 8192));
        let state = bmc.event_service.as_ref().unwrap();
        let old_id = state.publish(event()).unwrap();
        let held = request(&router, "GET", SSE, None).await;
        bmc.reset();
        assert_eq!(
            resume(&router, &old_id).await.status(),
            StatusCode::BAD_REQUEST,
            "a stale cursor is reported even while the slot is held"
        );
        let id = state.publish(event()).unwrap();
        assert_eq!(
            resume(&router, &id).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        state.queue_script(vec![StreamStep::Eof]).unwrap();
        assert_eq!(
            request(&router, "GET", SSE, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            state.stats().queued_scripts,
            1,
            "rejected opens claim nothing"
        );
        drop(held);
        let replay = resume(&router, &id).await;
        assert_eq!(replay.status(), StatusCode::OK);
        assert_eq!(
            state.stats().queued_scripts,
            1,
            "replay never claims a script"
        );
        drop(replay);
        let response = request(&router, "GET", SSE, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state.stats().queued_scripts,
            0,
            "the next live-only open claims it"
        );
    }

    #[tokio::test]
    async fn ipmi_cold_reset_closes_stream_and_invalidates_replay() {
        let (router, bmc) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "ipmi-reset".into(),
            false,
            MachineRouterOptions {
                bmc_reset_duration: Some(Duration::from_millis(100)),
                ..Default::default()
            },
        );
        let state = bmc.event_service.as_ref().unwrap();
        let old_id = state.publish(event()).unwrap();
        let old_generation = state.stats().generation;
        let mut body = request(&router, "GET", SSE, None).await.into_body();
        state.queue_script(vec![StreamStep::Eof]).unwrap();
        let response = request(
            &router,
            "POST",
            "/ipmi",
            Some(json!({"action":"bmc_cold_reset"})),
        )
        .await;
        assert_eq!(json_body(response).await["success"], true);
        assert!(body.frame().await.is_none());
        let stats = state.stats();
        assert_ne!(stats.generation, old_generation);
        assert_eq!(
            (
                stats.subscribers,
                stats.streams,
                stats.retained_frames,
                stats.queued_scripts
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(
            request(&router, "GET", SSE, None).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            resume(&router, &old_id).await.status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            request(&router, "GET", SSE, None).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn http2_stalled_reader_cannot_bypass_admission_after_close() {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        tokio::time::timeout(Duration::from_secs(5), async {
            let (router, bmc) = router_with(limits(1, 4096, 2, 8192));
            let state = bmc.event_service.as_ref().unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let socket = tokio::net::TcpStream::connect(address).await.unwrap();
            let (mut client, connection) =
                hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                    .initial_stream_window_size(1024)
                    .handshake::<_, Body>(TokioIo::new(socket))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            let open = || {
                Request::builder()
                    .uri(format!("http://{address}{SSE}"))
                    .body(Body::empty())
                    .unwrap()
            };
            state
                .queue_script(vec![
                    StreamStep::Bytes {
                        data: vec![b'x'; 16 * 1024]
                    };
                    64
                ])
                .unwrap();
            let mut stalled = client.send_request(open()).await.unwrap();
            assert_eq!(stalled.status(), StatusCode::OK);
            // One prefix proves the server handed a frame to Hyper. The rest of
            // that frame exceeds the receive window, so Hyper cannot poll the next.
            let prefix = stalled
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            assert!(prefix.len() <= 1024);
            state.close_subscribers();
            assert_eq!((state.stats().subscribers, state.stats().streams), (0, 1));
            for _ in 0..32 {
                assert_eq!(
                    client.send_request(open()).await.unwrap().status(),
                    StatusCode::SERVICE_UNAVAILABLE
                );
                state.close_subscribers();
            }
            drop(stalled);
            while state.stats().streams != 0 {
                tokio::task::yield_now().await;
            }
            assert_eq!(
                client.send_request(open()).await.unwrap().status(),
                StatusCode::OK
            );
            drop(client);
            connection.abort();
            server.abort();
        })
        .await
        .expect("stalled HTTP/2 admission test must finish promptly");
    }

    #[tokio::test]
    async fn router_controls_discovery_subscription_lifetime_and_isolation() {
        let (router, bmc) = router(false);
        let events = bmc.event_service.as_ref().unwrap();
        bmc.injection.put(vec![crate::injection::Rule {
            id: "sse-json-rule".into(),
            selector: crate::injection::Selector::OdataId(SSE.into()),
            action: crate::injection::Action::JsonMerge(json!({"must_not_apply": true})),
            remaining: Some(1),
        }]);
        let (other_router, other) = self::router(false);
        assert_eq!(
            json_body(request(&router, "GET", "/redfish/v1", None).await).await["EventService"]["@odata.id"],
            ROOT
        );
        let response = request(&router, "GET", SSE, None).await;
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        assert_eq!(response.headers()["cache-control"], "no-cache");
        assert!(!response.headers().contains_key("content-length"));
        let mut body = response.into_body();
        let id =
            json_body(request(&router, "POST", "/Mock/EventService/events", Some(event())).await)
                .await["id"]
                .as_str()
                .unwrap()
                .to_string();
        assert!(
            frame(&mut body)
                .await
                .starts_with(format!("id: {id}\n").as_bytes())
        );
        assert_eq!(bmc.injection.list()[0].remaining, Some(1));
        assert_eq!(
            other
                .event_service
                .as_ref()
                .unwrap()
                .stats()
                .retained_frames,
            0
        );
        assert_eq!(
            request(&router, "GET", "/redfish/v1/Systems", None)
                .await
                .status(),
            StatusCode::OK
        );
        let subscriptions = json_body(request(&router, "GET", SUBSCRIPTIONS, None).await).await;
        assert_eq!(subscriptions["Members@odata.count"], 1);
        let path = subscriptions["Members"][0]["@odata.id"].as_str().unwrap();
        assert_eq!(
            json_body(request(&router, "GET", path, None).await).await["SubscriptionType"],
            "SSE"
        );
        assert_eq!(
            request(&router, "DELETE", path, None).await.status(),
            StatusCode::NO_CONTENT
        );
        assert!(body.frame().await.is_none());
        assert_eq!(
            request(&router, "GET", path, None).await.status(),
            StatusCode::NOT_FOUND
        );
        let mut body = request(&router, "GET", SSE, None).await.into_body();
        let _other_body = request(&other_router, "GET", SSE, None).await.into_body();
        drop(router);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), body.frame())
                .await
                .unwrap()
                .is_none(),
            "router removal must close its streams"
        );
        assert_eq!(events.stats().subscribers, 0);
        assert_eq!(other.event_service.as_ref().unwrap().stats().subscribers, 1);
    }

    #[tokio::test]
    async fn admission_failures_leave_scripts_unclaimed_and_replay_preserves_ids() {
        let (router, bmc) = router(false);
        let state = bmc.event_service.as_ref().unwrap();
        assert_eq!(
            request(
                &router,
                "POST",
                "/Mock/EventService/scripts",
                Some(json!([{"kind": "eof"}]))
            )
            .await
            .status(),
            StatusCode::NO_CONTENT
        );
        let head = request(&router, "HEAD", SSE, None).await;
        assert_eq!(head.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(
            !head.headers().contains_key("content-type"),
            "HEAD errors stay bodiless"
        );
        assert_eq!(
            state.stats().queued_scripts,
            1,
            "HEAD must not claim an SSE script"
        );
        check_cases_async(
            [
                Case {
                    scenario: "unsupported query",
                    input: (format!("{SSE}?$expand=*"), "text/event-stream"),
                    expect: Yields(StatusCode::BAD_REQUEST),
                },
                Case {
                    scenario: "incompatible media type",
                    input: (SSE.into(), "application/json"),
                    expect: Yields(StatusCode::NOT_ACCEPTABLE),
                },
                Case {
                    scenario: "explicit exclusion overrides wildcard",
                    input: (SSE.into(), "text/event-stream;q=0, */*;q=1"),
                    expect: Yields(StatusCode::NOT_ACCEPTABLE),
                },
            ],
            |(uri, accept)| {
                let router = router.clone();
                async move {
                    let response = router
                        .oneshot(
                            Request::builder()
                                .uri(uri)
                                .header("accept", accept)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    let status = response.status();
                    assert_eq!(response.headers()["content-type"], "application/json");
                    assert!(json_body(response).await["error"]["code"].is_string());
                    assert_eq!(state.stats().queued_scripts, 1);
                    assert_eq!(state.stats().subscribers, 0);
                    Ok::<_, std::convert::Infallible>(status)
                }
            },
        )
        .await;
        let mut empty = request(&router, "GET", SSE, None).await.into_body();
        assert!(empty.frame().await.is_none());
        let first = state.publish(event()).unwrap();
        let second = state.publish(metric()).unwrap();
        let response = resume(&router, &first).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            frame(&mut response.into_body())
                .await
                .starts_with(format!("id: {second}\n").as_bytes())
        );
    }

    #[tokio::test]
    async fn https_typed_stream_auth_reset_and_shutdown() {
        use futures::StreamExt;
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc, HttpClient};

        tokio::time::timeout(Duration::from_secs(10), async {
            let (router, bmc) = router(true);
            bmc.account_service_state
                .change_factory_default_password("test-password");
            let state = bmc.event_service.as_ref().unwrap();
            let (mut server, base) = serve_https("sse-https-test", router);
            let params = || ClientParams {
                accept_invalid_certs: true,
                timeout: Some(Duration::from_secs(2)),
                ..Default::default()
            };
            let client = Client::with_params(params()).unwrap();
            let credentials = BmcCredentials::new("root".into(), "test-password".into());
            let headers = HeaderMap::new();
            state.queue_script(vec![StreamStep::Eof]).unwrap();
            let wrong = BmcCredentials::new("root".into(), "wrong".into());
            assert!(
                client
                    .sse::<Value>(base.join(SSE).unwrap(), &wrong, &headers)
                    .await
                    .is_err()
            );
            assert!(
                client
                    .post::<_, Value>(
                        base.join("/Mock/EventService/events").unwrap(),
                        &event(),
                        &wrong,
                        &headers
                    )
                    .await
                    .is_err()
            );
            assert_eq!(state.stats().queued_scripts, 1);
            let mut eof = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers)
                .await
                .unwrap();
            assert!(eof.next().await.is_none());

            let root = nv_redfish::ServiceRoot::new(Arc::new(HttpBmc::new(
                Client::with_params(params()).unwrap(),
                base.clone(),
                credentials.clone(),
                CacheSettings::with_capacity(32),
            )))
            .await
            .unwrap();
            let service = root
                .event_service()
                .await
                .unwrap()
                .expect("discover enabled EventService");
            let mut stream = service.events().await.unwrap();
            assert_eq!(state.stats().subscribers, 1);
            state.publish(event()).unwrap();
            state.publish(metric()).unwrap();
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                EventStreamPayload::Event(_)
            ));
            assert!(matches!(
                stream.next().await.unwrap().unwrap(),
                EventStreamPayload::MetricReport(_)
            ));
            let _: Value = client
                .get(
                    base.join("/redfish/v1/Systems").unwrap(),
                    &credentials,
                    None,
                    &headers,
                )
                .await
                .unwrap();

            let session = client
                .post_session::<_, Value>(
                    base.join("/redfish/v1/SessionService/Sessions").unwrap(),
                    &json!({"UserName": "root", "Password": "test-password"}),
                    &headers,
                )
                .await
                .unwrap();
            let token = BmcCredentials::token(session.auth_token);
            let mut session_stream = client
                .sse::<Value>(base.join(SSE).unwrap(), &token, &headers)
                .await
                .unwrap();
            let _ = client
                .delete::<Value>(
                    base.join(&session.location.to_string()).unwrap(),
                    &credentials,
                    &headers,
                )
                .await
                .unwrap();
            assert!(
                client
                    .sse::<Value>(base.join(SSE).unwrap(), &token, &headers)
                    .await
                    .is_err()
            );
            state.publish(event()).unwrap();
            assert_eq!(
                session_stream.next().await.unwrap().unwrap(),
                event(),
                "existing stream is authenticated at open"
            );
            drop(session_stream);
            // Wait for the server to observe cancellation, without assuming a fixed scheduling delay.
            while state.stats().subscribers != 1 {
                tokio::task::yield_now().await;
            }
            bmc.reset();
            // A frame sent before reset may already be in the transport buffer.
            while let Some(item) = stream.next().await {
                item.unwrap();
            }
            assert_eq!(state.stats().subscribers, 0);
            assert_eq!(state.stats().retained_frames, 0);
            while bmc.availability.as_ref().unwrap().is_offline() {
                tokio::task::yield_now().await;
            }
            let mut stream = service.events().await.unwrap();
            server.stop().await.unwrap();
            match stream.next().await {
                None | Some(Err(_)) => {}
                Some(Ok(_)) => panic!("shutdown must terminate the stream"),
            }
            while state.stats().subscribers != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("HTTPS stream lifecycle must complete promptly");
    }

    #[tokio::test]
    async fn https_fault_script_exercises_real_parser() {
        use futures::StreamExt;
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, HttpClient};

        tokio::time::timeout(Duration::from_secs(5), async {
            let (router, bmc) = router(false);
            let state = bmc.event_service.as_ref().unwrap();
            let (mut server, base) = serve_https("sse-parser-test", router);
            let client = Client::with_params(ClientParams {
                accept_invalid_certs: true,
                ..Default::default()
            })
            .unwrap();
            let credentials = BmcCredentials::new("root".into(), "unused".into());
            let headers = HeaderMap::new();
            state
                .queue_script(vec![
                    StreamStep::Bytes {
                        data: b": comment\r\ndata: {\r\ndata: \"text\":\"caf\xc3".to_vec(),
                    },
                    StreamStep::Delay { millis: 1 },
                    StreamStep::Bytes {
                        data: b"\xa9\"}\r\n\r\n".to_vec(),
                    },
                    StreamStep::Delay { millis: 1 },
                    StreamStep::Bytes {
                        data: b"data: invalid-json\n\n".to_vec(),
                    },
                    StreamStep::Eof,
                ])
                .unwrap();
            let mut stream = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers)
                .await
                .unwrap();
            assert_eq!(
                stream.next().await.unwrap().unwrap(),
                json!({"text": "café"})
            );
            assert!(stream.next().await.unwrap().is_err());
            drop(stream);
            server.stop().await.unwrap();
            assert_eq!(
                state.stats().retained_frames,
                0,
                "raw scripts never enter replay"
            );
        })
        .await
        .expect("fault script must complete promptly");
    }

    #[tokio::test]
    async fn https_stalled_http2_transport_releases_admission_without_client_drop() {
        use hyper_util::rt::{TokioExecutor, TokioIo};
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
        use rustls::pki_types::ServerName;

        tokio::time::timeout(Duration::from_secs(5), async {
            let (router, bmc) = router_with(
                EventServiceConfig::try_from(EventServiceLimits {
                    max_subscribers: 1,
                    max_frame_bytes: 4096,
                    max_frames: 2,
                    max_history_bytes: 8192,
                    heartbeat: None,
                    output_stall_timeout: Duration::from_millis(500),
                })
                .unwrap(),
            );
            let state = bmc.event_service.as_ref().unwrap();
            let (mut server, base) = serve_https("sse-stall-test", router);
            let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(
                forge_tls::dummy_tls_verifier::DummyTlsVerifier::new_for_tests(),
            ))
            .with_no_client_auth();
            config.alpn_protocols = vec![b"h2".to_vec()];
            let socket = tokio::net::TcpStream::connect(server.address)
                .await
                .unwrap();
            let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(ServerName::try_from("localhost").unwrap(), socket)
                .await
                .unwrap();
            let (mut client, connection) =
                hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                    .initial_stream_window_size(1024)
                    .handshake::<_, Body>(TokioIo::new(tls))
                    .await
                    .unwrap();
            let connection = tokio::spawn(connection);
            state
                .queue_script(vec![
                    StreamStep::Bytes {
                        data: vec![b'x'; 16 * 1024]
                    };
                    64
                ])
                .unwrap();
            let url = base.join(SSE).unwrap();
            let mut stalled = client
                .send_request(
                    Request::builder()
                        .uri(url.as_str())
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(stalled.status(), StatusCode::OK);
            assert_eq!(state.stats().streams, 1);
            // One prefix proves the server handed a frame to Hyper; the rest of
            // that frame exceeds the receive window and is never read.
            let prefix = stalled
                .body_mut()
                .frame()
                .await
                .unwrap()
                .unwrap()
                .into_data()
                .unwrap();
            assert!(prefix.len() <= 1024);
            state.close_subscribers();
            // Only the server's output-stall timeout can release this slot: the
            // client still owns the unpolled response.
            while state.stats().streams != 0 {
                tokio::task::yield_now().await;
            }
            let fresh = Client::with_params(ClientParams {
                accept_invalid_certs: true,
                ..Default::default()
            })
            .unwrap();
            let reopened = fresh
                .sse::<Value>(
                    url,
                    &BmcCredentials::new("root".into(), "unused".into()),
                    &Default::default(),
                )
                .await
                .unwrap();
            assert_eq!(state.stats().streams, 1);
            drop(reopened);
            drop(stalled);
            connection.abort();
            server.stop().await.unwrap();
        })
        .await
        .expect("transport timeout must free the stalled subscriber slot");
    }

    #[tokio::test]
    async fn manager_reset_without_outage_still_closes_sse() {
        let (router, bmc) = machine_router(
            &host_info(HardwareType::DellPowerEdgeR750),
            Arc::new(NoopCallbacks),
            "reset-test".into(),
            false,
            MachineRouterOptions::default(),
        );
        let state = bmc.event_service.as_ref().unwrap();
        let entries = "/redfish/v1/Systems/System.Embedded.1/LogServices/EventLog/Entries";
        let before = json_body(request(&router, "GET", entries, None).await).await;
        let mut body = request(&router, "GET", SSE, None).await.into_body();
        state.publish(event()).unwrap();
        frame(&mut body).await;
        let reset = manager_reset_target(&router).await;
        assert_eq!(
            request(
                &router,
                "POST",
                &reset,
                Some(json!({"ResetType": "ForceRestart"}))
            )
            .await
            .status(),
            StatusCode::OK
        );
        // The reset is logged but not announced: the stream closes first.
        assert!(body.frame().await.is_none());
        assert_eq!(state.stats().retained_frames, 0);
        let after = json_body(request(&router, "GET", entries, None).await).await;
        assert_eq!(
            after["Members@odata.count"].as_u64().unwrap(),
            before["Members@odata.count"].as_u64().unwrap() + 1
        );
        let logged = after["Members"].as_array().unwrap().last().unwrap();
        assert_eq!(logged["Severity"], "Warning");
        assert!(
            logged["Message"].as_str().unwrap().contains("is resetting"),
            "{logged}"
        );
        assert_eq!(
            request(&router, "GET", ROOT, None).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn lifecycle_actions_record_log_entries_and_publish_events() {
        let (router, bmc) = router(false);
        let system = "/redfish/v1/Systems/System.Embedded.1";
        let entries = format!("{system}/LogServices/EventLog/Entries");
        async fn count(router: &Router, entries: &str) -> u64 {
            json_body(request(router, "GET", entries, None).await).await["Members@odata.count"]
                .as_u64()
                .unwrap()
        }
        let before = count(&router, &entries).await;
        let mut body = request(&router, "GET", SSE, None).await.into_body();

        // An accepted reset action is logged and announced with the LogEntry as origin.
        assert_eq!(
            request(
                &router,
                "POST",
                &format!("{system}/Actions/ComputerSystem.Reset"),
                Some(json!({"ResetType": "ForceRestart"}))
            )
            .await
            .status(),
            StatusCode::OK
        );
        let record = payload(&frame(&mut body).await)["Events"][0].clone();
        assert_eq!(
            record["MessageId"],
            "ResourceEvent.1.3.ResourceStateChanged"
        );
        assert_eq!(record["MessageSeverity"], "OK");
        let origin = record["OriginOfCondition"]["@odata.id"].as_str().unwrap();
        assert!(origin.starts_with(&format!("{entries}/")), "{origin}");
        let entry = json_body(request(&router, "GET", origin, None).await).await;
        assert_eq!(entry["Message"], record["Message"]);
        assert_eq!(entry["MessageId"], record["MessageId"]);
        assert_eq!(entry["Links"]["OriginOfCondition"]["@odata.id"], system);

        // Power events reported by the embedder flow the same way.
        bmc.on_event(&BmcEvent::PowerOn);
        assert_eq!(
            payload(&frame(&mut body).await)["Events"][0]["MessageId"],
            "ResourceEvent.1.3.ResourcePoweredOn"
        );
        assert_eq!(count(&router, &entries).await, before + 2);

        // A profile without a log service still publishes, pointing at the system.
        let (bare, bare_bmc) = machine_router(
            &host_info(HardwareType::GenericAmi),
            Arc::new(NoopCallbacks),
            "bare-log".into(),
            false,
            MachineRouterOptions::default(),
        );
        let mut bare_body = request(&bare, "GET", SSE, None).await.into_body();
        bare_bmc.on_event(&BmcEvent::BootCompleted);
        let origin =
            payload(&frame(&mut bare_body).await)["Events"][0]["OriginOfCondition"]["@odata.id"]
                .as_str()
                .unwrap()
                .to_owned();
        assert!(origin.starts_with("/redfish/v1/Systems/"), "{origin}");
        assert_eq!(
            request(&bare, "GET", &origin, None).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            request(&bare, "GET", &format!("{origin}/LogServices"), None)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn https_authority_removal_closes_only_its_bmc_stream() {
        use std::collections::HashMap;

        use futures::StreamExt;
        use nv_redfish::bmc_http::reqwest::{Client, ClientParams};
        use nv_redfish::bmc_http::{BmcCredentials, HttpClient};
        use tokio::sync::RwLock;

        tokio::time::timeout(Duration::from_secs(5), async {
            let (router_a, a) = router(false);
            let (router_b, b) = router(false);
            let routers = Arc::new(RwLock::new(HashMap::from([
                ("bmc-a".into(), router_a),
                ("bmc-b".into(), router_b),
            ])));
            let (mut server, base) = serve_https(
                "sse-authority-test",
                crate::combined_router(routers.clone()),
            );
            let client = Client::with_params(ClientParams {
                accept_invalid_certs: true,
                ..Default::default()
            })
            .unwrap();
            let credentials = BmcCredentials::new("root".into(), "unused".into());
            let mut headers_a = HeaderMap::new();
            headers_a.insert("forwarded", "host=bmc-a".parse().unwrap());
            let mut headers_b = HeaderMap::new();
            headers_b.insert("forwarded", "host=bmc-b".parse().unwrap());
            let mut stream_a = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers_a)
                .await
                .unwrap();
            let mut stream_b = client
                .sse::<Value>(base.join(SSE).unwrap(), &credentials, &headers_b)
                .await
                .unwrap();
            let _ = client
                .post::<_, Value>(
                    base.join("/Mock/EventService/events").unwrap(),
                    &event(),
                    &credentials,
                    &headers_a,
                )
                .await
                .unwrap();
            assert_eq!(stream_a.next().await.unwrap().unwrap(), event());
            let stats_b: Value = client
                .get(
                    base.join("/Mock/EventService/stats").unwrap(),
                    &credentials,
                    None,
                    &headers_b,
                )
                .await
                .unwrap();
            assert_eq!(stats_b["retained_frames"], 0);
            routers.write().await.remove("bmc-a");
            assert!(stream_a.next().await.is_none());
            assert_eq!(a.event_service.as_ref().unwrap().stats().subscribers, 0);
            assert_eq!(b.event_service.as_ref().unwrap().stats().subscribers, 1);
            b.event_service.as_ref().unwrap().publish(metric()).unwrap();
            assert_eq!(stream_b.next().await.unwrap().unwrap(), metric());
            let _ = client
                .post::<_, Value>(
                    base.join("/Mock/EventService/close").unwrap(),
                    &json!({}),
                    &credentials,
                    &headers_b,
                )
                .await
                .unwrap();
            assert!(stream_b.next().await.is_none());
            server.stop().await.unwrap();
        })
        .await
        .expect("authority removal must terminate only the selected BMC");
    }
}
