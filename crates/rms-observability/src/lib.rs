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

//! NICo policy for W3C propagation and redacted unary RMS RPC audit records.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime};

use chrono::{DateTime, Utc};
use librms::client::{RmsTransportLayer, TransportService};
use librms::{RpcObservation, RpcObserver, RpcObserverError};
use prost_reflect::{DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor};
use serde_json::Value;
use tower::ServiceExt;
use tracing::dispatcher::Dispatch;
use tracing::instrument::{WithDispatch, WithSubscriber};

const REDACTED: &str = "[REDACTED]";
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Target of the DEBUG audit event, for filter directives such as `rms_rpc_audit=debug`.
pub const AUDIT_TARGET: &str = "rms_rpc_audit";

/// Reports whether the binary's current log filter lets the audit event reach its log output.
pub type AuditLogGate = Arc<dyn Fn() -> bool + Send + Sync>;

/// Enables propagation and auditing for all clients built from this config, including clones
/// and connections rebuilt after certificate rotation. Existing TLS and retry settings are retained.
/// Bodies appear in DEBUG logs and INFO spans subject to the binary's existing OTLP sampler.
/// The optional shared flag enables observation during tracing sessions; `None` uses DEBUG logs
/// alone. `audit_logging` answers whether the audit event is currently logged; binaries with
/// several tracing layers must supply it, because the default check passes whenever any layer
/// accepts DEBUG. When both are disabled, payloads are not encoded or sanitized. Propagation
/// stays active.
pub fn configure(
    config: &mut librms::client_config::RmsClientConfig,
    tracing_enabled: Option<Arc<AtomicBool>>,
    audit_logging: Option<AuditLogGate>,
) {
    let policy = Arc::new(NicoRmsObservability {
        tracing_enabled,
        audit_logging,
    });
    config.transport_layer = Some(policy.clone());
    config.rpc_observer = Some(policy);
}

#[derive(Default)]
struct NicoRmsObservability {
    tracing_enabled: Option<Arc<AtomicBool>>,
    audit_logging: Option<AuditLogGate>,
}

impl std::fmt::Debug for NicoRmsObservability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NicoRmsObservability")
            .field("tracing_enabled", &self.tracing_enabled)
            .field("audit_logging", &self.audit_logging.is_some())
            .finish()
    }
}

impl RmsTransportLayer for NicoRmsObservability {
    fn layer(&self, transport: TransportService) -> TransportService {
        // Injection runs first, under the caller's span; the transport then runs detached.
        trace_propagation::TraceInjectService::new(DetachedFromCallerSpan(transport)).boxed_clone()
    }
}

/// Runs the inner service without the caller's span or subscriber.
///
/// Hyper's executor spawns each connection task with `in_current_span()` when its `tracing`
/// feature is enabled, as `kube-client` does in `nico-api`. A connection opened during the first
/// RPC would otherwise keep that RPC's span open for the life of the connection, delaying its
/// export and inflating its duration. Events from the inner call are dropped in exchange.
#[derive(Clone)]
struct DetachedFromCallerSpan<S>(S);

impl<S, R> tower::Service<R> for DetachedFromCallerSpan<S>
where
    S: tower::Service<R>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = WithDispatch<S::Future>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), S::Error>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, request: R) -> Self::Future {
        // `call` may do work before returning its future; the future is detached on each poll.
        tracing::dispatcher::with_default(&Dispatch::none(), || self.0.call(request))
            .with_subscriber(Dispatch::none())
    }
}

impl RpcObserver for NicoRmsObservability {
    fn enabled(&self) -> bool {
        let audit_logging = match &self.audit_logging {
            Some(gate) => gate(),
            None => tracing::enabled!(target: AUDIT_TARGET, tracing::Level::DEBUG),
        };
        audit_logging
            || self
                .tracing_enabled
                .as_ref()
                .is_some_and(|enabled| enabled.load(Ordering::Relaxed))
    }

    fn start(
        &self,
        method: &'static str,
        request_type: &'static str,
        request: &[u8],
        started: SystemTime,
    ) -> Result<Box<dyn RpcObservation>, RpcObserverError> {
        // `started` was taken before this callback ran. Back-date the monotonic clock to it so
        // `elapsed_milliseconds` spans the same interval as the two timestamps instead of
        // omitting the body sanitization and span setup below.
        let lag = SystemTime::now()
            .duration_since(started)
            .unwrap_or_default();
        let entered = Instant::now();
        let started_at = entered.checked_sub(lag).unwrap_or(entered);
        let request_body = sanitized_body(request_type, request);
        let request_timestamp = DateTime::<Utc>::from(started).to_rfc3339();
        let span = tracing::info_span!(
            "rms_rpc",
            carbide.trace_root = true,
            // Bodies are reported through the `rms_rpc_audit` event and OTLP, not a close line.
            logfmt.suppress = true,
            rpc.system = "grpc",
            rpc.method = method,
            rpc.request_type = request_type,
            backend = "rms",
            request_timestamp = %request_timestamp,
            request_body = %request_body,
            response_timestamp = tracing::field::Empty,
            response_body = tracing::field::Empty,
            grpc_status_code = tracing::field::Empty,
            elapsed_milliseconds = tracing::field::Empty,
        );
        Ok(Box::new(NicoRmsObservation {
            span,
            method,
            request_type,
            request_body,
            request_timestamp,
            started: started_at,
            completed: false,
        }))
    }
}

struct NicoRmsObservation {
    span: tracing::Span,
    method: &'static str,
    request_type: &'static str,
    request_body: String,
    request_timestamp: String,
    started: Instant,
    completed: bool,
}

impl RpcObservation for NicoRmsObservation {
    fn span(&self) -> tracing::Span {
        self.span.clone()
    }

    fn finish(
        &mut self,
        response_type: &'static str,
        response: Option<&[u8]>,
        code: tonic::Code,
        finished: SystemTime,
    ) -> Result<(), RpcObserverError> {
        // Measured before sanitizing the response, which `finished` also precedes.
        let elapsed_milliseconds = self.started.elapsed().as_secs_f64() * 1000.0;
        let response_body = response.map(|body| sanitized_body(response_type, body));
        let response_timestamp = DateTime::<Utc>::from(finished).to_rfc3339();
        self.span
            .record("response_timestamp", response_timestamp.as_str());
        self.span
            .record("response_body", response_body.as_deref().unwrap_or("null"));
        self.span.record("grpc_status_code", code as i32);
        self.span
            .record("elapsed_milliseconds", elapsed_milliseconds);
        tracing::debug!(
            target: AUDIT_TARGET,
            parent: &self.span,
            backend = "rms",
            rpc_method = self.method,
            request_type = self.request_type,
            response_type,
            request_timestamp = %self.request_timestamp,
            %response_timestamp,
            request_body = %self.request_body,
            response_body = response_body.as_deref().unwrap_or("null"),
            grpc_status_code = code as i32,
            elapsed_milliseconds,
            "RMS RPC completed",
        );
        self.completed = true;
        Ok(())
    }
}

impl Drop for NicoRmsObservation {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.finish("", None, tonic::Code::Cancelled, SystemTime::now());
        }
    }
}

/// Marks a descriptor that cannot be walked, so the body is omitted rather than risk exposing it.
struct MalformedDescriptor;

fn descriptor_pool() -> Option<&'static DescriptorPool> {
    static POOL: OnceLock<Option<DescriptorPool>> = OnceLock::new();
    POOL.get_or_init(|| DescriptorPool::decode(librms::FILE_DESCRIPTOR_SET).ok())
        .as_ref()
}

fn sanitized_body(message_type: &str, body: &[u8]) -> String {
    if body.len() > MAX_BODY_BYTES {
        return "[BODY OMITTED: exceeds 65536 bytes]".to_string();
    }
    let Some(pool) = descriptor_pool() else {
        return "[BODY OMITTED: descriptors unavailable]".to_string();
    };
    let Some(descriptor) = pool.get_message_by_name(message_type) else {
        return "[BODY OMITTED: unknown message type]".to_string();
    };
    let Ok(message) = DynamicMessage::decode(descriptor.clone(), body) else {
        return "[BODY OMITTED: invalid protobuf]".to_string();
    };
    let Ok(mut value) = serde_json::to_value(message) else {
        return "[BODY OMITTED: serialization failed]".to_string();
    };
    if redact_message(&descriptor, &mut value).is_err() {
        return "[BODY OMITTED: malformed descriptor]".to_string();
    }
    let serialized = value.to_string();
    if serialized.len() > MAX_BODY_BYTES {
        "[BODY OMITTED: exceeds 65536 bytes]".to_string()
    } else {
        serialized
    }
}

fn redact_message(
    descriptor: &MessageDescriptor,
    value: &mut Value,
) -> Result<(), MalformedDescriptor> {
    let Value::Object(fields) = value else {
        return Ok(());
    };
    for field in descriptor.fields() {
        if let Some(value) = fields.get_mut(field.json_name()) {
            redact_field(&field, value)?;
        }
    }
    Ok(())
}

/// Fields whose whole value is withheld, whatever its type.
const WITHHELD_FIELDS: &[&str] = &["credentials", "user_pass", "attributes"];

/// String fields whose values are retained: identifiers, addresses, domains, and versions.
/// Every other string field is withheld, including JSON, config values, URLs (which may carry
/// userinfo or signed queries), and error text echoing request secrets. An allowlist makes a new
/// string field private until its semantics have been reviewed; `exposed_string_fields` in the
/// tests pins exactly which fields this retains.
const VISIBLE_STRING_FIELDS: &[&str] = &[
    "node_id",
    "node_ids",
    "rack_id",
    "rack_ids",
    "job_id",
    "parent_job_id",
    "child_job_ids",
    "component_id",
    "version",
    "ip_address",
    "mac_address",
    "host_name",
    "host_ip_addresses",
    "host_mac_addresses",
    "domain",
    "primary_switch_node_id",
];

fn is_visible_string(field: &FieldDescriptor) -> bool {
    matches!(field.kind(), Kind::String) && VISIBLE_STRING_FIELDS.contains(&field.name())
}

fn redact_field(field: &FieldDescriptor, value: &mut Value) -> Result<(), MalformedDescriptor> {
    if WITHHELD_FIELDS.contains(&field.name()) {
        *value = Value::String(REDACTED.to_string());
        return Ok(());
    }
    if field.is_map() {
        if let (Kind::Message(entry), Value::Object(entries)) = (field.kind(), value) {
            let value_field = entry
                .get_field_by_name("value")
                .ok_or(MalformedDescriptor)?;
            for value in entries.values_mut() {
                redact_field(&value_field, value)?;
            }
        }
        return Ok(());
    }
    if let Value::Array(values) = value {
        for value in values {
            redact_scalar(field, value)?;
        }
        Ok(())
    } else {
        redact_scalar(field, value)
    }
}

fn redact_scalar(field: &FieldDescriptor, value: &mut Value) -> Result<(), MalformedDescriptor> {
    match field.kind() {
        Kind::Message(descriptor) => return redact_message(&descriptor, value),
        Kind::Bytes => *value = Value::String(REDACTED.to_string()),
        Kind::String if !is_visible_string(field) => *value = Value::String(REDACTED.to_string()),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::Write;
    use std::sync::Mutex;
    use std::time::Duration;

    use carbide_test_support::{Check, check_values};
    use librms::protos::{rack_manager as rms, rack_manager_v2 as rms_v2};
    use prost::Message;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    const OMITTED_INVALID: &str = "[BODY OMITTED: invalid protobuf]";
    const OMITTED_UNKNOWN: &str = "[BODY OMITTED: unknown message type]";
    const OMITTED_OVERSIZED: &str = "[BODY OMITTED: exceeds 65536 bytes]";

    /// Every value any `sanitized_body` row plants, plus the markers the policy may substitute.
    /// A row's expectation is the subset of these that survives into the sanitized body.
    const PROBES: &[&str] = &[
        "switch-01",
        "job-42",
        "test-user",
        "test-password",
        "test-session-token",
        "test-new-password",
        "test-secret",
        REDACTED,
        OMITTED_INVALID,
        OMITTED_UNKNOWN,
        OMITTED_OVERSIZED,
    ];

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Capture {
        fn output(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    struct Payload {
        message_type: &'static str,
        bytes: Vec<u8>,
    }

    fn payload(message_type: &'static str, message: &impl Message) -> Payload {
        Payload {
            message_type,
            bytes: message.encode_to_vec(),
        }
    }

    fn surviving_probes(input: Payload) -> Vec<&'static str> {
        let body = sanitized_body(input.message_type, &input.bytes);
        PROBES
            .iter()
            .copied()
            .filter(|probe| body.contains(probe))
            .collect()
    }

    #[test]
    fn sanitized_body_keeps_identifiers_and_withholds_everything_else() {
        let node = rms::NodeInfo {
            node_id: "switch-01".to_string(),
            host_endpoint: Some(rms::Endpoint {
                credentials: Some(rms::Credentials {
                    auth: Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
                        username: "test-user".to_string(),
                        password: "test-password".to_string(),
                    })),
                }),
                ..Default::default()
            }),
            bmc_endpoint: Some(rms::Endpoint {
                credentials: Some(rms::Credentials {
                    auth: Some(rms::credentials::Auth::SessionToken(
                        "test-session-token".to_string(),
                    )),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let checks = [
            Check {
                scenario: "credentials nested in a request container",
                input: payload(
                    "rack_manager.CreateNodesRequest",
                    &rms::CreateNodesRequest {
                        nodes: Some(rms::NodeSet {
                            nodes: vec![node.clone()],
                        }),
                    },
                ),
                expect: vec!["switch-01", REDACTED],
            },
            Check {
                scenario: "credentials in an inventory-style response message",
                input: payload("rack_manager.NodeInfo", &node),
                expect: vec!["switch-01", REDACTED],
            },
            Check {
                scenario: "direct password field",
                input: payload(
                    "rack_manager.UpdateSwitchSystemPasswordRequest",
                    &rms::UpdateSwitchSystemPasswordRequest {
                        password: "test-new-password".to_string(),
                        ..Default::default()
                    },
                ),
                expect: vec![REDACTED],
            },
            Check {
                scenario: "opaque status text and embedded JSON beside an identifier",
                input: payload(
                    "rack_manager.GetFirmwareJobStatusResponse",
                    &rms::GetFirmwareJobStatusResponse {
                        job_id: "job-42".to_string(),
                        error_message: "failed with password test-new-password".to_string(),
                        result_json: r#"{"password":"test-new-password"}"#.to_string(),
                        ..Default::default()
                    },
                ),
                expect: vec!["job-42", REDACTED],
            },
            Check {
                scenario: "V2 message names resolve in the descriptor pool",
                input: payload(
                    "rack_manager_v2.ConfigureScaleUpFabricManagerRequest",
                    &rms_v2::ConfigureScaleUpFabricManagerRequest {
                        primary_switch_node_id: Some("switch-01".to_string()),
                        ..Default::default()
                    },
                ),
                expect: vec!["switch-01"],
            },
            Check {
                scenario: "malformed protobuf never falls back to raw bytes",
                input: Payload {
                    message_type: "rack_manager.NodeInfo",
                    bytes: vec![255],
                },
                expect: vec![OMITTED_INVALID],
            },
            Check {
                scenario: "unknown message type never falls back to raw bytes",
                input: Payload {
                    message_type: "unknown.Type",
                    bytes: b"test-secret".to_vec(),
                },
                expect: vec![OMITTED_UNKNOWN],
            },
            Check {
                scenario: "oversized payload is omitted before decoding",
                input: Payload {
                    message_type: "rack_manager.NodeInfo",
                    bytes: vec![b'x'; MAX_BODY_BYTES + 1],
                },
                expect: vec![OMITTED_OVERSIZED],
            },
        ];
        check_values(checks, surviving_probes);
    }

    fn rms_messages() -> impl Iterator<Item = MessageDescriptor> {
        descriptor_pool()
            .expect("librms embeds a valid protobuf descriptor set")
            .all_messages()
            .filter(|message| {
                !message.is_map_entry() && message.full_name().starts_with("rack_manager")
            })
    }

    /// Names of the fields of `message` whose values `sanitized_body` leaves readable: allowlisted
    /// string fields, and the keys of string-keyed maps, which the policy never rewrites.
    fn exposed_string_fields(message: &MessageDescriptor) -> Vec<String> {
        message
            .fields()
            .filter(|field| !WITHHELD_FIELDS.contains(&field.name()))
            .filter_map(|field| match field.kind() {
                Kind::Message(entry)
                    if field.is_map()
                        && matches!(entry.map_entry_key_field().kind(), Kind::String) =>
                {
                    Some(format!("{}{{key}}", field.name()))
                }
                _ if is_visible_string(&field) => Some(field.name().to_string()),
                _ => None,
            })
            .collect()
    }

    /// Every RMS message that exposes a string, with the fields it exposes. Adding a message,
    /// or adding or renaming a field that matches the allowlist, changes what is written to logs
    /// and traces, so it must be reviewed here: extend the row only once the value is known to
    /// be safe to record.
    const EXPOSED_STRING_FIELDS: &[(&str, &[&str])] = &[
        ("rack_manager.ApplyFirmwareObjectRequest", &["rack_id"]),
        (
            "rack_manager.ApplyStoredFirmwareObjectRequest",
            &["rack_id"],
        ),
        (
            "rack_manager.ApplyStoredSwitchSystemImageRequest",
            &["rack_id"],
        ),
        ("rack_manager.ApplySwitchSystemImageRequest", &["rack_id"]),
        (
            "rack_manager.BatchCollectSwitchSpdmAttestationEvidenceRequest",
            &["domain"],
        ),
        (
            "rack_manager.BatchGetScaleUpFabricServiceStatusResponse",
            &["service_statuses{key}"],
        ),
        (
            "rack_manager.BatchResetSwitchFactoryDefaultRequest",
            &["domain"],
        ),
        (
            "rack_manager.BatchResetSwitchSdnFactoryDefaultRequest",
            &["domain"],
        ),
        (
            "rack_manager.BatchUpdateFirmwareByNodeTypeRequest",
            &["rack_id"],
        ),
        ("rack_manager.ComponentInventoryInfo", &["component_id"]),
        (
            "rack_manager.ConfigureScaleUpFabricManagerRequest",
            &["domain"],
        ),
        (
            "rack_manager.ConfigureSwitchCertificateJobInfo",
            &["node_id", "job_id"],
        ),
        (
            "rack_manager.ConfigureSwitchCertificateRequest",
            &["domain"],
        ),
        ("rack_manager.DeleteNodeRequest", &["node_id", "rack_id"]),
        ("rack_manager.ExecuteColdRebootRequest", &["domain"]),
        ("rack_manager.FirmwareInventoryInfo", &["version"]),
        ("rack_manager.FirmwareObjectComponent", &["version"]),
        (
            "rack_manager.FirmwareObjectHistoryRecord",
            &["rack_id", "node_ids"],
        ),
        ("rack_manager.FirmwareObjectSubcomponent", &["version"]),
        ("rack_manager.FirmwareObjectSwitchSystemImage", &["version"]),
        (
            "rack_manager.GetConfigureSwitchCertificateJobStatusRequest",
            &["job_id"],
        ),
        (
            "rack_manager.GetConfigureSwitchCertificateJobStatusResponse",
            &["job_id", "rack_id", "node_id"],
        ),
        ("rack_manager.GetFirmwareJobStatusRequest", &["job_id"]),
        (
            "rack_manager.GetFirmwareJobStatusResponse",
            &["job_id", "rack_id", "node_id"],
        ),
        (
            "rack_manager.GetFirmwareObjectHistoryRequest",
            &["rack_ids"],
        ),
        ("rack_manager.GetJobStatusRequest", &["job_id"]),
        (
            "rack_manager.GetNodeDeviceInfoRequest",
            &["rack_id", "node_id"],
        ),
        (
            "rack_manager.GetNodeFirmwareInventoryRequest",
            &["node_id", "rack_id"],
        ),
        ("rack_manager.GetPowerStateRequest", &["node_id", "rack_id"]),
        (
            "rack_manager.GetPowerStateResponse",
            &["node_id", "rack_id"],
        ),
        ("rack_manager.GetRackFirmwareInventoryRequest", &["rack_id"]),
        ("rack_manager.GetScaleUpFabricStatusRequest", &["domain"]),
        (
            "rack_manager.GetSwitchSystemImageJobStatusRequest",
            &["job_id"],
        ),
        (
            "rack_manager.GetSwitchSystemImageJobStatusResponse",
            &["job_id", "rack_id", "node_id"],
        ),
        ("rack_manager.GetVersionResponse", &["version"]),
        (
            "rack_manager.JobStatus",
            &[
                "job_id",
                "parent_job_id",
                "child_job_ids",
                "rack_id",
                "node_id",
            ],
        ),
        (
            "rack_manager.ListNodeDeviceInfoByNodeTypeRequest",
            &["rack_id"],
        ),
        ("rack_manager.ListRacksResponse", &["rack_ids"]),
        (
            "rack_manager.ListSwitchFirmwareRequest",
            &["rack_id", "node_id"],
        ),
        (
            "rack_manager.ListSwitchSystemImagesRequest",
            &["rack_id", "node_id"],
        ),
        (
            "rack_manager.NetworkInterface",
            &["ip_address", "mac_address", "host_name"],
        ),
        ("rack_manager.NodeBatchResponse", &["job_id"]),
        ("rack_manager.NodeDeviceInfo", &["node_id"]),
        ("rack_manager.NodeFirmwareInventory", &["node_id"]),
        ("rack_manager.NodeFirmwareJobInfo", &["node_id", "job_id"]),
        ("rack_manager.NodeFirmwareManifestComparison", &["node_id"]),
        ("rack_manager.NodeInfo", &["node_id", "rack_id"]),
        (
            "rack_manager.NodeInventoryInfo",
            &[
                "node_id",
                "ip_address",
                "mac_address",
                "rack_id",
                "host_mac_addresses",
                "host_ip_addresses",
            ],
        ),
        ("rack_manager.NodeOperationResult", &["node_id"]),
        ("rack_manager.NodePowerState", &["node_id"]),
        (
            "rack_manager.PushSwitchFirmwareRequest",
            &["rack_id", "node_id"],
        ),
        ("rack_manager.ScaleUpFabricSwitchStatus", &["node_id"]),
        ("rack_manager.SetPowerStateRequest", &["node_id", "rack_id"]),
        (
            "rack_manager.SwitchSpdmAttestationTargetResult",
            &["node_id"],
        ),
        ("rack_manager.SwitchSpdmComponentResult", &["component_id"]),
        (
            "rack_manager.SwitchSystemImageUpdateJobInfo",
            &["node_id", "job_id"],
        ),
        (
            "rack_manager.UpdateFirmwareRequest",
            &["node_id", "rack_id"],
        ),
        ("rack_manager.UpdateFirmwareResponse", &["job_id"]),
        ("rack_manager.UpdateNodeRequest", &["node_id", "rack_id"]),
        (
            "rack_manager_v2.ConfigureScaleUpFabricManagerRequest",
            &["primary_switch_node_id", "domain"],
        ),
        (
            "rack_manager_v2.ConfigureScaleUpFabricManagerResponse",
            &["job_id"],
        ),
        (
            "rack_manager_v2.NodeSystemValidationJobInfo",
            &["node_id", "job_id"],
        ),
    ];

    #[test]
    fn exposed_string_fields_match_the_reviewed_snapshot() {
        let checks = EXPOSED_STRING_FIELDS.iter().map(|(message, fields)| Check {
            scenario: message,
            input: *message,
            expect: fields.iter().map(|field| field.to_string()).collect(),
        });
        check_values(checks, |message| {
            exposed_string_fields(
                &descriptor_pool()
                    .and_then(|pool| pool.get_message_by_name(message))
                    .unwrap_or_else(|| panic!("{message} is no longer in the descriptor set")),
            )
        });
        // The rows above only cover messages already listed, so also catch a new one.
        let exposing: BTreeSet<String> = rms_messages()
            .filter(|message| !exposed_string_fields(message).is_empty())
            .map(|message| message.full_name().to_string())
            .collect();
        let reviewed: BTreeSet<String> = EXPOSED_STRING_FIELDS
            .iter()
            .map(|(message, _)| message.to_string())
            .collect();
        assert_eq!(
            exposing, reviewed,
            "messages exposing strings are unreviewed"
        );
    }

    #[test]
    fn policy_field_names_still_exist_in_the_descriptor_set() {
        // A name that matches no field is a stale entry: the proto renamed or dropped the field,
        // so the policy no longer says what its author reviewed.
        let allowlisted = VISIBLE_STRING_FIELDS.iter().map(|name| (*name, true));
        let withheld = WITHHELD_FIELDS.iter().map(|name| (*name, false));
        let checks = allowlisted
            .chain(withheld)
            .map(|(name, string_only)| Check {
                scenario: name,
                input: (name, string_only),
                expect: true,
            });
        check_values(checks, |(name, string_only)| {
            rms_messages().any(|message| {
                message.fields().any(|field| {
                    field.name() == name && (!string_only || matches!(field.kind(), Kind::String))
                })
            })
        });
    }

    #[test]
    fn completion_and_cancellation_emit_redacted_records_with_timestamps() {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let observer = NicoRmsObservability::default();
            let request = rms::UpdateSwitchSystemPasswordRequest {
                password: "test-secret".to_string(),
                ..Default::default()
            };
            // The caller took `started` before invoking the observer, so the audited call began
            // 50 ms ago by the time `start` runs.
            let started = SystemTime::now() - Duration::from_millis(50);
            let mut observation = observer
                .start(
                    "UpdateSwitchSystemPassword",
                    "rack_manager.UpdateSwitchSystemPasswordRequest",
                    &request.encode_to_vec(),
                    started,
                )
                .unwrap();
            observation
                .finish(
                    "rack_manager.UpdateSwitchSystemPasswordResponse",
                    Some(&[]),
                    tonic::Code::Ok,
                    SystemTime::now(),
                )
                .unwrap();
            drop(observation);
            // Dropping an in-flight observation produces exactly one cancellation record.
            drop(
                observer
                    .start(
                        "GetVersion",
                        "rack_manager.GetVersionRequest",
                        &[],
                        SystemTime::now(),
                    )
                    .unwrap(),
            );
        });
        let output = capture.output();
        assert!(!output.contains("test-secret"));
        let records: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["fields"]["grpc_status_code"], 0);
        assert_eq!(records[1]["fields"]["grpc_status_code"], 1);
        assert!(
            records[0]["fields"]["request_body"]
                .as_str()
                .unwrap()
                .contains(REDACTED)
        );
        // The first call started 50 ms before the observer was invoked.
        assert!(
            records[0]["fields"]["elapsed_milliseconds"]
                .as_f64()
                .unwrap()
                >= 50.0
        );
        for record in records {
            let fields = &record["fields"];
            let started =
                DateTime::parse_from_rfc3339(fields["request_timestamp"].as_str().unwrap())
                    .unwrap();
            let finished =
                DateTime::parse_from_rfc3339(fields["response_timestamp"].as_str().unwrap())
                    .unwrap();
            assert!(finished >= started);
            // The monotonic elapsed time covers the same interval as the two timestamps; the
            // tolerance absorbs skew between the wall and monotonic clocks.
            let timestamp_gap_ms = (finished - started).num_microseconds().unwrap() as f64 / 1000.0;
            let elapsed_ms = fields["elapsed_milliseconds"].as_f64().unwrap();
            assert!(
                elapsed_ms + 1.0 >= timestamp_gap_ms,
                "elapsed {elapsed_ms} ms is shorter than the {timestamp_gap_ms} ms between timestamps"
            );
        }
    }

    #[test]
    fn logfmt_writes_the_audit_event_but_not_a_span_close_line() {
        // The close line would repeat both bodies on every call at INFO, outside the DEBUG gate.
        let capture = Capture::default();
        let writer = capture.clone();
        let layer = logfmt::layer().with_writer(Arc::new(move || Box::new(writer.clone())));
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let observer = NicoRmsObservability::default();
            let mut observation = observer
                .start(
                    "GetVersion",
                    "rack_manager.GetVersionRequest",
                    &[],
                    SystemTime::now(),
                )
                .unwrap();
            observation
                .finish(
                    "rack_manager.GetVersionResponse",
                    Some(&[]),
                    tonic::Code::Ok,
                    SystemTime::now(),
                )
                .unwrap();
            drop(observation);
        });
        let output = capture.output();
        assert!(output.contains("RMS RPC completed"), "{output}");
        assert!(!output.contains("level=SPAN"), "{output}");
    }

    #[test]
    fn runtime_tracing_flag_enables_observation_without_debug_logs() {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let enabled = Arc::new(AtomicBool::new(false));
            let observer = NicoRmsObservability {
                tracing_enabled: Some(enabled.clone()),
                ..Default::default()
            };
            assert!(!observer.enabled());
            enabled.store(true, Ordering::Relaxed);
            assert!(observer.enabled());
            enabled.store(false, Ordering::Relaxed);
            assert!(!observer.enabled());
        });
    }

    struct Gates {
        audit_logging: Option<bool>,
        tracing: bool,
    }

    #[test]
    fn observation_follows_the_audit_filter_not_any_layer_accepting_debug() {
        let checks = [
            Check {
                scenario: "audit filter and tracing session both off",
                input: Gates {
                    audit_logging: Some(false),
                    tracing: false,
                },
                expect: false,
            },
            Check {
                scenario: "audit filter on",
                input: Gates {
                    audit_logging: Some(true),
                    tracing: false,
                },
                expect: true,
            },
            Check {
                scenario: "tracing session on with the audit filter off",
                input: Gates {
                    audit_logging: Some(false),
                    tracing: true,
                },
                expect: true,
            },
            Check {
                scenario: "no audit filter supplied defers to the subscriber",
                input: Gates {
                    audit_logging: None,
                    tracing: false,
                },
                expect: true,
            },
        ];
        // A DEBUG consumer unrelated to audit logging, like the API's span counter, makes
        // `tracing::enabled!` true by itself.
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::sink)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            check_values(checks, |gates| {
                NicoRmsObservability {
                    tracing_enabled: Some(Arc::new(AtomicBool::new(gates.tracing))),
                    audit_logging: gates
                        .audit_logging
                        .map(|enabled| Arc::new(move || enabled) as AuditLogGate),
                }
                .enabled()
            });
        });
    }

    struct CloseRecorder(Arc<Mutex<Vec<&'static str>>>);

    impl<S> tracing_subscriber::Layer<S> for CloseRecorder
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn on_close(&self, id: tracing::span::Id, ctx: tracing_subscriber::layer::Context<'_, S>) {
            if let Some(span) = ctx.span(&id) {
                self.0.lock().unwrap().push(span.name());
            }
        }
    }

    fn call_once<S>(mut service: S, span: &tracing::Span)
    where
        S: tower::Service<(), Error = std::convert::Infallible>,
    {
        use std::future::Future;

        use tracing::Instrument;

        let mut call = std::pin::pin!(span.in_scope(|| service.call(())).instrument(span.clone()));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(call.as_mut().poll(&mut cx).is_ready());
    }

    #[test]
    fn connection_tasks_do_not_retain_the_rpc_span() {
        let checks = [
            Check {
                scenario: "a clone captured during the call keeps the span open",
                input: false,
                expect: false,
            },
            Check {
                scenario: "a detached call closes the span with the RPC",
                input: true,
                expect: true,
            },
        ];
        check_values(checks, |detached| {
            let closed = Arc::new(Mutex::new(Vec::new()));
            let subscriber = tracing_subscriber::registry().with(CloseRecorder(closed.clone()));
            tracing::subscriber::with_default(subscriber, || {
                // Hyper's `tracing` executor captures `Span::current()` for each connection task
                // it spawns; the clone stands in for that task.
                let retained = Arc::new(Mutex::new(Vec::new()));
                let capture = retained.clone();
                let transport = tower::service_fn(move |()| {
                    let capture = capture.clone();
                    async move {
                        capture.lock().unwrap().push(tracing::Span::current());
                        Ok::<_, std::convert::Infallible>(())
                    }
                });
                let span = tracing::info_span!("rms_rpc");
                if detached {
                    call_once(DetachedFromCallerSpan(transport), &span);
                } else {
                    call_once(transport, &span);
                }
                drop(span);
                let closed_with_rpc = closed.lock().unwrap().contains(&"rms_rpc");
                drop(retained);
                closed_with_rpc
            })
        });
    }
}
