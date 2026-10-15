/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use carbide_host_support::agent_config::AgentConfig;
use carbide_instrument::testing::{capture_logs, capture_logs_async};
use rpc::forge;
use rpc::forge_tls_client::ForgeClientConfig;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

use super::common;
use crate::periodic_config_fetcher::{PeriodicConfigFetcher, PeriodicConfigFetcherConfig};

const FETCH_INTERVAL: Duration = Duration::from_millis(100);
const SLOW_RESPONSE: Duration = Duration::from_millis(300);
const HOST_INTERFACE_ID: &str = "f377ed72-d912-4879-958a-8d1f82a50d62";
const HOST_ID: &str = "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg";

struct FetchState {
    requests: mpsc::Sender<Instant>,
    request_count: AtomicUsize,
    failed_requests: Vec<usize>,
    invalid_metadata_requests: Vec<usize>,
    third_response_gate: Option<Arc<Notify>>,
}

#[test]
fn config_fetch_waits_after_success_failure_and_slow_response() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut failure_gap = None;
    // The thread-scoped capture includes background tasks polled by this
    // runtime, without changing the process-wide tracing subscriber.
    let logs = capture_logs(|| {
        failure_gap = Some(runtime.block_on(async {
            let (requests, mut request_times) = mpsc::channel(4);
            let third_response_gate = Arc::new(Notify::new());
            let state = Arc::new(FetchState {
                requests,
                request_count: AtomicUsize::new(0),
                failed_requests: vec![2],
                invalid_metadata_requests: vec![],
                third_response_gate: Some(third_response_gate.clone()),
            });
            let (address, server) = run_server(state).await;
            let fetcher = PeriodicConfigFetcher::new(fetcher_config(address, FETCH_INTERVAL)).await;
            let reader = fetcher.reader();
            assert_eq!(
                reader.net_conf_read().unwrap().managed_host_config_version,
                "initial"
            );
            let first = request_times
                .try_recv()
                .expect("initial fetch completed before new returned");

            // The next RPC fails. It still waits after startup and leaves the last
            // successful configuration available until a later fetch recovers.
            let second = next_request(&mut request_times).await;
            assert!(second - first >= FETCH_INTERVAL);
            let third = next_request(&mut request_times).await;
            assert!(third - second >= FETCH_INTERVAL);
            // Request three cannot begin until the failure was processed. Hold its
            // response so it cannot replace either cached value before this check.
            assert_eq!(
                reader.net_conf_read().unwrap().managed_host_config_version,
                "initial"
            );
            assert_eq!(
                reader
                    .meta_data_conf_reader()
                    .unwrap()
                    .instance_name
                    .as_deref(),
                Some("initial")
            );
            third_response_gate.notify_one();

            // This response takes longer than the interval. Wait again after it
            // completes instead of issuing a request to catch up with missed ticks.
            let fourth = next_request(&mut request_times).await;
            assert!(fourth - third >= SLOW_RESPONSE + FETCH_INTERVAL);
            assert_eq!(
                reader.net_conf_read().unwrap().managed_host_config_version,
                "recovered"
            );
            assert_eq!(
                reader
                    .meta_data_conf_reader()
                    .unwrap()
                    .instance_name
                    .as_deref(),
                Some("recovered")
            );

            drop(fetcher);
            assert!(
                tokio::time::timeout(FETCH_INTERVAL * 3, request_times.recv())
                    .await
                    .is_err(),
                "dropping the fetcher must stop recurring requests"
            );
            server.abort();
            server.await.expect_err("mock server was aborted");
            third - second
        }));
    });
    let failed = logs
        .iter()
        .find(|log| log.field("event_name") == Some("dpu_agent_config_fetch_failed"))
        .expect("the injected fetch failure was recorded");
    let delay: f64 = failed
        .field("retry_interval_seconds")
        .unwrap()
        .parse()
        .unwrap();
    assert!((0.1..=0.11).contains(&delay), "sampled retry wait: {delay}");
    assert!(failure_gap.unwrap().as_secs_f64() >= delay);
}

#[tokio::test]
async fn dropping_config_fetcher_releases_state_before_next_interval() {
    let (requests, _request_times) = mpsc::channel(4);
    let state = Arc::new(FetchState {
        requests,
        request_count: AtomicUsize::new(0),
        failed_requests: vec![],
        invalid_metadata_requests: vec![],
        third_response_gate: None,
    });
    let (address, server) = run_server(state).await;
    let config = fetcher_config(address, Duration::from_secs(30));
    let client_config = Arc::downgrade(&config.forge_client_config);
    let fetcher = PeriodicConfigFetcher::new(config).await;
    // Allow the background task to enter its first wait. Keep no reader, which
    // would independently retain the shared state after the owner is dropped.
    tokio::task::yield_now().await;
    drop(fetcher);

    tokio::time::timeout(Duration::from_secs(5), async {
        while client_config.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping the fetcher must release its state without waiting for the interval");
    server.abort();
    server.await.expect_err("mock server was aborted");
}

#[tokio::test]
async fn failed_initial_fetch_retries_before_host_id_lookup_expires() {
    let (requests, _request_times) = mpsc::channel(4);
    let state = Arc::new(FetchState {
        requests,
        request_count: AtomicUsize::new(0),
        failed_requests: vec![1],
        invalid_metadata_requests: vec![],
        third_response_gate: None,
    });
    let (address, server) = run_server(state.clone()).await;
    let config = fetcher_config(address, Duration::from_secs(30));
    let client_config = config.forge_client_config.clone();
    let forge_api = config.forge_api.clone();
    let agent_config = AgentConfig::default();
    assert!(agent_config.machine.interface_id.is_none());
    let (fetcher, logs) = tokio::time::timeout(
        Duration::from_secs(5),
        capture_logs_async(PeriodicConfigFetcher::new(config)),
    )
    .await
    .expect("startup must not wait for the 30-second polling interval");
    let host_id = tokio::time::timeout(
        Duration::from_secs(5),
        crate::host_machine_id::get_host_machine_id_retry(
            &agent_config,
            &fetcher,
            client_config,
            &forge_api,
        ),
    )
    .await
    .expect("host lookup must not wait for the polling interval")
    .expect("the immediate background retry supplies the host interface");
    assert_eq!(host_id.to_string(), HOST_ID);
    assert_eq!(state.request_count.load(Ordering::Relaxed), 2);
    let failed = logs
        .iter()
        .find(|log| log.field("event_name") == Some("dpu_agent_config_fetch_failed"))
        .expect("initial failure was recorded");
    assert_eq!(failed.field("retry_interval_seconds"), Some("0"));
    drop(fetcher);
    server.abort();
    server.await.expect_err("mock server was aborted");
}

#[tokio::test]
async fn failed_startup_retry_returns_to_periodic_waits() {
    let (requests, mut request_times) = mpsc::channel(4);
    let state = Arc::new(FetchState {
        requests,
        request_count: AtomicUsize::new(0),
        failed_requests: vec![1, 2],
        invalid_metadata_requests: vec![],
        third_response_gate: None,
    });
    let (address, server) = run_server(state).await;
    let fetcher = PeriodicConfigFetcher::new(fetcher_config(address, FETCH_INTERVAL)).await;
    request_times.try_recv().expect("initial request completed");
    let retry = next_request(&mut request_times).await;
    let recurring = next_request(&mut request_times).await;
    assert!(
        recurring - retry >= FETCH_INTERVAL,
        "only the initial failure gets an immediate retry"
    );
    drop(fetcher);
    server.abort();
    server.await.expect_err("mock server was aborted");
}

#[test]
fn failed_initial_metadata_conversion_retries_once_before_periodic_waits() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut failure_gap = None;
    let logs = capture_logs(|| {
        failure_gap = Some(runtime.block_on(async {
            let (requests, mut request_times) = mpsc::channel(4);
            let third_response_gate = Arc::new(Notify::new());
            let state = Arc::new(FetchState {
                requests,
                request_count: AtomicUsize::new(0),
                failed_requests: vec![],
                invalid_metadata_requests: vec![1, 2],
                third_response_gate: Some(third_response_gate.clone()),
            });
            let (address, server) = run_server(state).await;
            let fetcher = PeriodicConfigFetcher::new(fetcher_config(address, FETCH_INTERVAL)).await;
            let reader = fetcher.reader();
            request_times.try_recv().expect("initial request completed");
            let retry = next_request(&mut request_times).await;
            let recurring = next_request(&mut request_times).await;
            assert!(recurring - retry >= FETCH_INTERVAL);

            // Hold recovery until the second conversion failure has been observed.
            // Network configuration remains available even when metadata is invalid.
            assert_eq!(
                reader.net_conf_read().unwrap().managed_host_config_version,
                "recovered"
            );
            assert!(reader.meta_data_conf_reader().is_none());
            third_response_gate.notify_one();
            next_request(&mut request_times).await;
            assert_eq!(
                reader
                    .meta_data_conf_reader()
                    .unwrap()
                    .instance_name
                    .as_deref(),
                Some("recovered")
            );

            drop(fetcher);
            server.abort();
            server.await.expect_err("mock server was aborted");
            recurring - retry
        }));
    });
    let failures = logs
        .iter()
        .filter(|log| log.field("event_name") == Some("dpu_agent_config_fetch_failed"))
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2);
    assert!(failures.iter().all(|log| {
        log.field("error")
            .unwrap()
            .contains("failed to parse instance config_version")
    }));
    assert_eq!(failures[0].field("retry_interval_seconds"), Some("0"));
    let delay: f64 = failures[1]
        .field("retry_interval_seconds")
        .unwrap()
        .parse()
        .unwrap();
    assert!((0.1..=0.11).contains(&delay), "sampled retry wait: {delay}");
    assert!(failure_gap.unwrap().as_secs_f64() >= delay);
}

async fn next_request(requests: &mut mpsc::Receiver<Instant>) -> Instant {
    tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .expect("config fetch did not resume")
        .expect("request observer closed")
}

async fn run_server(state: Arc<FetchState>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new()
        .route("/up", get(|| async { "OK" }))
        .route(
            rpc::service_path!("Version"),
            post(|| async { common::respond(forge::BuildInfo::default()) }),
        )
        .route(
            rpc::service_path!("GetManagedHostNetworkConfig"),
            post(fetch_config),
        )
        .route(
            rpc::service_path!("FindInterfaces"),
            post(|| async {
                common::respond(forge::InterfaceList {
                    interfaces: vec![forge::MachineInterface {
                        id: Some(HOST_INTERFACE_ID.parse().unwrap()),
                        machine_id: Some(HOST_ID.parse().unwrap()),
                        ..Default::default()
                    }],
                })
            }),
        )
        .with_state(state);
    common::run_grpc_server(app).await.unwrap()
}

fn fetcher_config(address: SocketAddr, interval: Duration) -> PeriodicConfigFetcherConfig {
    PeriodicConfigFetcherConfig {
        config_fetch_interval: interval,
        machine_id: "fm100dsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g"
            .parse()
            .unwrap(),
        forge_api: format!("https://{address}"),
        forge_client_config: Arc::new(ForgeClientConfig {
            root_ca_path: concat!(env!("CARGO_MANIFEST_DIR"), "/test-certs/tls.crt").into(),
            ..Default::default()
        }),
    }
}

async fn fetch_config(State(state): State<Arc<FetchState>>) -> axum::response::Response {
    let count = state.request_count.fetch_add(1, Ordering::Relaxed) + 1;
    state.requests.send(Instant::now()).await.unwrap();
    if state.failed_requests.contains(&count) {
        return (
            [
                ("content-type", "application/grpc"),
                ("grpc-status", "14"),
                ("grpc-message", "injected%20configuration%20failure"),
            ],
            "",
        )
            .into_response();
    }
    if count == 3
        && let Some(gate) = &state.third_response_gate
    {
        gate.notified().await;
        tokio::time::sleep(SLOW_RESPONSE).await;
    }
    let name = if count == 1 { "initial" } else { "recovered" };
    common::respond(forge::ManagedHostNetworkConfigResponse {
        managed_host_config_version: name.into(),
        host_interface_id: Some(HOST_INTERFACE_ID.into()),
        instance: Some(rpc::Instance {
            id: Some("9afaedd3-b36e-4603-a029-8b94a82b89a0".parse().unwrap()),
            metadata: Some(rpc::Metadata {
                name: name.into(),
                ..Default::default()
            }),
            config_version: if state.invalid_metadata_requests.contains(&count) {
                "invalid"
            } else {
                "V1-T1748645613333257"
            }
            .into(),
            network_config_version: "V1-T1748645613333257".into(),
            dpu_extension_service_version: "V1-T1748645613333257".into(),
            ..Default::default()
        }),
        ..Default::default()
    })
    .into_response()
}
