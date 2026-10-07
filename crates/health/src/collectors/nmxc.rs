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
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use nv_redfish::core::Bmc;
use rpc::protos::nmx_c::nmx_controller_client::NmxControllerClient;
use rpc::protos::nmx_c::server_notification::Notification;
use rpc::protos::nmx_c::{
    ClientHello, Context, ControlPlaneState, DomainStateInfo, FmEvent, NmxControllerHealth,
    PartitionId, ProtoMsgMajorVersion, ProtoMsgMinorVersion, ServerHeader, ServerNotification,
    StReturnCode, SubscribeRequest, SubscriptionResponse, fm_event,
};
use tonic::Streaming;
use tonic::transport::{Channel, Endpoint};

use crate::HealthError;
use crate::collectors::runtime::{StreamingCollector, StreamingConnectResult};
use crate::config::{MtlsProfileConfig, NmxcCollectorConfig as NmxcCollectorOptions};
use crate::endpoint::BmcEndpoint;
use crate::sink::{CollectorEvent, LogRecord, LogSeverity};

const NMX_C_HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(300);
const NMX_C_HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(30);

type NmxcClient = NmxControllerClient<Channel>;
type NmxcNotificationStream = Streaming<ServerNotification>;

/// Runtime configuration passed from discovery into an NMX-C collector task.
pub struct NmxcCollectorConfig {
    /// User-facing collector settings from the health service configuration.
    pub nmxc_config: NmxcCollectorOptions,

    /// mTLS profile used for the gRPC channel when configured.
    pub(crate) tls_config: Option<MtlsProfileConfig>,
}

/// Streaming collector for NMX-C server notifications.
pub struct NmxcCollector {
    endpoint_url: String,
    gateway_id: String,
    notify_on_self_change: bool,
    heartbeat_rate: u32,
    connect_timeout: Duration,
    rpc_timeout: Duration,

    // NMX-C subscriptions are long-lived. Rotated mTLS profile files are picked
    // up when the stream reconnects and builds a new channel.
    tls_config: Option<MtlsProfileConfig>,
}

#[async_trait]
impl<B: Bmc + 'static> StreamingCollector<B> for NmxcCollector {
    type Config = NmxcCollectorConfig;

    /// Builds an NMX-C runner for the switch-host endpoint selected by discovery.
    fn new_runner(
        _bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let nmxc_config = config.nmxc_config;
        let connect_timeout = nmxc_config.connect_timeout();
        let rpc_timeout = nmxc_config.rpc_timeout();
        let switch_connect_host = endpoint.switch_connect_host_for_uri();

        let endpoint_url = nmxc_endpoint_url(
            switch_connect_host.as_ref(),
            nmxc_config.grpc_port,
            config.tls_config.is_some(),
        );

        Ok(Self {
            endpoint_url,
            gateway_id: nmxc_config.gateway_id,
            notify_on_self_change: nmxc_config.notify_on_self_change,
            heartbeat_rate: nmxc_config.heartbeat_rate,
            connect_timeout,
            rpc_timeout,
            tls_config: config.tls_config,
        })
    }

    /// Connects to NMX-C, completes Hello and Subscribe, and maps notifications into collector events.
    async fn connect(&mut self) -> Result<StreamingConnectResult<'_>, HealthError> {
        let mut client = nmxc_client(
            &self.endpoint_url,
            self.connect_timeout,
            self.tls_config.as_ref(),
        )
        .await?;

        send_hello(&mut client, &self.gateway_id, self.rpc_timeout).await?;

        let subscribe_request = SubscribeRequest {
            gateway_id: self.gateway_id.clone(),
            notify_on_self_change: self.notify_on_self_change,
            heart_beat_rate: self.heartbeat_rate,
        };

        let mut stream = subscribe(&mut client, subscribe_request, self.rpc_timeout).await?;
        let first_notification =
            receive_initial_subscribe_notification(&mut stream, self.rpc_timeout).await?;

        let mut accepted_events = Vec::new();

        for event in initial_subscribe_notification_to_events(first_notification) {
            match event {
                Ok(event) => accepted_events.push(event),
                Err(error) => {
                    return Ok(StreamingConnectResult::Failed {
                        events: accepted_events,
                        error,
                    });
                }
            }
        }

        Ok(StreamingConnectResult::Connected(
            futures::stream::iter(accepted_events.into_iter().map(Ok))
                .chain(
                    stream
                        .map(notification_to_events)
                        .flat_map(futures::stream::iter),
                )
                .boxed(),
        ))
    }

    /// Returns the stable collector type label used by the streaming runtime.
    fn collector_type(&self) -> &'static str {
        "nmxc"
    }
}

/// Sends the NMX-C Hello RPC and verifies the server accepted the connection.
async fn send_hello(
    client: &mut NmxcClient,
    gateway_id: &str,
    rpc_timeout: Duration,
) -> Result<(), HealthError> {
    let response = tokio::time::timeout(
        rpc_timeout,
        client.hello(tonic::Request::new(hello_request(gateway_id))),
    )
    .await
    .map_err(|_| nmxc_timeout_error("Hello", rpc_timeout))?
    .map_err(HealthError::NmxcStatus)?
    .into_inner();

    check_server_header_success(response.server_header.as_ref(), "Hello")
}

/// Starts the NMX-C Subscribe RPC and returns the streaming response.
async fn subscribe(
    client: &mut NmxcClient,
    request: SubscribeRequest,
    rpc_timeout: Duration,
) -> Result<NmxcNotificationStream, HealthError> {
    let response =
        tokio::time::timeout(rpc_timeout, client.subscribe(tonic::Request::new(request)))
            .await
            .map_err(|_| nmxc_timeout_error("Subscribe", rpc_timeout))?
            .map_err(HealthError::NmxcStatus)?;

    Ok(response.into_inner())
}

/// Reads the first Subscribe notification so rejected subscriptions fail before stream handoff.
async fn receive_initial_subscribe_notification(
    stream: &mut NmxcNotificationStream,
    rpc_timeout: Duration,
) -> Result<ServerNotification, HealthError> {
    // NMX-C confirms Subscribe on the response stream. Bound the first receive
    // so a switch that accepts TCP but wedges before acknowledgement still
    // returns to the streaming runtime's reconnect/backoff loop.
    tokio::time::timeout(rpc_timeout, stream.message())
        .await
        .map_err(|_| nmxc_timeout_error("Subscribe acknowledgement", rpc_timeout))?
        .map_err(HealthError::NmxcStatus)?
        .ok_or_else(|| {
            HealthError::NmxcStatus(tonic::Status::unavailable(
                "NMX-C subscribe stream ended before acknowledgement",
            ))
        })
}

/// Converts the first Subscribe item into pre-stream events and validation errors.
fn initial_subscribe_notification_to_events(
    notification: ServerNotification,
) -> Vec<Result<CollectorEvent, HealthError>> {
    match notification.notification {
        Some(Notification::SubscriptionResponse(response)) => {
            subscription_response_to_events(&response)
        }
        Some(notification) => vec![Err(HealthError::NmxcStatus(
            tonic::Status::failed_precondition(format!(
                "NMX-C Subscribe expected subscription_response acknowledgement, received {}",
                notification_name(&notification),
            )),
        ))],
        None => vec![Err(HealthError::NmxcStatus(
            tonic::Status::failed_precondition(
                "NMX-C Subscribe acknowledgement missing notification payload",
            ),
        ))],
    }
}

/// Builds the NMX-C Hello request using the gateway ID and generated proto version constants.
fn hello_request(gateway_id: &str) -> ClientHello {
    ClientHello {
        gateway_id: gateway_id.to_string(),
        major_version: ProtoMsgMajorVersion::ProtoMsgMajorVersion as i32,
        minor_version: ProtoMsgMinorVersion::ProtoMsgMinorVersion as i32,
    }
}

/// Builds the switch-host gRPC endpoint URL, including IPv6 bracket formatting.
pub(super) fn nmxc_endpoint_url(host: &str, port: u16, tls_enabled: bool) -> String {
    let scheme = if tls_enabled { "https" } else { "http" };
    format!("{scheme}://{host}:{port}")
}

/// Creates a tonic NMX-C channel with transport settings scoped to the collector.
///
/// When an mTLS profile is configured, certificate files are read while
/// building the channel, so reconnects pick up rotated material.
pub(super) async fn nmxc_channel(
    endpoint_url: &str,
    connect_timeout: Duration,
    tls_config: Option<&MtlsProfileConfig>,
) -> Result<Channel, HealthError> {
    let mut endpoint = Endpoint::from_shared(endpoint_url.to_string())
        .map_err(|error| nmxc_transport_error(endpoint_url, "parse endpoint", error))?
        .connect_timeout(connect_timeout)
        // Long-lived Subscribe streams need keepalive so dead peers eventually
        // surface as transport errors. Use a conservative interval because
        // NMX-C rejects aggressive clients with `Too many pings`.
        .http2_keep_alive_interval(NMX_C_HTTP2_KEEPALIVE_INTERVAL)
        .keep_alive_timeout(NMX_C_HTTP2_KEEPALIVE_TIMEOUT)
        .keep_alive_while_idle(true);

    if let Some(config) = tls_config {
        let tls_config = crate::tls::tonic_tls_config(config).await?;
        endpoint = endpoint
            .tls_config(tls_config)
            .map_err(|error| nmxc_transport_error(endpoint_url, "configure TLS", error))?;
    }

    endpoint
        .connect()
        .await
        .map_err(|error| nmxc_transport_error(endpoint_url, "connect", error))
}

async fn nmxc_client(
    endpoint_url: &str,
    connect_timeout: Duration,
    tls_config: Option<&MtlsProfileConfig>,
) -> Result<NmxcClient, HealthError> {
    nmxc_channel(endpoint_url, connect_timeout, tls_config)
        .await
        .map(NmxControllerClient::new)
}

/// Converts tonic transport failures into collector status errors with endpoint context.
fn nmxc_transport_error(
    endpoint_url: &str,
    operation: &str,
    error: tonic::transport::Error,
) -> HealthError {
    HealthError::NmxcStatus(tonic::Status::unavailable(format!(
        "NMX-C {operation} failed for {endpoint_url}: {error}",
    )))
}

/// Creates a deadline-exceeded status for bounded NMX-C RPC phases.
pub(super) fn nmxc_timeout_error(operation: &str, timeout: Duration) -> HealthError {
    HealthError::NmxcStatus(tonic::Status::deadline_exceeded(format!(
        "NMX-C {operation} timed out after {timeout:?}"
    )))
}

/// Converts a streamed NMX-C notification item into zero or more collector events.
fn notification_to_events(
    item: Result<ServerNotification, tonic::Status>,
) -> Vec<Result<CollectorEvent, HealthError>> {
    let notification = match item {
        Ok(item) => item.notification,
        Err(status) => return vec![Err(HealthError::NmxcStatus(status))],
    };

    match notification {
        Some(Notification::SubscriptionResponse(response)) => {
            subscription_response_to_events(&response)
        }
        Some(Notification::DomainStateInfo(info)) => domain_state_info_to_events(&info),
        Some(notification) => vec![Ok(notification_to_log(&notification))],
        None => vec![Ok(log_record(
            LogSeverity::Warn,
            "NMX-C stream notification omitted its payload",
            vec![(Cow::Borrowed("notification"), "missing".to_string())],
        ))],
    }
}

/// Converts the Subscribe acknowledgement into logs and reconnect-driving errors.
///
/// `SubscriptionResponse` is not a health update. A rejected acknowledgement
/// means the stream is unusable, so the error is returned after logging the full
/// payload.
fn subscription_response_to_events(
    response: &SubscriptionResponse,
) -> Vec<Result<CollectorEvent, HealthError>> {
    if let Err(error) = check_server_header_success(response.server_header.as_ref(), "Subscribe") {
        // Keep the rejected acknowledgement visible in log sinks before
        // returning the stream error that drives runtime reconnect behavior.
        return vec![Ok(subscription_response_to_log(response)), Err(error)];
    }

    // Successful acknowledgements carry no health payload; keep them as logs so
    // operators can see the subscription was accepted without creating metrics
    // or health reports.
    vec![Ok(subscription_response_to_log(response))]
}

/// Validates an NMX-C server header and rejects missing, unknown, or failed return codes.
fn check_server_header_success(
    header: Option<&ServerHeader>,
    operation: &str,
) -> Result<(), HealthError> {
    let Some(header) = header else {
        return Err(HealthError::NmxcStatus(tonic::Status::failed_precondition(
            format!("NMX-C {operation} response missing server_header"),
        )));
    };

    let return_code = StReturnCode::try_from(header.return_code).map_err(|_| {
        HealthError::NmxcStatus(tonic::Status::failed_precondition(format!(
            "NMX-C {operation} returned unknown return code {}",
            header.return_code,
        )))
    })?;

    if return_code != StReturnCode::NmxStSuccess {
        return Err(HealthError::NmxcStatus(tonic::Status::failed_precondition(
            format!("NMX-C {operation} returned {}", return_code.as_str_name(),),
        )));
    }

    Ok(())
}

/// Builds a log event that preserves the NMX-C notification summary and full payload.
fn notification_to_log(notification: &Notification) -> CollectorEvent {
    let name = notification_name(notification);
    let mut attributes = vec![(Cow::Borrowed("notification"), name.to_string())];

    append_notification_attributes(notification, &mut attributes);
    append_notification_payload_attribute(notification, &mut attributes);

    let body = notification_log_body(name, &attributes);

    push_attribute(&mut attributes, "body", &body);

    log_record(LogSeverity::Info, body, attributes)
}

/// Builds a log event for Subscribe acknowledgements, including rejection details.
fn subscription_response_to_log(response: &SubscriptionResponse) -> CollectorEvent {
    let name = "subscription_response";
    let mut attributes = vec![(Cow::Borrowed("notification"), name.to_string())];

    append_server_header_attributes(response.server_header.as_ref(), &mut attributes);

    append_notification_payload_attribute(
        &Notification::SubscriptionResponse(response.clone()),
        &mut attributes,
    );

    let body = notification_log_body(name, &attributes);

    push_attribute(&mut attributes, "body", &body);

    let severity =
        if check_server_header_success(response.server_header.as_ref(), "Subscribe").is_ok() {
            LogSeverity::Info
        } else {
            LogSeverity::Error
        };

    log_record(severity, body, attributes)
}

/// Formats the human-readable NMX-C log body from structured attributes.
fn notification_log_body(name: &str, attributes: &[(Cow<'static, str>, String)]) -> String {
    let summary = attributes
        .iter()
        .filter(|(key, _)| key.as_ref() != "notification")
        .map(|(key, value)| format!("{}={}", key.as_ref(), value))
        .collect::<Vec<_>>()
        .join(" ");

    if summary.is_empty() {
        format!("NMX-C stream notification received: {name}")
    } else {
        format!("NMX-C stream notification received: {name} {summary}")
    }
}

/// Appends concise, queryable fields for every NMX-C notification variant.
fn append_notification_attributes(
    notification: &Notification,
    attributes: &mut Vec<(Cow<'static, str>, String)>,
) {
    // Keep this exhaustive over the ServerNotification.notification oneof in
    // nmx_c.proto. Similar-looking arms stay explicit because prost generates
    // distinct Rust message types for each response payload.
    match notification {
        Notification::SubscriptionResponse(response) => {
            append_server_header_attributes(response.server_header.as_ref(), attributes);
        }
        Notification::StaticConfigResponse(response) => {
            append_server_header_attributes(response.server_header.as_ref(), attributes);
        }
        Notification::CreatePartitionResponse(response) => {
            append_server_header_attributes(response.server_header.as_ref(), attributes);
            append_context_attribute(response.context.as_ref(), attributes);
            append_partition_id_attribute(response.partition_id.as_ref(), attributes);
        }
        Notification::DeletePartitionResponse(response) => {
            append_server_header_attributes(response.server_header.as_ref(), attributes);
            append_context_attribute(response.context.as_ref(), attributes);
            append_partition_id_attribute(response.partition_id.as_ref(), attributes);
        }
        Notification::UpdatePartitionResponse(response) => {
            append_server_header_attributes(response.server_header.as_ref(), attributes);
            append_context_attribute(response.context.as_ref(), attributes);
            append_partition_id_attribute(response.partition_id.as_ref(), attributes);
        }
        Notification::FmEvent(event) => append_fm_event_attributes(event, attributes),
        Notification::HealthStateChanged(event) => {
            append_server_header_attributes(event.server_header.as_ref(), attributes);
        }
        Notification::SetAdminStateResponse(response) => {
            append_server_header_attributes(response.server_header.as_ref(), attributes);
            append_context_attribute(response.context.as_ref(), attributes);
        }
        Notification::DomainStateInfo(info) => {
            append_server_header_attributes(info.server_header.as_ref(), attributes);
            append_context_attribute(info.context.as_ref(), attributes);

            let control_plane_state = ControlPlaneState::try_from(info.control_plane_state)
                .unwrap_or(ControlPlaneState::NmxControlPlaneStateUndefined);

            push_attribute(
                attributes,
                "control_plane_state",
                control_plane_state.as_str_name(),
            );

            let controller_health = NmxControllerHealth::try_from(info.nmx_controller_health)
                .unwrap_or(NmxControllerHealth::Unknown);

            push_attribute(
                attributes,
                "nmx_controller_health",
                controller_health.as_str_name(),
            );
        }
        Notification::InitDone(done) => {
            append_server_header_attributes(done.server_header.as_ref(), attributes);
        }
    }
}

/// Appends the complete serialized NMX-C notification payload for log-oriented sinks.
fn append_notification_payload_attribute(
    notification: &Notification,
    attributes: &mut Vec<(Cow<'static, str>, String)>,
) {
    // The payload is intentionally duplicated as one opaque JSON attribute:
    // tracing's default path emits only the body, while log_file and OTLP retain
    // attributes for downstream search and filtering.
    let payload = serde_json::to_string(notification)
        .unwrap_or_else(|error| format!("failed to serialize NMX-C notification: {error}"));

    push_attribute(attributes, "notification_payload", payload);
}

/// Appends FabricManager event type and event-specific identifiers.
fn append_fm_event_attributes(event: &FmEvent, attributes: &mut Vec<(Cow<'static, str>, String)>) {
    append_server_header_attributes(event.server_header.as_ref(), attributes);
    append_context_attribute(event.context.as_ref(), attributes);

    match event.event.as_ref() {
        Some(fm_event::Event::FmEventControlPlaneStateChange(change)) => {
            push_attribute(attributes, "fm_event_type", "control_plane_state_change");
            append_context_attribute(change.context.as_ref(), attributes);
        }
        Some(fm_event::Event::FmEventTopologyChange(change)) => {
            push_attribute(attributes, "fm_event_type", "topology_change");
            append_context_attribute(change.context.as_ref(), attributes);
        }
        Some(fm_event::Event::FmEventPartitionChange(change)) => {
            push_attribute(attributes, "fm_event_type", "partition_change");
            append_context_attribute(change.context.as_ref(), attributes);
            append_partition_id_attribute(change.partition_id.as_ref(), attributes);
        }
        None => push_attribute(attributes, "fm_event_type", "missing"),
    }
}

/// Appends common NMX-C server header fields and normalizes the return code label.
fn append_server_header_attributes(
    header: Option<&ServerHeader>,
    attributes: &mut Vec<(Cow<'static, str>, String)>,
) {
    let Some(header) = header else {
        push_attribute(attributes, "server_header", "missing");
        return;
    };

    push_non_empty_attribute(attributes, "domain_uuid", &header.domain_uuid);
    push_non_empty_attribute(attributes, "app_uuid", &header.app_uuid);
    push_non_empty_attribute(attributes, "app_ver", &header.app_ver);

    let return_code = StReturnCode::try_from(header.return_code)
        .map(|return_code| return_code.as_str_name().to_string())
        .unwrap_or_else(|_| header.return_code.to_string());

    push_attribute(attributes, "return_code", return_code);
}

/// Appends a non-empty NMX-C context string when the payload carries one.
fn append_context_attribute(
    context: Option<&Context>,
    attributes: &mut Vec<(Cow<'static, str>, String)>,
) {
    if let Some(context) = context {
        push_non_empty_attribute(attributes, "context", &context.context);
    }
}

/// Appends a partition identifier when the payload carries one.
fn append_partition_id_attribute(
    partition_id: Option<&PartitionId>,
    attributes: &mut Vec<(Cow<'static, str>, String)>,
) {
    if let Some(partition_id) = partition_id {
        push_attribute(
            attributes,
            "partition_id",
            partition_id.partition_id.to_string(),
        );
    }
}

/// Adds an attribute only when the value is non-empty.
fn push_non_empty_attribute(
    attributes: &mut Vec<(Cow<'static, str>, String)>,
    key: &'static str,
    value: &str,
) {
    if !value.is_empty() {
        push_attribute(attributes, key, value);
    }
}

/// Adds a string-valued collector event attribute.
fn push_attribute(
    attributes: &mut Vec<(Cow<'static, str>, String)>,
    key: &'static str,
    value: impl ToString,
) {
    attributes.push((Cow::Borrowed(key), value.to_string()));
}

/// Returns the stable snake-case notification name for log bodies and attributes.
fn notification_name(notification: &Notification) -> &'static str {
    match notification {
        Notification::SubscriptionResponse(_) => "subscription_response",
        Notification::StaticConfigResponse(_) => "static_config_response",
        Notification::CreatePartitionResponse(_) => "create_partition_response",
        Notification::DeletePartitionResponse(_) => "delete_partition_response",
        Notification::UpdatePartitionResponse(_) => "update_partition_response",
        Notification::FmEvent(_) => "fm_event",
        Notification::HealthStateChanged(_) => "health_state_changed",
        Notification::SetAdminStateResponse(_) => "set_admin_state_response",
        Notification::DomainStateInfo(_) => "domain_state_info",
        Notification::InitDone(_) => "init_done",
    }
}

/// Emits `DomainStateInfo` as a structured log event.
fn domain_state_info_to_events(info: &DomainStateInfo) -> Vec<Result<CollectorEvent, HealthError>> {
    // The complete payload remains available to log sinks. When domain health
    // reporting is enabled, the event processor derives a domain report from
    // the structured health and server-header attributes.
    vec![Ok(notification_to_log(&Notification::DomainStateInfo(
        info.clone(),
    )))]
}

/// Builds a sink log event with the supplied severity, body, and attributes.
pub(super) fn log_record(
    severity: LogSeverity,
    body: impl Into<String>,
    attributes: Vec<(Cow<'static, str>, String)>,
) -> CollectorEvent {
    CollectorEvent::Log(Box::new(LogRecord {
        body: body.into(),
        severity,
        attributes,
        diagnostic_record: None,
    }))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use rpc::protos::nmx_c::{
        ConfigKeyVal, ConfigKeyVals, FmEventPartitionChange, HealthStateChanged, ServerHeader,
        StaticConfig, StaticConfigResponse, SubscriptionResponse, static_config,
    };

    use super::*;

    /// Builds a server header with the supplied domain UUID and return code.
    fn server_header_with_domain(
        domain_uuid: impl Into<String>,
        return_code: StReturnCode,
    ) -> ServerHeader {
        ServerHeader {
            domain_uuid: domain_uuid.into(),
            app_uuid: "app-1".to_string(),
            app_ver: "1.0".to_string(),
            return_code: return_code.into(),
        }
    }

    /// Builds a successful-domain server header with a variable return code.
    fn server_header(return_code: StReturnCode) -> ServerHeader {
        server_header_with_domain("domain-1", return_code)
    }

    /// Builds a domain-state notification with the default domain UUID.
    fn domain_state(health: NmxControllerHealth, return_code: StReturnCode) -> DomainStateInfo {
        DomainStateInfo {
            server_header: Some(server_header(return_code)),
            context: None,
            control_plane_state: ControlPlaneState::NmxControlPlaneStateConfigured.into(),
            available_multicast_groups: 10,
            config_status_description: "configured".to_string(),
            nmx_controller_health: health.into(),
        }
    }

    #[derive(Debug, PartialEq)]
    enum StreamErrorSummary {
        None,
        BadParam,
        ConnectionNotValid,
        DeadlineExceeded,
        MissingHeader,
        Unavailable,
        UnknownCode,
        Unknown(String),
    }

    /// Summarizes notification conversion output for table-driven stream tests.
    fn stream_event_summary(
        item: Result<ServerNotification, tonic::Status>,
    ) -> (usize, StreamErrorSummary) {
        let events = notification_to_events(item);
        let error = events
            .iter()
            .find_map(|event| event.as_ref().err())
            .map_or(StreamErrorSummary::None, stream_error_summary);

        (events.len(), error)
    }

    /// Maps a collector error into a compact enum for assertions.
    fn stream_error_summary(error: &HealthError) -> StreamErrorSummary {
        let message = error.to_string();

        if message.contains("timed out") {
            StreamErrorSummary::DeadlineExceeded
        } else if message.contains("NMX_ST_BADPARAM") {
            StreamErrorSummary::BadParam
        } else if message.contains("NMX_ST_CONNECTION_NOT_VALID") {
            StreamErrorSummary::ConnectionNotValid
        } else if message.contains("missing server_header") {
            StreamErrorSummary::MissingHeader
        } else if message.contains("stream unavailable") {
            StreamErrorSummary::Unavailable
        } else if message.contains("unknown return code") {
            StreamErrorSummary::UnknownCode
        } else {
            StreamErrorSummary::Unknown(message)
        }
    }

    /// Extracts the log record from a collector event.
    fn extract_log_record(event: CollectorEvent) -> Box<LogRecord> {
        let CollectorEvent::Log(record) = event else {
            panic!("expected log event");
        };

        record
    }

    /// Returns the first log record from a list of collector event results.
    fn extract_first_log_record(
        events: Vec<Result<CollectorEvent, HealthError>>,
    ) -> Option<Box<LogRecord>> {
        events.into_iter().find_map(|event| match event {
            Ok(CollectorEvent::Log(record)) => Some(record),
            Ok(_) | Err(_) => None,
        })
    }

    /// Finds a log attribute by key.
    fn log_attribute<'a>(record: &'a LogRecord, key: &str) -> Option<&'a str> {
        record
            .attributes
            .iter()
            .find(|(attribute_key, _)| attribute_key.as_ref() == key)
            .map(|(_, value)| value.as_str())
    }

    /// Verifies NMX-C endpoint URL formatting for IPv4 and IPv6 targets.
    #[test]
    fn endpoint_url_formats_ip_hosts() {
        value_scenarios!(
            run = |(host, tls_enabled)| nmxc_endpoint_url(host, 9370, tls_enabled);

            "endpoint URL" {
                ("10.0.0.1", false) => "http://10.0.0.1:9370".to_string(),

                ("[::1]", false) => "http://[::1]:9370".to_string(),

                ("10.0.0.1", true) => "https://10.0.0.1:9370".to_string(),

                ("[::1]", true) => "https://[::1]:9370".to_string(),
            }
        );
    }

    #[test]
    /// Verifies Hello uses the configured gateway ID and generated proto versions.
    fn hello_request_uses_current_proto_version_and_gateway_id() {
        let request = hello_request("hw-health");

        assert_eq!(request.gateway_id, "hw-health");

        assert_eq!(
            request.major_version,
            ProtoMsgMajorVersion::ProtoMsgMajorVersion as i32
        );

        assert_eq!(
            request.minor_version,
            ProtoMsgMinorVersion::ProtoMsgMinorVersion as i32
        );
    }

    #[test]
    /// Verifies collector timeouts are reported as gRPC deadline-exceeded statuses.
    fn timeout_error_uses_deadline_exceeded_status() {
        let HealthError::NmxcStatus(status) =
            nmxc_timeout_error("Subscribe", Duration::from_secs(2))
        else {
            panic!("expected NMX-C status error");
        };

        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);

        assert!(
            status
                .message()
                .contains("NMX-C Subscribe timed out after 2s")
        );
    }

    #[test]
    /// Verifies failed or malformed NMX-C server headers are rejected.
    fn server_header_success_check_rejects_failed_handshake_statuses() {
        value_scenarios!(
            run = |header: Option<ServerHeader>| {
                check_server_header_success(header.as_ref(), "Hello")
                    .map(|()| StreamErrorSummary::None)
                    .unwrap_or_else(|error| stream_error_summary(&error))
            };

            "server header check" {
                Some(server_header(StReturnCode::NmxStSuccess)) => StreamErrorSummary::None,

                Some(server_header(StReturnCode::NmxStBadparam)) => StreamErrorSummary::BadParam,

                Some(server_header(StReturnCode::NmxStConnectionNotValid)) => {
                    StreamErrorSummary::ConnectionNotValid
                },

                None => StreamErrorSummary::MissingHeader,

                Some(ServerHeader {
                    return_code: i32::MAX,
                    ..server_header(StReturnCode::NmxStSuccess)
                }) => {
                    StreamErrorSummary::UnknownCode
                },
            }
        );
    }

    #[test]
    /// Verifies health-state-change logs include server-header details.
    fn health_state_changed_log_includes_server_header_details() {
        let event = notification_to_log(&Notification::HealthStateChanged(HealthStateChanged {
            server_header: Some(server_header_with_domain(
                "domain-42",
                StReturnCode::NmxStSuccess,
            )),
        }));

        let body = extract_log_record(event).body;

        assert!(body.contains("health_state_changed"));
        assert!(body.contains("domain_uuid=domain-42"));
        assert!(body.contains("app_uuid=app-1"));
        assert!(body.contains("app_ver=1.0"));
        assert!(body.contains("return_code=NMX_ST_SUCCESS"));
    }

    #[test]
    /// Verifies FabricManager event logs include event type and partition context.
    fn fm_event_log_includes_event_type_and_partition_details() {
        let event = notification_to_log(&Notification::FmEvent(FmEvent {
            server_header: Some(server_header(StReturnCode::NmxStSuccess)),
            context: Some(Context {
                context: "stream-context".to_string(),
            }),
            event: Some(fm_event::Event::FmEventPartitionChange(
                FmEventPartitionChange {
                    context: Some(Context {
                        context: "partition-context".to_string(),
                    }),
                    partition_id: Some(PartitionId { partition_id: 7 }),
                },
            )),
        }));

        let body = extract_log_record(event).body;

        assert!(body.contains("fm_event"));
        assert!(body.contains("return_code=NMX_ST_SUCCESS"));
        assert!(body.contains("fm_event_type=partition_change"));
        assert!(body.contains("partition_id=7"));
    }

    #[test]
    /// Verifies generic notification logs preserve the full serialized payload.
    fn notification_log_includes_full_payload_for_log_sinks() {
        let event =
            notification_to_log(&Notification::StaticConfigResponse(StaticConfigResponse {
                server_header: Some(server_header(StReturnCode::NmxStSuccess)),
                static_config: Some(StaticConfig {
                    context: Some(Context {
                        context: "static-config-context".to_string(),
                    }),
                    config: Some(static_config::Config::ConfigKeyVals(ConfigKeyVals {
                        config_key_val: vec![ConfigKeyVal {
                            config_file_name: "switch.conf".to_string(),
                            key: "routing.mode".to_string(),
                            value: "adaptive".to_string(),
                        }],
                    })),
                }),
            }));

        let record = extract_log_record(event);
        let payload =
            log_attribute(&record, "notification_payload").expect("payload attribute is present");

        assert!(record.body.contains("notification_payload="));
        assert!(payload.contains("switch.conf"));
        assert!(payload.contains("routing.mode"));
        assert!(payload.contains("adaptive"));
        assert_eq!(log_attribute(&record, "body"), Some(record.body.as_str()));
    }

    #[test]
    /// Verifies Subscribe acknowledgement logs preserve the full serialized payload.
    fn subscription_response_log_includes_full_payload_for_log_sinks() {
        let event = subscription_response_to_log(&SubscriptionResponse {
            server_header: Some(server_header_with_domain(
                "domain-subscribe",
                StReturnCode::NmxStSuccess,
            )),
        });

        let record = extract_log_record(event);
        let payload =
            log_attribute(&record, "notification_payload").expect("payload attribute is present");

        assert!(record.body.contains("subscription_response"));
        assert!(record.body.contains("notification_payload="));
        assert!(payload.contains("SubscriptionResponse"));
        assert!(payload.contains("domain-subscribe"));
        assert_eq!(record.severity, LogSeverity::Info);
        assert_eq!(log_attribute(&record, "body"), Some(record.body.as_str()));
    }

    #[test]
    /// Verifies the first Subscribe stream item must be a successful acknowledgement.
    fn initial_subscribe_notification_requires_successful_subscription_response() {
        let success_events = initial_subscribe_notification_to_events(ServerNotification {
            notification: Some(Notification::SubscriptionResponse(SubscriptionResponse {
                server_header: Some(server_header_with_domain(
                    "domain-subscribe",
                    StReturnCode::NmxStSuccess,
                )),
            })),
        });

        assert_eq!(success_events.len(), 1);
        assert!(extract_first_log_record(success_events).is_some());

        let rejected_events = initial_subscribe_notification_to_events(ServerNotification {
            notification: Some(Notification::SubscriptionResponse(SubscriptionResponse {
                server_header: Some(server_header_with_domain(
                    "domain-rejected",
                    StReturnCode::NmxStConnectionNotValid,
                )),
            })),
        });

        assert_eq!(rejected_events.len(), 2);

        assert_eq!(
            stream_error_summary(
                rejected_events
                    .iter()
                    .find_map(|event| event.as_ref().err())
                    .expect("rejected acknowledgement should fail initial acknowledgement"),
            ),
            StreamErrorSummary::ConnectionNotValid
        );

        assert!(
            extract_first_log_record(rejected_events)
                .expect("rejected acknowledgement should preserve payload log")
                .body
                .contains("return_code=NMX_ST_CONNECTION_NOT_VALID")
        );

        let non_ack_events = initial_subscribe_notification_to_events(ServerNotification {
            notification: Some(Notification::DomainStateInfo(domain_state(
                NmxControllerHealth::Healthy,
                StReturnCode::NmxStSuccess,
            ))),
        });

        assert!(
            non_ack_events[0]
                .as_ref()
                .expect_err("non-acknowledgement notification should fail initial acknowledgement")
                .to_string()
                .contains("received domain_state_info")
        );

        let missing_payload_events =
            initial_subscribe_notification_to_events(ServerNotification { notification: None });

        assert!(
            missing_payload_events[0]
                .as_ref()
                .expect_err("missing notification should fail initial acknowledgement")
                .to_string()
                .contains("acknowledgement missing notification payload")
        );
    }

    #[test]
    /// Verifies rejected Subscribe acknowledgements emit a log before the stream error.
    fn rejected_subscription_response_emits_payload_before_stream_error() {
        let events = subscription_response_to_events(&SubscriptionResponse {
            server_header: Some(server_header_with_domain(
                "domain-rejected",
                StReturnCode::NmxStConnectionNotValid,
            )),
        });

        assert_eq!(events.len(), 2);

        let error = events
            .iter()
            .find_map(|event| event.as_ref().err())
            .expect("rejected subscription response emits stream error");

        assert_eq!(
            stream_error_summary(error),
            StreamErrorSummary::ConnectionNotValid
        );

        let record =
            extract_first_log_record(events).expect("rejected subscription response emits log");

        let payload =
            log_attribute(&record, "notification_payload").expect("payload attribute is present");

        assert!(record.body.contains("subscription_response"));

        assert!(
            record
                .body
                .contains("return_code=NMX_ST_CONNECTION_NOT_VALID")
        );

        assert!(payload.contains("SubscriptionResponse"));
        assert!(payload.contains("domain-rejected"));
        assert_eq!(record.severity, LogSeverity::Error);
    }

    #[test]
    /// Verifies DomainStateInfo emits a complete log payload.
    fn domain_state_info_events_include_full_payload_log() {
        let events = domain_state_info_to_events(&domain_state(
            NmxControllerHealth::Degraded,
            StReturnCode::NmxStSuccess,
        ));

        assert_eq!(events.len(), 1);

        let record = extract_first_log_record(events).expect("domain state emits log");
        let payload =
            log_attribute(&record, "notification_payload").expect("payload attribute is present");

        assert!(record.body.contains("domain_state_info"));
        assert!(record.body.contains("notification_payload="));
        assert!(payload.contains("DomainStateInfo"));
        assert!(payload.contains("available_multicast_groups"));
        assert!(payload.contains("config_status_description"));
        assert_eq!(log_attribute(&record, "body"), Some(record.body.as_str()));
    }

    #[test]
    /// Verifies stream notifications map to the expected event and error shapes.
    fn stream_notifications_map_to_expected_event_shapes() {
        // These cases lock the stream contract: Subscribe rejections emit a log
        // before their reconnect-driving error, successful acknowledgements stay
        // informational, and DomainStateInfo stays log-only until metric and
        // health-report semantics are designed separately.
        value_scenarios!(
            run = stream_event_summary;

            "stream notification" {
                Ok(ServerNotification {
                    notification: Some(Notification::DomainStateInfo(domain_state(
                        NmxControllerHealth::Healthy,
                        StReturnCode::NmxStSuccess,
                    ))),
                }) => (1, StreamErrorSummary::None),

                Ok(ServerNotification {
                    notification: Some(Notification::SubscriptionResponse(SubscriptionResponse {
                        server_header: Some(server_header(StReturnCode::NmxStSuccess)),
                    })),
                }) => (1, StreamErrorSummary::None),

                Ok(ServerNotification {
                    notification: Some(Notification::SubscriptionResponse(SubscriptionResponse {
                        server_header: Some(server_header(StReturnCode::NmxStBadparam)),
                    })),
                }) => (2, StreamErrorSummary::BadParam),

                Ok(ServerNotification {
                    notification: Some(Notification::SubscriptionResponse(SubscriptionResponse {
                        server_header: Some(server_header(StReturnCode::NmxStConnectionNotValid)),
                    })),
                }) => (2, StreamErrorSummary::ConnectionNotValid),

                Ok(ServerNotification {
                    notification: Some(Notification::SubscriptionResponse(SubscriptionResponse {
                        server_header: None,
                    })),
                }) => (2, StreamErrorSummary::MissingHeader),

                Ok(ServerNotification {
                    notification: Some(Notification::HealthStateChanged(HealthStateChanged {
                        server_header: None,
                    })),
                }) => (1, StreamErrorSummary::None),

                Ok(ServerNotification { notification: None }) => (1, StreamErrorSummary::None),

                Err(tonic::Status::unavailable("stream unavailable")) => {
                    (1, StreamErrorSummary::Unavailable)
                },
            }
        );
    }
}
