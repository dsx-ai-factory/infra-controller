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

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nv_redfish::core::{Bmc, EntityTypeRef, NavProperty};
use nv_redfish::event_service::{Event, EventStreamPayload};
use tokio::sync::Semaphore;

use super::diagnostic::{
    DiagnosticPayload, make_diagnostic_record, nullable_ref, nullable_str, redfish_enum_string,
};
use super::redfish::{
    RedfishLogFields, RedfishSeverity, add_redfish_analyzer_attributes, event_diagnostic_is_cper,
    nvidia_error_id, redfish_event_type_string, redfish_log_type,
};
use crate::HealthError;
use crate::collectors::inventory::{
    EntityInventory, GpuIdentity, SharedInventory, normalize_odata_id,
};
use crate::collectors::runtime::{
    EventStream, StreamingCollector, StreamingConnectResult, open_sse_stream,
};
use crate::endpoint::BmcEndpoint;
use crate::metrics::MetricLabel;
use crate::sink::{CollectorEvent, LogRecord};

const EVENT_RECORD_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_RECORD_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const EVENT_RECORD_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum EventRecordResolutionFailure {
    Fetch,
    Timeout,
}

#[derive(carbide_instrument::Event)]
#[event(
    event_name = "redfish_sse_event_record_resolution_failed",
    metric_name = "carbide_health_redfish_sse_event_record_resolution_failures_total",
    component = "nico-hardware-health",
    log = warn,
    metric = counter,
    message = "failed to resolve Redfish SSE event record",
    describe = "Number of Redfish SSE event records dropped after a referenced record could not be resolved, by failure reason."
)]
struct EventRecordResolutionFailed {
    #[label]
    reason: EventRecordResolutionFailure,
    #[context]
    odata_id: String,
    #[context]
    error: String,
}

/// Configuration for the Redfish SSE log collector.
pub struct SseLogCollectorConfig<B: Bmc> {
    /// Attach Redfish diagnostic payloads to emitted log records.
    pub include_diagnostics: bool,

    /// Bounds event-record resolution to the endpoint Redfish operation limit.
    pub request_concurrency: NonZeroUsize,

    /// Entity inventory used to resolve `origin_of_condition` to the GPU
    /// currently occupying that chassis slot. `None` disables enrichment.
    pub(crate) gpu_inventory: Option<SharedInventory<B>>,
}

pub struct SseLogCollector<B: Bmc> {
    bmc: Arc<B>,
    include_diagnostics: bool,
    request_concurrency: usize,
    gpu_inventory: Option<SharedInventory<B>>,
}

#[async_trait]
impl<B: Bmc + 'static> StreamingCollector<B> for SseLogCollector<B> {
    type Config = SseLogCollectorConfig<B>;

    fn new_runner(
        bmc: Arc<B>,
        _endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        Ok(Self {
            bmc,
            include_diagnostics: config.include_diagnostics,
            request_concurrency: config.request_concurrency.get(),
            gpu_inventory: config.gpu_inventory,
        })
    }

    async fn connect(&mut self) -> Result<StreamingConnectResult<'_>, HealthError> {
        let sse_stream = open_sse_stream(Arc::clone(&self.bmc)).await?;

        let event_stream = map_event_stream(
            sse_stream,
            Arc::clone(&self.bmc),
            self.include_diagnostics,
            self.request_concurrency,
            self.gpu_inventory.clone(),
        );

        Ok(StreamingConnectResult::Connected(event_stream))
    }

    fn collector_type(&self) -> &'static str {
        "sse_logs"
    }
}

fn map_event_stream<'a, B, S>(
    sse_stream: S,
    bmc: Arc<B>,
    include_diagnostics: bool,
    request_concurrency: usize,
    gpu_inventory: Option<SharedInventory<B>>,
) -> EventStream<'a>
where
    B: Bmc + 'static,
    S: futures::Stream<Item = Result<EventStreamPayload, HealthError>> + Send + 'a,
{
    let fetch_permits = Arc::new(Semaphore::new(request_concurrency));

    sse_stream
        .map(move |result| {
            let bmc = Arc::clone(&bmc);
            let fetch_permits = Arc::clone(&fetch_permits);
            let gpu_inventory = gpu_inventory.clone();
            async move {
                map_payload(
                    result,
                    bmc.as_ref(),
                    include_diagnostics,
                    fetch_permits.as_ref(),
                    gpu_inventory.as_ref(),
                )
                .await
            }
        })
        .buffered(request_concurrency)
        .flat_map(futures::stream::iter)
        .boxed()
}

async fn map_payload<B: Bmc>(
    result: Result<EventStreamPayload, HealthError>,
    bmc: &B,
    include_diagnostics: bool,
    fetch_permits: &Semaphore,
    gpu_inventory: Option<&SharedInventory<B>>,
) -> Vec<Result<CollectorEvent, HealthError>> {
    match result {
        Ok(EventStreamPayload::Event(event)) => {
            event_to_logs(
                &event,
                bmc,
                include_diagnostics,
                fetch_permits,
                EVENT_RECORD_RESOLUTION_TIMEOUT,
                gpu_inventory,
            )
            .await
        }
        Ok(EventStreamPayload::MetricReport(_)) => Vec::new(),
        Err(e) => vec![Err(e)],
    }
}

/// Converts one Redfish SSE event into collector log events.
async fn event_to_logs<B: Bmc>(
    event: &Event,
    bmc: &B,
    include_diagnostics: bool,
    fetch_permits: &Semaphore,
    resolution_timeout: Duration,
    gpu_inventory: Option<&SharedInventory<B>>,
) -> Vec<Result<CollectorEvent, HealthError>> {
    let deadline = tokio::time::Instant::now() + resolution_timeout;

    futures::future::join_all(
        event
            .events
            .iter()
            .map(|nav| resolve_event_record(nav, bmc, fetch_permits, deadline)),
    )
    .await
    .into_iter()
    .flatten()
    .map(|record| {
        let gpu = gpu_attributes_for_record(gpu_inventory, &record);
        Ok(record_to_log(&record, include_diagnostics, gpu))
    })
    .collect()
}

/// Resolves the GPU an event came from, against the current inventory snapshot.
///
/// A GPU event names its GPU by location rather than by identity, and the two
/// places it can do so need different lookups:
///
/// - `origin_of_condition` is a path (`/redfish/v1/Chassis/HGX_GPU_SXM_1`) that
///   survives a GPU swap while the silicon behind it changes.
/// - A driver event — the shape an Xid arrives in — reports the *baseboard* as
///   its origin and names the GPU only in its message arguments, so the origin
///   alone cannot identify it. See [`gpu_slot_from_message_args`].
///
/// Every step is best-effort. Non-GPU origins are common, and the snapshot is
/// absent until the first discovery pass completes; both degrade to an
/// unenriched record rather than a dropped one.
fn gpu_attributes_for_record<B: Bmc>(
    gpu_inventory: Option<&SharedInventory<B>>,
    record: &nv_redfish::schema::event::EventRecord,
) -> Vec<MetricLabel> {
    let Some(shared) = gpu_inventory else {
        return Vec::new();
    };
    let Some(snapshot) = shared.load_full() else {
        return Vec::new();
    };

    gpu_identity_by_origin(&snapshot, record)
        .or_else(|| gpu_identity_by_message_args(&snapshot, record))
        .map(|gpu| gpu.attributes())
        .unwrap_or_default()
}

/// GPU named by an event's `origin_of_condition`, when that path is a GPU.
fn gpu_identity_by_origin<'a, B: Bmc>(
    snapshot: &'a EntityInventory<B>,
    record: &nv_redfish::schema::event::EventRecord,
) -> Option<&'a GpuIdentity> {
    let odata_id = record.origin_of_condition.as_ref()?.odata_id.to_string();
    let origin_path = normalize_odata_id(&odata_id);

    snapshot
        .entities
        .iter()
        .find(|entity| entity.gpu_origin_path().as_deref() == Some(origin_path))
        .and_then(|entity| entity.gpu_identity())
}

/// GPU named in a driver event's message arguments.
///
/// Only consulted when the origin did not resolve, which is the case that
/// matters for Xid errors: they arrive with `MessageId`
/// `ResourceEvent.1.0.ResourceErrorsDetected` and the HGX baseboard as their
/// origin, so the GPU appears nowhere but the message.
fn gpu_identity_by_message_args<'a, B: Bmc>(
    snapshot: &'a EntityInventory<B>,
    record: &nv_redfish::schema::event::EventRecord,
) -> Option<&'a GpuIdentity> {
    let slot = gpu_slot_from_message_args(record.message_args.as_deref()?)?;

    snapshot
        .entities
        .iter()
        .filter(|entity| entity.gpu_identity().is_some())
        .find(|entity| entity.gpu_slot_id().as_deref() == Some(slot))
        .and_then(|entity| entity.gpu_identity())
}

/// The slot a driver event's first message argument names.
///
/// The argument reads `"GPU_SXM_1 Driver Event Message"` on HGX H100 and
/// `"GPU_1 Driver Event Message"` on GB200-class hardware, so the slot is its
/// leading word. Matching that word against a slot id by equality rather than
/// by substring is what keeps `GPU_SXM_1` from also matching `GPU_SXM_10`; the
/// caller additionally requires the match to be an entity discovery already
/// classified as a GPU, so a non-GPU subject resolves to nothing.
fn gpu_slot_from_message_args(args: &[String]) -> Option<&str> {
    args.first()?.split_whitespace().next()
}

async fn resolve_event_record<B: Bmc>(
    nav: &nv_redfish::core::NavProperty<nv_redfish::schema::event::EventRecord>,
    bmc: &B,
    fetch_permits: &Semaphore,
    deadline: tokio::time::Instant,
) -> Option<Arc<nv_redfish::schema::event::EventRecord>> {
    let odata_id = nav.odata_id().to_string();
    let is_reference = matches!(nav, NavProperty::Reference(_));
    let mut retry_backoff = EVENT_RECORD_RETRY_INITIAL_BACKOFF;

    loop {
        let get_record = async {
            let _permit = if is_reference {
                Some(
                    fetch_permits
                        .acquire()
                        .await
                        .expect("event record fetch semaphore remains open"),
                )
            } else {
                None
            };
            nav.get(bmc).await
        };

        match tokio::time::timeout_at(deadline, get_record).await {
            Ok(Ok(record)) => return Some(record),
            Ok(Err(error)) if is_reference => {
                let retry_at = (tokio::time::Instant::now() + retry_backoff).min(deadline);
                tokio::time::sleep_until(retry_at).await;
                if retry_at == deadline {
                    carbide_instrument::emit(EventRecordResolutionFailed {
                        reason: EventRecordResolutionFailure::Fetch,
                        odata_id,
                        error: error.to_string(),
                    });
                    return None;
                }
                retry_backoff = (retry_backoff * 2).min(EVENT_RECORD_RETRY_MAX_BACKOFF);
            }
            Ok(Err(error)) => {
                carbide_instrument::emit(EventRecordResolutionFailed {
                    reason: EventRecordResolutionFailure::Fetch,
                    odata_id,
                    error: error.to_string(),
                });
                return None;
            }
            Err(error) => {
                carbide_instrument::emit(EventRecordResolutionFailed {
                    reason: EventRecordResolutionFailure::Timeout,
                    odata_id,
                    error: error.to_string(),
                });
                return None;
            }
        }
    }
}

fn record_to_log(
    record: &nv_redfish::schema::event::EventRecord,
    include_diagnostics: bool,
    gpu_attributes: Vec<MetricLabel>,
) -> CollectorEvent {
    let diagnostic_data_type =
        nullable_ref(&record.diagnostic_data_type).and_then(redfish_enum_string);
    let redfish_fields = RedfishLogFields {
        message: record.message.as_deref(),
        message_args: record.message_args.as_deref(),
        has_cper: record.cper.is_some()
            || nullable_ref(&record.diagnostic_data_type).is_some_and(event_diagnostic_is_cper),
    };
    let log_type = redfish_log_type(redfish_fields);
    let body = record.message.as_deref().unwrap_or_default().to_string();

    // Fallback for a missing or unreadable Severity
    let severity = record
        .message_severity
        .as_ref()
        .and_then(RedfishSeverity::from_health)
        .or_else(|| record.severity.as_deref().map(RedfishSeverity::from_raw))
        .unwrap_or(RedfishSeverity::Unknown);

    // Reuse the same Redfish log-entry reference for the parent log attribute
    // and the diagnostic correlation attribute.
    let log_entry_id = record
        .log_entry
        .as_ref()
        .map(|log_entry_ref| log_entry_ref.odata_id().to_string());

    let mut attributes = vec![
        (Cow::Borrowed("message_id"), record.message_id.clone()),
        (
            Cow::Borrowed("event_record_id"),
            record.base.odata_id().to_string(),
        ),
    ];

    if let Some(event_type) = redfish_event_type_string(Some(&record.event_type)) {
        attributes.push((Cow::Borrowed("event_type"), event_type));
    }
    add_redfish_analyzer_attributes(
        &mut attributes,
        log_type,
        severity,
        nvidia_error_id(record.base.base.oem.as_ref()),
    );
    if let Some(event_id) = &record.event_id {
        attributes.push((Cow::Borrowed("event_id"), event_id.clone()));
    }
    if let Some(timestamp) = &record.event_timestamp {
        attributes.push((Cow::Borrowed("event_timestamp"), timestamp.to_string()));
    }
    if let Some(args) = &record.message_args {
        attributes.push((
            Cow::Borrowed("message_args"),
            serde_json::to_string(args).unwrap_or_default(),
        ));
    }
    if let Some(message_severity) = record
        .message_severity
        .as_ref()
        .and_then(RedfishSeverity::from_health)
    {
        attributes.push((
            Cow::Borrowed("message_severity"),
            message_severity.as_str().to_string(),
        ));
    }
    if let Some(origin) = &record.origin_of_condition {
        attributes.push((
            Cow::Borrowed("origin_of_condition"),
            origin.odata_id.to_string(),
        ));
    }
    attributes.extend(gpu_attributes);
    if let Some(log_entry_id) = &log_entry_id {
        attributes.push((Cow::Borrowed("log_entry_id"), log_entry_id.clone()));
    }
    if let Some(group_id) = record.event_group_id {
        attributes.push((Cow::Borrowed("event_group_id"), group_id.to_string()));
    }
    if let Some(resolution) = &record.resolution {
        attributes.push((Cow::Borrowed("resolution"), resolution.clone()));
    }
    if let Some(oem) = &record.base.base.oem {
        attributes.push((
            Cow::Borrowed("redfish.oem"),
            oem.additional_properties.to_string(),
        ));
    }

    let diagnostic_record = if include_diagnostics {
        make_diagnostic_record(DiagnosticPayload {
            diagnostic_data: nullable_str(&record.diagnostic_data),
            diagnostic_data_type,
            oem_diagnostic_data_type: nullable_str(&record.oem_diagnostic_data_type),
            additional_data_uri: nullable_str(&record.additional_data_uri),
            additional_data_size_bytes: nullable_ref(&record.additional_data_size_bytes).copied(),
            message_id: Some(record.message_id.as_str()),
            event_id: record.event_id.as_deref(),
            log_entry_id: log_entry_id.as_deref(),
        })
    } else {
        None
    };

    CollectorEvent::Log(Box::new(LogRecord {
        body,
        severity: severity.into(),
        attributes,
        diagnostic_record,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::{Json, Router};
    use bmc_mock::test_support::axum_http_client::AxumRouterHttpClient;
    use futures::FutureExt;
    use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
    use serde_json::{Value, json};
    use url::Url;

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::sink::LogSeverity;

    async fn event_to_logs_with_timeout<B: Bmc>(
        event: &Event,
        bmc: &B,
        include_diagnostics: bool,
        resolution_timeout: Duration,
    ) -> Vec<Result<CollectorEvent, HealthError>> {
        let fetch_permits = Semaphore::new(event.events.len().max(1));
        event_to_logs(
            event,
            bmc,
            include_diagnostics,
            &fetch_permits,
            resolution_timeout,
            None,
        )
        .await
    }

    #[tokio::test]
    async fn sse_event_preserves_oem_extensions() -> Result<(), HealthError> {
        let payload = serde_json::from_value(serde_json::json!({
            "@odata.id": "/redfish/v1/EventService/SSE#/Event1",
            "@odata.type": "#Event.v1_0_0.Event",
            "Id": "1",
            "Name": "Event Array",
            "Events": [
                {
                    "@odata.id": "/redfish/v1/EventService/SSE#/Events/1",
                    "MemberId": "1",
                    "EventType": "Alert",
                    "MessageId": "Example.1.0.Event",
                    "Oem": {
                        "Nvidia": {
                            "ErrorId": "example-error-id"
                        }
                    }
                }
            ]
        }))?;

        let endpoint = test_endpoint(mac("00:11:22:33:44:55"));

        let fetch_permits = Semaphore::new(1);
        let events = map_payload(
            Ok(payload),
            endpoint.bmc().as_ref(),
            false,
            &fetch_permits,
            None,
        )
        .await;

        let [Ok(CollectorEvent::Log(record))] = events.as_slice() else {
            panic!("expected one SSE log record");
        };

        let oem = record
            .attributes
            .iter()
            .find_map(|(key, value)| (key.as_ref() == "redfish.oem").then_some(value));

        assert_eq!(
            oem.map(String::as_str),
            Some(r#"{"Nvidia":{"ErrorId":"example-error-id"}}"#)
        );

        Ok(())
    }

    type TestBmc = HttpBmc<AxumRouterHttpClient>;

    fn test_bmc(router: Router) -> TestBmc {
        HttpBmc::new(
            AxumRouterHttpClient::new(router),
            Url::parse("https://bmc-mock.local").expect("valid test URL"),
            BmcCredentials::new("root".to_string(), "password".to_string()),
            CacheSettings::with_capacity(8),
        )
    }

    fn referenced_event(paths: &[&str]) -> Event {
        serde_json::from_value(json!({
            "@odata.id": "/redfish/v1/EventService/Events/1",
            "Id": "1",
            "Name": "Test event",
            "Events": paths
                .iter()
                .map(|path| json!({"@odata.id": path}))
                .collect::<Vec<_>>(),
        }))
        .expect("valid referenced Redfish event")
    }

    fn c12_platform_record(path: &str) -> Value {
        json!({
            "@odata.id": path,
            "MemberId": "0",
            "EventType": "Alert",
            "MessageId": "IANA.0.1.CPLD-PSEQ-FAULT",
            "MessageSeverity": "Critical",
            "MessageArgs": ["CPLD_0", ""],
            "Message": "",
            "Oem": {
                "Nvidia": {
                    "ErrorId": "CPLD-PSEQ-FAULT"
                }
            },
            "OriginOfCondition": {
                "@odata.id": "/redfish/v1/Chassis/HGX_Baseboard_0"
            }
        })
    }

    fn log_record(event: &CollectorEvent) -> &LogRecord {
        let CollectorEvent::Log(record) = event else {
            panic!("expected log event");
        };
        record
    }

    fn attribute<'a>(record: &'a LogRecord, key: &str) -> Option<&'a str> {
        record
            .attributes
            .iter()
            .find(|(candidate, _)| candidate.as_ref() == key)
            .map(|(_, value)| value.as_str())
    }

    #[tokio::test]
    async fn referenced_event_record_is_awaited_and_emitted() {
        let path = "/redfish/v1/EventService/Events/records/1";
        let payload = c12_platform_record(path);
        let router = Router::new().route(
            path,
            get(move || {
                let payload = payload.clone();
                async move { Json(payload) }
            }),
        );
        let bmc = test_bmc(router);

        let logs = event_to_logs_with_timeout(
            &referenced_event(&[path]),
            &bmc,
            false,
            Duration::from_secs(1),
        )
        .await;

        assert_eq!(logs.len(), 1);
        let event = logs[0].as_ref().expect("resolved record should be emitted");
        let record = log_record(event);
        assert_eq!(record.body, "");
        assert_eq!(
            attribute(record, "oem.nvidia.error_id"),
            Some("CPLD-PSEQ-FAULT")
        );
        assert_eq!(
            attribute(record, "redfish.event.type"),
            Some("redfish_event")
        );
        assert_eq!(
            attribute(record, "redfish.event.severity"),
            Some("Critical")
        );

        assert_eq!(attribute(record, "event_type"), Some("Alert"));
        assert_eq!(attribute(record, "event_record_id"), Some(path));
    }

    #[tokio::test(start_paused = true)]
    async fn referenced_event_record_is_retried_until_available() {
        let path = "/redfish/v1/EventService/Events/records/delayed";
        let payload = c12_platform_record(path);
        let request_count = Arc::new(AtomicUsize::new(0));
        let handler_request_count = Arc::clone(&request_count);
        let router = Router::new().route(
            path,
            get(move || {
                let payload = payload.clone();
                let request_count = Arc::clone(&handler_request_count);
                async move {
                    if request_count.fetch_add(1, Ordering::SeqCst) == 0 {
                        StatusCode::NOT_FOUND.into_response()
                    } else {
                        Json(payload).into_response()
                    }
                }
            }),
        );
        let bmc = test_bmc(router);

        let logs = event_to_logs_with_timeout(
            &referenced_event(&[path]),
            &bmc,
            false,
            Duration::from_secs(1),
        )
        .await;

        assert_eq!(request_count.load(Ordering::SeqCst), 2);
        assert_eq!(logs.len(), 1);
        let event = logs[0]
            .as_ref()
            .expect("record should be emitted after retry");
        assert_eq!(
            attribute(log_record(event), "oem.nvidia.error_id"),
            Some("CPLD-PSEQ-FAULT")
        );
    }

    #[tokio::test]
    async fn failed_reference_does_not_drop_sibling_record() {
        let missing_path = "/redfish/v1/EventService/Events/records/missing";
        let good_path = "/redfish/v1/EventService/Events/records/good";
        let payload = c12_platform_record(good_path);
        let router = Router::new().route(
            good_path,
            get(move || {
                let payload = payload.clone();
                async move { Json(payload) }
            }),
        );
        let bmc = test_bmc(router);

        let logs = event_to_logs_with_timeout(
            &referenced_event(&[missing_path, good_path]),
            &bmc,
            false,
            Duration::from_secs(1),
        )
        .await;

        assert_eq!(logs.len(), 1);
        let event = logs[0].as_ref().expect("good sibling should be emitted");
        assert_eq!(
            attribute(log_record(event), "oem.nvidia.error_id"),
            Some("CPLD-PSEQ-FAULT")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn hung_reference_does_not_drop_ready_sibling_record() {
        let hung_path = "/redfish/v1/EventService/Events/records/hung";
        let good_path = "/redfish/v1/EventService/Events/records/good";
        let payload = c12_platform_record(good_path);
        let router = Router::new()
            .route(
                hung_path,
                get(|| async { std::future::pending::<Json<Value>>().await }),
            )
            .route(
                good_path,
                get(move || {
                    let payload = payload.clone();
                    async move { Json(payload) }
                }),
            );
        let bmc = test_bmc(router);
        let resolution_timeout = Duration::from_secs(1);
        let started_at = tokio::time::Instant::now();

        let logs = event_to_logs_with_timeout(
            &referenced_event(&[hung_path, good_path]),
            &bmc,
            false,
            resolution_timeout,
        )
        .await;

        assert_eq!(tokio::time::Instant::now() - started_at, resolution_timeout);
        assert_eq!(logs.len(), 1);
        let event = logs[0].as_ref().expect("ready sibling should be emitted");
        assert_eq!(
            attribute(log_record(event), "oem.nvidia.error_id"),
            Some("CPLD-PSEQ-FAULT")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn request_concurrency_limits_event_record_gets() {
        let first_path = "/redfish/v1/EventService/Events/records/hung";
        let second_path = "/redfish/v1/EventService/Events/records/waiting";
        let first_request_started = Arc::new(tokio::sync::Notify::new());
        let second_request_started = Arc::new(tokio::sync::Notify::new());
        let first_notification = Arc::clone(&first_request_started);
        let second_notification = Arc::clone(&second_request_started);
        let payload = c12_platform_record(second_path);
        let router = Router::new()
            .route(
                first_path,
                get(move || {
                    let first_notification = Arc::clone(&first_notification);
                    async move {
                        first_notification.notify_one();
                        std::future::pending::<Json<Value>>().await
                    }
                }),
            )
            .route(
                second_path,
                get(move || {
                    let second_notification = Arc::clone(&second_notification);
                    let payload = payload.clone();
                    async move {
                        second_notification.notify_one();
                        Json(payload)
                    }
                }),
            );
        let bmc = test_bmc(router);
        let event = referenced_event(&[first_path, second_path]);
        let fetch_permits = Arc::new(Semaphore::new(1));
        let task_permits = Arc::clone(&fetch_permits);

        let task = tokio::spawn(async move {
            event_to_logs(
                &event,
                &bmc,
                false,
                task_permits.as_ref(),
                Duration::from_secs(1),
                None,
            )
            .await
        });

        first_request_started.notified().await;
        assert_eq!(fetch_permits.available_permits(), 0);
        assert!(second_request_started.notified().now_or_never().is_none());

        task.await.expect("event resolution task should complete");
    }

    #[tokio::test(start_paused = true)]
    async fn referenced_event_record_batch_fetch_is_bounded() {
        let first_path = "/redfish/v1/EventService/Events/records/hung-1";
        let second_path = "/redfish/v1/EventService/Events/records/hung-2";
        let router = Router::new()
            .route(
                first_path,
                get(|| async { std::future::pending::<Json<Value>>().await }),
            )
            .route(
                second_path,
                get(|| async { std::future::pending::<Json<Value>>().await }),
            );
        let bmc = test_bmc(router);
        let started_at = tokio::time::Instant::now();

        let logs = event_to_logs_with_timeout(
            &referenced_event(&[first_path, second_path]),
            &bmc,
            false,
            EVENT_RECORD_RESOLUTION_TIMEOUT,
        )
        .await;

        assert!(logs.is_empty());
        assert_eq!(EVENT_RECORD_RESOLUTION_TIMEOUT, Duration::from_secs(10));
        assert_eq!(
            tokio::time::Instant::now() - started_at,
            EVENT_RECORD_RESOLUTION_TIMEOUT
        );
    }

    #[tokio::test(start_paused = true)]
    async fn slow_payload_does_not_block_sse_stream_polling() {
        let hung_path = "/redfish/v1/EventService/Events/records/hung";
        let good_path = "/redfish/v1/EventService/Events/records/good";
        let good_request_started = Arc::new(tokio::sync::Notify::new());
        let request_notification = Arc::clone(&good_request_started);
        let payload = c12_platform_record(good_path);
        let router = Router::new()
            .route(
                hung_path,
                get(|| async { std::future::pending::<Json<Value>>().await }),
            )
            .route(
                good_path,
                get(move || {
                    let request_notification = Arc::clone(&request_notification);
                    let payload = payload.clone();
                    async move {
                        request_notification.notify_one();
                        Json(payload)
                    }
                }),
            );
        let bmc = Arc::new(test_bmc(router));
        let sse_stream = futures::stream::iter([
            Ok(EventStreamPayload::Event(referenced_event(&[hung_path]))),
            Ok(EventStreamPayload::Event(referenced_event(&[good_path]))),
        ]);
        let mut event_stream = map_event_stream(sse_stream, bmc, false, 2, None);
        let next_event = tokio::spawn(async move { event_stream.next().await });

        tokio::time::timeout(Duration::from_secs(1), good_request_started.notified())
            .await
            .expect("later SSE payload should be polled before the first payload times out");

        let event = next_event
            .await
            .expect("event stream task should complete")
            .expect("ready payload should emit an event")
            .expect("ready payload should emit a log");
        assert_eq!(
            attribute(log_record(&event), "oem.nvidia.error_id"),
            Some("CPLD-PSEQ-FAULT")
        );
    }

    #[test]
    fn diagnostic_payload_remains_behind_sink_gate() {
        let record: nv_redfish::schema::event::EventRecord = serde_json::from_value(json!({
            "@odata.id": "/redfish/v1/EventService/Events/records/cper",
            "MemberId": "0",
            "EventType": "Alert",
            "MessageId": "ResourceEvent.1.0.ResourceErrorsDetected",
            "Message": "PCIe error",
            "MessageSeverity": "Critical",
            "DiagnosticData": "base64-cper-payload",
            "DiagnosticDataType": "CPER",
            "CPER": {}
        }))
        .expect("valid CPER event record");

        let without_diagnostics = record_to_log(&record, false, Vec::new());
        let without_diagnostics = log_record(&without_diagnostics);
        assert_eq!(without_diagnostics.body, "PCIe error");
        assert!(without_diagnostics.diagnostic_record.is_none());

        let with_diagnostics = record_to_log(&record, true, Vec::new());
        let with_diagnostics = log_record(&with_diagnostics);
        assert_eq!(with_diagnostics.body, "PCIe error");
        assert!(with_diagnostics.diagnostic_record.is_some());
        assert_eq!(
            with_diagnostics.emitted_log_record(false).body,
            "PCIe error"
        );

        let emitted = with_diagnostics.emitted_log_record(true);
        let body: Value = serde_json::from_str(&emitted.body).expect("diagnostic body is JSON");
        assert_eq!(body["message"], "PCIe error");
        assert_eq!(body["diagnostic_data"], "base64-cper-payload");
    }

    fn severity_record(
        message_severity: Option<&str>,
        severity: Option<&str>,
    ) -> nv_redfish::schema::event::EventRecord {
        let mut value = json!({
            "@odata.id": "/redfish/v1/EventService/Events/records/1",
            "MemberId": "0",
            "EventType": "Alert",
            "MessageId": "Example.1.0.Event",
        });
        if let Some(message_severity) = message_severity {
            value["MessageSeverity"] = json!(message_severity);
        }
        if let Some(severity) = severity {
            value["Severity"] = json!(severity);
        }
        serde_json::from_value(value).expect("valid event record")
    }

    /// Exercises the severity chain through `record_to_log`: the schema field
    /// wins, a value the schema could not parse falls back to the raw
    /// `Severity` string, and `message_severity` is emitted only when the
    /// schema field parsed.
    #[test]
    fn severity_resolution_chain() {
        let event = record_to_log(
            &severity_record(Some("Critical"), Some("WARNING")),
            false,
            Vec::new(),
        );
        let record = log_record(&event);
        assert_eq!(record.severity, LogSeverity::Fatal);
        assert_eq!(
            attribute(record, "redfish.event.severity"),
            Some("Critical")
        );
        assert_eq!(attribute(record, "message_severity"), Some("Critical"));

        // "CRITICAL" is outside the schema, so MessageSeverity lands on
        // UnsupportedValue and the raw Severity string carries the value.
        let event = record_to_log(
            &severity_record(Some("CRITICAL"), Some("Critical")),
            false,
            Vec::new(),
        );
        let record = log_record(&event);
        assert_eq!(record.severity, LogSeverity::Fatal);
        assert_eq!(
            attribute(record, "redfish.event.severity"),
            Some("Critical")
        );
        assert_eq!(attribute(record, "message_severity"), None);

        let event = record_to_log(&severity_record(None, None), false, Vec::new());
        let record = log_record(&event);
        assert_eq!(record.severity, LogSeverity::Unspecified);
        assert_eq!(attribute(record, "redfish.event.severity"), Some("Unknown"));
        assert_eq!(attribute(record, "message_severity"), None);

        let event = record_to_log(&severity_record(Some("Meltdown"), None), false, Vec::new());
        let record = log_record(&event);
        assert_eq!(record.severity, LogSeverity::Unspecified);
        assert_eq!(attribute(record, "redfish.event.severity"), Some("Unknown"));
        assert_eq!(attribute(record, "message_severity"), None);
    }
}

/// Verifies the join that gives an SSE log record its GPU identity: discovery
/// records what occupies each chassis slot, and a log record naming that slot
/// in `OriginOfCondition` is attributed to the device found there.
#[cfg(test)]
mod gpu_enrichment_tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Instant;

    use arc_swap::ArcSwapOption;
    use bmc_mock::test_support::{TestBmc, nvidia_dgx_h100_bmc};
    use serde_json::json;

    use super::{gpu_attributes_for_record, gpu_slot_from_message_args};
    use crate::collectors::discovery::{
        gpu_identity_from_chassis, gpu_identity_from_processor, gpu_processor_ids,
    };
    use crate::collectors::inventory::{DiscoveredEntity, EntityInventory, SharedInventory};

    /// Build the inventory snapshot the way the discovery collector does —
    /// processors first, then chassis matched against them — so the join is
    /// exercised against identities the mock BMC actually serves.
    async fn h100_inventory() -> SharedInventory<TestBmc> {
        let h = nvidia_dgx_h100_bmc().await;

        let mut entities = Vec::new();

        let systems = h
            .service_root
            .systems()
            .await
            .expect("systems collection")
            .expect("systems collection is present");
        for system in systems.members().await.expect("system members") {
            let system = Arc::new(system);
            for processor in system
                .processors()
                .await
                .expect("processors")
                .unwrap_or_default()
            {
                let gpu = gpu_identity_from_processor::<TestBmc>(&processor);
                entities.push(DiscoveredEntity::Processor {
                    entity: Arc::new(processor),
                    system: system.clone(),
                    sensors: Vec::new(),
                    gpu,
                });
            }
        }

        let gpu_processors = gpu_processor_ids(&entities);

        let chassis_list = h
            .service_root
            .chassis()
            .await
            .expect("chassis collection")
            .expect("chassis collection is present");
        for chassis in chassis_list.members().await.expect("chassis members") {
            let gpu = gpu_identity_from_chassis::<TestBmc>(&chassis, &gpu_processors);
            entities.push(DiscoveredEntity::Chassis {
                entity: Arc::new(chassis),
                sensors: Vec::new(),
                gpu,
            });
        }

        Arc::new(ArcSwapOption::from_pointee(EntityInventory {
            entities,
            discovered_at: Instant::now(),
            generation: 1,
        }))
    }

    fn xid_record(origin: Option<&str>) -> nv_redfish::schema::event::EventRecord {
        let mut value = json!({
            "@odata.id": "/redfish/v1/EventService/SSE#/Events/1",
            "MemberId": "1",
            "EventType": "Alert",
            "MessageId": "Nvidia.1.0.XidError",
            "Message": "Xid 79: GPU has fallen off the bus",
            "MessageSeverity": "Critical",
        });
        if let Some(origin) = origin {
            value["OriginOfCondition"] = json!({ "@odata.id": origin });
        }
        serde_json::from_value(value).expect("valid event record")
    }

    fn attributes_of(
        inventory: Option<&SharedInventory<TestBmc>>,
        origin: Option<&str>,
    ) -> HashMap<String, String> {
        gpu_attributes_for_record(inventory, &xid_record(origin))
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    /// A driver event in the shape HGX firmware reports an Xid: the baseboard
    /// as origin, with the GPU named only in the message arguments.
    fn driver_event_record(origin: &str, args: &[&str]) -> nv_redfish::schema::event::EventRecord {
        serde_json::from_value(json!({
            "@odata.id": "/redfish/v1/EventService/SSE#/Events/1",
            "MemberId": "1",
            "EventType": "Alert",
            "MessageId": "ResourceEvent.1.0.ResourceErrorsDetected",
            "Message": "The resource property has detected errors.",
            "MessageArgs": args,
            "MessageSeverity": "Critical",
            "OriginOfCondition": { "@odata.id": origin },
        }))
        .expect("valid event record")
    }

    fn attributes_of_driver_event(
        inventory: &SharedInventory<TestBmc>,
        origin: &str,
        args: &[&str],
    ) -> HashMap<String, String> {
        gpu_attributes_for_record(Some(inventory), &driver_event_record(origin, args))
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    #[tokio::test]
    async fn gpu_origin_resolves_uuid_serial_and_model() {
        let inventory = h100_inventory().await;

        let attributes = attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_3"));

        assert_eq!(
            attributes.get("gpu_model").map(String::as_str),
            Some("H100 80GB HBM3")
        );
        for key in ["gpu_uuid", "gpu_serial", "gpu_chassis_serial"] {
            assert!(
                attributes.contains_key(key),
                "expected a {key} attribute, got {attributes:?}"
            );
        }
    }

    /// A GPU event can name the GPU's processor rather than its chassis, so both
    /// origin shapes must resolve to the same device.
    #[tokio::test]
    async fn processor_origin_resolves_the_same_gpu_as_its_chassis() {
        let inventory = h100_inventory().await;

        let via_processor = attributes_of(
            Some(&inventory),
            Some("/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_SXM_3"),
        );
        let via_chassis =
            attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_3"));

        assert!(
            via_processor.contains_key("gpu_uuid"),
            "got {via_processor:?}"
        );
        assert_eq!(via_processor.get("gpu_uuid"), via_chassis.get("gpu_uuid"));
        assert_eq!(
            via_processor.get("gpu_serial"),
            via_chassis.get("gpu_serial")
        );
    }

    /// The lookup key is the whole `@odata.id` rather than its last segment. A
    /// bare id would let a chassis origin resolve to a processor on a platform
    /// that reuses one id across the two collections.
    #[tokio::test]
    async fn gpu_origin_keys_are_whole_odata_ids() {
        let inventory = h100_inventory().await;
        let snapshot = inventory.load_full().expect("inventory snapshot");

        let keys: Vec<String> = snapshot
            .entities
            .iter()
            .filter(|entity| entity.gpu_identity().is_some())
            .filter_map(|entity| entity.gpu_origin_path())
            .collect();

        for expected in [
            "/redfish/v1/Chassis/HGX_GPU_SXM_1",
            "/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_SXM_1",
        ] {
            assert!(
                keys.iter().any(|key| key == expected),
                "expected {expected} among {keys:?}"
            );
        }
    }

    /// Each origin shape must resolve to its own variant. Only the chassis
    /// reports the enclosing module's serial, so resolving a chassis origin to a
    /// processor would silently drop `gpu_chassis_serial`.
    #[tokio::test]
    async fn each_origin_shape_resolves_its_own_variant() {
        let inventory = h100_inventory().await;

        let via_chassis =
            attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_3"));
        let via_processor = attributes_of(
            Some(&inventory),
            Some("/redfish/v1/Systems/HGX_Baseboard_0/Processors/GPU_SXM_3"),
        );

        assert!(
            via_chassis.contains_key("gpu_chassis_serial"),
            "a chassis origin must resolve the chassis, got {via_chassis:?}"
        );
        assert!(
            !via_processor.contains_key("gpu_chassis_serial"),
            "a processor origin must resolve the processor, got {via_processor:?}"
        );
    }

    /// Two slots must not be attributed to the same physical GPU, which is the
    /// whole reason the label carries a UUID rather than the slot name.
    #[tokio::test]
    async fn distinct_slots_resolve_distinct_uuids() {
        let inventory = h100_inventory().await;

        let first = attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_1"));
        let second = attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_2"));

        assert_ne!(first.get("gpu_uuid"), second.get("gpu_uuid"));
        assert!(first.contains_key("gpu_uuid"));
    }

    #[tokio::test]
    async fn trailing_slash_in_origin_still_resolves() {
        let inventory = h100_inventory().await;

        let attributes =
            attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_4/"));

        assert!(attributes.contains_key("gpu_uuid"), "got {attributes:?}");
    }

    #[tokio::test]
    async fn non_gpu_origin_yields_no_attributes() {
        let inventory = h100_inventory().await;

        assert!(
            attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_Chassis_0")).is_empty()
        );
        assert!(
            attributes_of(Some(&inventory), Some("/redfish/v1/Systems/System_0")).is_empty(),
            "an origin outside the chassis collection must not match a chassis id"
        );
    }

    /// The case the origin lookup alone cannot serve: an Xid names the HGX
    /// baseboard as its origin, which holds eight GPUs, so the identity has to
    /// come from the GPU named in the message arguments.
    #[tokio::test]
    async fn baseboard_origin_xid_resolves_the_gpu_named_in_message_args() {
        let inventory = h100_inventory().await;

        let from_xid = attributes_of_driver_event(
            &inventory,
            "/redfish/v1/Systems/HGX_Baseboard_0",
            &[
                "GPU_SXM_1 Driver Event Message",
                "[Tue Apr 22 23:22:23 UTC 2025][7][00] XID 95 Uncontained: FBHUB. RST: Yes; D-RST: No",
            ],
        );

        assert!(
            from_xid.contains_key("gpu_uuid"),
            "an Xid must resolve its GPU, got {from_xid:?}"
        );

        // Must be the same device the slot's own chassis origin resolves to.
        let from_chassis =
            attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_1"));
        assert_eq!(from_xid.get("gpu_uuid"), from_chassis.get("gpu_uuid"));
        assert_eq!(from_xid.get("gpu_serial"), from_chassis.get("gpu_serial"));
    }

    /// Two Xids from different slots must not collapse onto one GPU.
    #[tokio::test]
    async fn xids_from_different_slots_resolve_different_gpus() {
        let inventory = h100_inventory().await;
        let origin = "/redfish/v1/Systems/HGX_Baseboard_0";

        let first =
            attributes_of_driver_event(&inventory, origin, &["GPU_SXM_1 Driver Event Message", ""]);
        let second =
            attributes_of_driver_event(&inventory, origin, &["GPU_SXM_2 Driver Event Message", ""]);

        assert!(first.contains_key("gpu_uuid"));
        assert_ne!(first.get("gpu_uuid"), second.get("gpu_uuid"));
    }

    /// A resolvable origin is the better signal and must not be overridden by
    /// whatever the message text happens to name.
    #[tokio::test]
    async fn origin_takes_precedence_over_message_args() {
        let inventory = h100_inventory().await;

        let resolved = attributes_of_driver_event(
            &inventory,
            "/redfish/v1/Chassis/HGX_GPU_SXM_3",
            &["GPU_SXM_1 Driver Event Message", ""],
        );
        let slot_three = attributes_of(Some(&inventory), Some("/redfish/v1/Chassis/HGX_GPU_SXM_3"));

        assert_eq!(resolved.get("gpu_uuid"), slot_three.get("gpu_uuid"));
    }

    /// The fallback must not label events whose subject is not a discovered GPU,
    /// which is why the slot is compared by equality against inventory rather
    /// than by searching the message for a `GPU` substring.
    #[tokio::test]
    async fn message_args_naming_no_discovered_gpu_yield_no_attributes() {
        let inventory = h100_inventory().await;
        let origin = "/redfish/v1/Systems/HGX_Baseboard_0";

        for args in [
            // A sensor threshold event: names a GPU sensor, not a GPU.
            vec!["HGX_GPU_0_Temp_1", "3.96", "-0.05"],
            // A slot that does not exist on this baseboard.
            vec!["GPU_SXM_99 Driver Event Message", ""],
            // The root-of-trust component, which reports its own unrelated UUID.
            vec!["HGX_ERoT_GPU_SXM_1 Driver Event Message", ""],
            vec![],
        ] {
            let attributes = attributes_of_driver_event(&inventory, origin, &args);
            assert!(
                attributes.is_empty(),
                "args {args:?} must not resolve a GPU, got {attributes:?}"
            );
        }
    }

    /// The slot is the leading word, and is matched whole: a prefix test would
    /// let `GPU_SXM_1` also claim events belonging to `GPU_SXM_10`.
    #[test]
    fn slot_is_the_leading_word_of_the_first_argument() {
        let cases = [
            ("GPU_SXM_1 Driver Event Message", Some("GPU_SXM_1")),
            ("GPU_1 Driver Event Message", Some("GPU_1")),
            ("GPU_SXM_10 Driver Event Message", Some("GPU_SXM_10")),
        ];

        for (arg, expected) in cases {
            let args = vec![arg.to_string()];
            assert_eq!(gpu_slot_from_message_args(&args), expected, "{arg}");
        }

        assert_eq!(gpu_slot_from_message_args(&[]), None);
    }

    #[tokio::test]
    async fn records_without_origin_or_inventory_are_left_unenriched() {
        let inventory = h100_inventory().await;

        assert!(attributes_of(Some(&inventory), None).is_empty());
        assert!(attributes_of(None, Some("/redfish/v1/Chassis/HGX_GPU_SXM_1")).is_empty());

        // Before the first discovery pass completes the snapshot is empty.
        let empty: SharedInventory<TestBmc> = Arc::new(ArcSwapOption::empty());
        assert!(attributes_of(Some(&empty), Some("/redfish/v1/Chassis/HGX_GPU_SXM_1")).is_empty());
    }
}
