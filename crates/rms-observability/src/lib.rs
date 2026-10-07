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
use librms::{RpcObservation, RpcObserver};
use prost_reflect::{DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor};
use serde_json::Value;
use tower::ServiceExt;

const REDACTED: &str = "[REDACTED]";
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Enables propagation and auditing for all clients built from this config, including clones
/// and connections rebuilt after certificate rotation. Existing TLS and retry settings are retained.
/// Bodies appear in DEBUG logs and INFO spans subject to the binary's existing OTLP sampler.
/// The optional shared flag enables observation during tracing sessions; `None` uses DEBUG logs
/// alone. When both are disabled, payloads are not encoded or sanitized. Propagation stays active.
pub fn configure(
    config: &mut librms::client_config::RmsClientConfig,
    tracing_enabled: Option<Arc<AtomicBool>>,
) {
    let policy = Arc::new(NicoRmsObservability { tracing_enabled });
    config.transport_layer = Some(policy.clone());
    config.rpc_observer = Some(policy);
}

#[derive(Debug, Default)]
struct NicoRmsObservability {
    tracing_enabled: Option<Arc<AtomicBool>>,
}

impl RmsTransportLayer for NicoRmsObservability {
    fn layer(&self, transport: TransportService) -> TransportService {
        trace_propagation::TraceInjectService::new(transport).boxed_clone()
    }
}

impl RpcObserver for NicoRmsObservability {
    fn enabled(&self) -> bool {
        tracing::enabled!(target: "rms_rpc_audit", tracing::Level::DEBUG)
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
    ) -> Box<dyn RpcObservation> {
        let request_body = sanitized_body(request_type, request);
        let request_timestamp = DateTime::<Utc>::from(started).to_rfc3339();
        let span = tracing::info_span!(
            "rms_rpc",
            carbide.trace_root = true,
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
        Box::new(NicoRmsObservation {
            span,
            method,
            request_type,
            request_body,
            request_timestamp,
            started: Instant::now(),
            completed: false,
        })
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
    ) {
        let response_body = response.map(|body| sanitized_body(response_type, body));
        let response_timestamp = DateTime::<Utc>::from(finished).to_rfc3339();
        let elapsed_milliseconds = self.started.elapsed().as_secs_f64() * 1000.0;
        self.span
            .record("response_timestamp", response_timestamp.as_str());
        self.span
            .record("response_body", response_body.as_deref().unwrap_or("null"));
        self.span.record("grpc_status_code", code as i32);
        self.span
            .record("elapsed_milliseconds", elapsed_milliseconds);
        tracing::debug!(
            target: "rms_rpc_audit",
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
    }
}

impl Drop for NicoRmsObservation {
    fn drop(&mut self) {
        if !self.completed {
            self.finish("", None, tonic::Code::Cancelled, SystemTime::now());
        }
    }
}

fn descriptor_pool() -> &'static DescriptorPool {
    static POOL: OnceLock<DescriptorPool> = OnceLock::new();
    POOL.get_or_init(|| {
        DescriptorPool::decode(librms::FILE_DESCRIPTOR_SET)
            .expect("librms embeds a valid protobuf descriptor set")
    })
}

fn sanitized_body(message_type: &str, body: &[u8]) -> String {
    if body.len() > MAX_BODY_BYTES {
        return "[BODY OMITTED: exceeds 65536 bytes]".to_string();
    }
    let Some(descriptor) = descriptor_pool().get_message_by_name(message_type) else {
        return "[BODY OMITTED: unknown message type]".to_string();
    };
    let Ok(message) = DynamicMessage::decode(descriptor.clone(), body) else {
        return "[BODY OMITTED: invalid protobuf]".to_string();
    };
    let Ok(mut value) = serde_json::to_value(message) else {
        return "[BODY OMITTED: serialization failed]".to_string();
    };
    redact_message(&descriptor, &mut value);
    let serialized = value.to_string();
    if serialized.len() > MAX_BODY_BYTES {
        "[BODY OMITTED: exceeds 65536 bytes]".to_string()
    } else {
        serialized
    }
}

fn redact_message(descriptor: &MessageDescriptor, value: &mut Value) {
    let Value::Object(fields) = value else { return };
    for field in descriptor.fields() {
        if let Some(value) = fields.get_mut(field.json_name()) {
            redact_field(&field, value);
        }
    }
}

fn redact_field(field: &FieldDescriptor, value: &mut Value) {
    // Credentials and opaque strings are deliberately withheld, including JSON, config values,
    // URLs (which may carry userinfo or signed queries), and error text echoing request secrets.
    // An allowlist makes new string fields private until their semantics have been reviewed.
    if matches!(field.name(), "credentials" | "auth" | "user_pass") {
        *value = Value::String(REDACTED.to_string());
        return;
    }
    if field.is_map() && field.name() == "attributes" {
        *value = Value::String(REDACTED.to_string());
        return;
    }
    if field.is_map() {
        if let (Kind::Message(entry), Value::Object(entries)) = (field.kind(), value) {
            let value_field = entry
                .get_field_by_name("value")
                .expect("protobuf map has a value field");
            for value in entries.values_mut() {
                redact_field(&value_field, value);
            }
        }
        return;
    }
    if let Value::Array(values) = value {
        for value in values {
            redact_scalar(field, value);
        }
    } else {
        redact_scalar(field, value);
    }
}

fn redact_scalar(field: &FieldDescriptor, value: &mut Value) {
    match field.kind() {
        Kind::Message(descriptor) => redact_message(&descriptor, value),
        Kind::Bytes => *value = Value::String(REDACTED.to_string()),
        Kind::String
            if !matches!(
                field.name(),
                "node_id"
                    | "node_ids"
                    | "rack_id"
                    | "rack_ids"
                    | "job_id"
                    | "parent_job_id"
                    | "child_job_ids"
                    | "component_id"
                    | "firmware_id"
                    | "firmware_object_id"
                    | "version"
                    | "ip_address"
                    | "mac_address"
                    | "host_name"
                    | "host_ip_addresses"
                    | "host_mac_addresses"
                    | "domain"
                    | "primary_switch_node_id"
            ) =>
        {
            *value = Value::String(REDACTED.to_string())
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use librms::protos::rack_manager as rms;
    use prost::Message;

    use super::*;

    #[test]
    fn redacts_nested_credentials_in_requests_and_inventory_responses() {
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
        // Both directions use the same descriptor-driven policy, regardless of container depth.
        let bodies = [
            sanitized_body(
                "rack_manager.CreateNodesRequest",
                &rms::CreateNodesRequest {
                    nodes: Some(rms::NodeSet {
                        nodes: vec![node.clone()],
                    }),
                }
                .encode_to_vec(),
            ),
            sanitized_body("rack_manager.NodeInfo", &node.encode_to_vec()),
        ];
        for body in bodies {
            assert!(body.contains("switch-01"));
            assert!(body.contains(REDACTED));
            for secret in ["test-user", "test-password", "test-session-token"] {
                assert!(!body.contains(secret), "secret exposed in {body}");
            }
        }
    }

    #[test]
    fn redacts_direct_passwords_and_opaque_status_text() {
        let request = rms::UpdateSwitchSystemPasswordRequest {
            password: "test-new-password".to_string(),
            ..Default::default()
        };
        let request_body = sanitized_body(
            "rack_manager.UpdateSwitchSystemPasswordRequest",
            &request.encode_to_vec(),
        );
        assert!(!request_body.contains("test-new-password"));
        let response = rms::GetFirmwareJobStatusResponse {
            job_id: "job-42".to_string(),
            error_message: "failed with password test-new-password".to_string(),
            result_json: r#"{"password":"test-new-password"}"#.to_string(),
            ..Default::default()
        };
        let body = sanitized_body(
            "rack_manager.GetFirmwareJobStatusResponse",
            &response.encode_to_vec(),
        );
        assert!(body.contains("job-42"));
        assert!(!body.contains("test-new-password"));
    }

    #[test]
    fn invalid_unknown_and_oversized_payloads_never_fall_back_to_raw_bytes() {
        let cases = [
            ("rack_manager.NodeInfo", vec![255]),
            ("unknown.Type", b"test-secret".to_vec()),
            ("rack_manager.NodeInfo", vec![b'x'; MAX_BODY_BYTES + 1]),
        ];
        for (message_type, payload) in cases {
            let body = sanitized_body(message_type, &payload);
            assert!(body.starts_with("[BODY OMITTED:"));
            assert!(!body.contains("test-secret"));
        }
    }
    #[test]
    fn completion_and_cancellation_emit_redacted_records_with_timestamps() {
        use std::io::Write;
        use std::sync::Mutex;

        #[derive(Clone)]
        struct Writer(Arc<Mutex<Vec<u8>>>);
        impl Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = Writer(output.clone());
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
            let started = SystemTime::now();
            let mut observation = observer.start(
                "UpdateSwitchSystemPassword",
                "rack_manager.UpdateSwitchSystemPasswordRequest",
                &request.encode_to_vec(),
                started,
            );
            observation.finish(
                "rack_manager.UpdateSwitchSystemPasswordResponse",
                Some(&[]),
                tonic::Code::Ok,
                SystemTime::now(),
            );
            drop(observation);
            // Dropping an in-flight observation produces exactly one cancellation record.
            drop(observer.start(
                "GetVersion",
                "rack_manager.GetVersionRequest",
                &[],
                SystemTime::now(),
            ));
        });
        let output = String::from_utf8(output.lock().unwrap().clone()).unwrap();
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
        for record in records {
            let fields = &record["fields"];
            let started =
                DateTime::parse_from_rfc3339(fields["request_timestamp"].as_str().unwrap())
                    .unwrap();
            let finished =
                DateTime::parse_from_rfc3339(fields["response_timestamp"].as_str().unwrap())
                    .unwrap();
            assert!(finished >= started);
            assert!(fields["elapsed_milliseconds"].as_f64().unwrap() >= 0.0);
        }
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
            };
            assert!(!observer.enabled());
            enabled.store(true, Ordering::Relaxed);
            assert!(observer.enabled());
            enabled.store(false, Ordering::Relaxed);
            assert!(!observer.enabled());
        });
    }
}
