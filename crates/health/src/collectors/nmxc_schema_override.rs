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

//! Descriptor-driven replacement for the generated NMX-C collector.
//!
//! The descriptor is validated at startup, supplies both RPC request and
//! response types, and decodes every Subscribe frame to canonical protobuf
//! JSON. The generated NMX-C event mapping is not used in this mode.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Buf, Bytes};
use futures::StreamExt;
use http::uri::PathAndQuery;
use nv_redfish::core::Bmc;
use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
    OneofDescriptor, ReflectMessage, Value,
};
use serde::de::IntoDeserializer;
use tonic::codec::{BufferSettings, Codec, DecodeBuf, Decoder};
use tonic::transport::Channel;
use tonic::{Status, Streaming};
use tonic_prost::ProstEncoder;

use super::nmxc::{log_record, nmxc_channel, nmxc_endpoint_url, nmxc_timeout_error};
use super::runtime::{StreamingCollector, StreamingConnectResult};
use crate::HealthError;
use crate::config::{
    MtlsProfileConfig, NmxcCollectorConfig as NmxcCollectorOptions, NmxcSchemaOverrideConfig,
};
use crate::endpoint::BmcEndpoint;
use crate::sink::{CollectorEvent, LogSeverity};

const NMX_C_CURRENT_MAJOR_VERSION: &str = "PROTO_MSG_MAJOR_VERSION";
const NMX_C_CURRENT_MINOR_VERSION: &str = "PROTO_MSG_MINOR_VERSION";
const NMX_C_SUCCESS_RETURN_CODE: &str = "NMX_ST_SUCCESS";

// The collector populates protobuf fields 1, 2, and 3 in each request:
// Hello: gateway_id, major_version, minor_version.
// Subscribe: gateway_id, notify_on_self_change, heart_beat_rate.
const NMX_C_CORE_REQUEST_FIELDS: [u32; 3] = [1, 2, 3];

/// Validated descriptor state and immutable requests for one NMX-C schema override.
///
/// Loading verifies the Hello and Subscribe method shapes, the base NMX-C
/// request fields, response status fields, and the initial acknowledgement.
/// Runtime frames are decoded exclusively through this descriptor.
#[derive(Clone, Debug)]
pub(crate) struct NmxcSchemaOverride {
    hello_request: DynamicMessage,
    hello_response: MessageDescriptor,
    subscribe_request: DynamicMessage,
    response: MessageDescriptor,
    notification: OneofDescriptor,
    hello_rpc_path: PathAndQuery,
    subscribe_rpc_path: PathAndQuery,
    max_frame_size_bytes: usize,
}

impl NmxcSchemaOverride {
    /// Loads and validates the descriptor set and builds immutable request templates.
    pub(crate) fn load(
        schema_override: &NmxcSchemaOverrideConfig,
        nmxc: &NmxcCollectorOptions,
    ) -> Result<Self, NmxcSchemaOverrideError> {
        let descriptor_bytes =
            fs::read(&schema_override.descriptor_set_path).map_err(|source| {
                NmxcSchemaOverrideError::ReadDescriptor {
                    path: schema_override.descriptor_set_path.clone(),
                    source,
                }
            })?;

        let pool = DescriptorPool::decode(descriptor_bytes.as_slice())?;

        let (hello, hello_rpc_path) = resolve_method(&pool, &schema_override.hello_rpc_path)?;

        if hello.is_client_streaming() || hello.is_server_streaming() {
            return Err(invalid_descriptor(format!(
                "{} must be unary",
                schema_override.hello_rpc_path
            )));
        }

        let (subscribe, subscribe_rpc_path) =
            resolve_method(&pool, &schema_override.subscribe_rpc_path)?;

        if subscribe.is_client_streaming() || !subscribe.is_server_streaming() {
            return Err(invalid_descriptor(format!(
                "{} must accept one request and stream responses",
                schema_override.subscribe_rpc_path
            )));
        }

        let hello_request = build_hello_request(hello.input(), &nmxc.gateway_id)?;
        let hello_response = hello.output();

        validate_response_header(hello_response.clone())?;

        let subscribe_request =
            build_subscribe_request(subscribe.input(), &schema_override.subscribe_fields, nmxc)?;

        let response = subscribe.output();

        let acknowledgement = response.get_field(1).ok_or_else(|| {
            invalid_descriptor(format!(
                "{} must define subscription acknowledgement field 1",
                response.full_name()
            ))
        })?;

        let notification = acknowledgement.containing_oneof().ok_or_else(|| {
            invalid_descriptor(format!(
                "{}.{} must belong to a notification oneof",
                response.full_name(),
                acknowledgement.name()
            ))
        })?;

        let Kind::Message(acknowledgement) = acknowledgement.kind() else {
            return Err(invalid_descriptor(format!(
                "{}.{} must be a message",
                response.full_name(),
                acknowledgement.name()
            )));
        };

        validate_response_header(acknowledgement)?;

        Ok(Self {
            hello_request,
            hello_response,
            subscribe_request,
            response,
            notification,
            hello_rpc_path,
            subscribe_rpc_path,
            max_frame_size_bytes: schema_override.max_frame_size_bytes,
        })
    }

    /// Decodes a response frame and serializes its canonical protobuf JSON form.
    fn decode_notification(
        &self,
        raw: &Bytes,
    ) -> Result<DecodedNotification, NmxcSchemaOverrideError> {
        let message = DynamicMessage::decode(self.response.clone(), raw.clone())
            .map_err(NmxcSchemaOverrideError::DecodeNotification)?;

        let field = self
            .notification
            .fields()
            .find(|field| message.has_field(field));

        let json = serde_json::to_string(&message)
            .map_err(NmxcSchemaOverrideError::SerializeNotification)?;

        Ok(DecodedNotification {
            message,
            field,
            json,
        })
    }

    /// Completes the descriptor-defined Hello and starts the Subscribe stream.
    pub(crate) async fn open_stream(
        &self,
        channel: Channel,
        rpc_timeout: Duration,
    ) -> Result<Streaming<Bytes>, HealthError> {
        let mut grpc = tonic::client::Grpc::new(channel.clone())
            .max_decoding_message_size(self.max_frame_size_bytes);

        let hello = tokio::time::timeout(rpc_timeout, async {
            grpc.ready().await.map_err(service_not_ready)?;

            grpc.unary(
                tonic::Request::new(self.hello_request.clone()),
                self.hello_rpc_path.clone(),
                DynamicRpcCodec,
            )
            .await
        })
        .await
        .map_err(|_| nmxc_timeout_error("Hello", rpc_timeout))?
        .map_err(HealthError::NmxcStatus)?
        .into_inner();

        let hello =
            DynamicMessage::decode(self.hello_response.clone(), hello).map_err(|error| {
                HealthError::NmxcStatus(tonic::Status::internal(format!(
                    "NMX-C hello response decode failed: {error}"
                )))
            })?;

        check_dynamic_response_success(&hello, "Hello")?;

        let mut grpc =
            tonic::client::Grpc::new(channel).max_decoding_message_size(self.max_frame_size_bytes);

        let response = tokio::time::timeout(rpc_timeout, async {
            grpc.ready().await.map_err(service_not_ready)?;

            grpc.server_streaming(
                tonic::Request::new(self.subscribe_request.clone()),
                self.subscribe_rpc_path.clone(),
                DynamicRpcCodec,
            )
            .await
        })
        .await
        .map_err(|_| nmxc_timeout_error("Subscribe", rpc_timeout))?
        .map_err(HealthError::NmxcStatus)?;

        Ok(response.into_inner())
    }
}

/// Runtime configuration for the descriptor-driven NMX-C collector.
pub(crate) struct NmxcSchemaOverrideCollectorConfig {
    pub(crate) nmxc_config: NmxcCollectorOptions,
    pub(crate) tls_config: Option<MtlsProfileConfig>,
    pub(crate) schema_override: Arc<NmxcSchemaOverride>,
}

/// NMX-C collector whose protobuf contract is supplied at service startup.
pub(crate) struct NmxcSchemaOverrideCollector {
    endpoint_url: String,
    connect_timeout: Duration,
    rpc_timeout: Duration,
    tls_config: Option<MtlsProfileConfig>,
    schema_override: Arc<NmxcSchemaOverride>,
}

#[async_trait]
impl<B: Bmc + 'static> StreamingCollector<B> for NmxcSchemaOverrideCollector {
    type Config = NmxcSchemaOverrideCollectorConfig;

    fn new_runner(
        _bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let connect_timeout = config.nmxc_config.connect_timeout();
        let rpc_timeout = config.nmxc_config.rpc_timeout();
        let switch_connect_host = endpoint.switch_connect_host_for_uri();

        let endpoint_url = nmxc_endpoint_url(
            switch_connect_host.as_ref(),
            config.nmxc_config.grpc_port,
            config.tls_config.is_some(),
        );

        Ok(Self {
            endpoint_url,
            connect_timeout,
            rpc_timeout,
            tls_config: config.tls_config,
            schema_override: config.schema_override,
        })
    }

    async fn connect(&mut self) -> Result<StreamingConnectResult<'_>, HealthError> {
        let channel = nmxc_channel(
            &self.endpoint_url,
            self.connect_timeout,
            self.tls_config.as_ref(),
        )
        .await?;

        let mut stream = self
            .schema_override
            .open_stream(channel, self.rpc_timeout)
            .await?;

        let first = receive_initial_subscribe_item(&mut stream, self.rpc_timeout).await?;
        let initial = initial_schema_override_frame_to_event(first, &self.schema_override);
        let schema_override = self.schema_override.clone();

        let remaining = stream
            .map(move |item| schema_override_frame_to_event(item, &schema_override))
            .boxed();

        Ok(finalize_connection(initial, remaining))
    }

    fn collector_type(&self) -> &'static str {
        "nmxc"
    }
}

async fn receive_initial_subscribe_item(
    stream: &mut Streaming<Bytes>,
    rpc_timeout: Duration,
) -> Result<Bytes, HealthError> {
    tokio::time::timeout(rpc_timeout, stream.message())
        .await
        .map_err(|_| nmxc_timeout_error("initial Subscribe response", rpc_timeout))?
        .map_err(HealthError::NmxcStatus)?
        .ok_or_else(|| {
            HealthError::NmxcStatus(tonic::Status::unavailable(
                "NMX-C subscribe stream closed before its acknowledgement",
            ))
        })
}

fn finalize_connection(
    initial: Result<CollectorEvent, (CollectorEvent, HealthError)>,
    remaining: super::runtime::EventStream<'static>,
) -> StreamingConnectResult<'static> {
    let event = match initial {
        Ok(event) => event,
        Err((event, error)) => {
            return StreamingConnectResult::Failed {
                events: vec![event],
                error,
            };
        }
    };

    StreamingConnectResult::Connected(
        futures::stream::once(async { Ok(event) })
            .chain(remaining)
            .boxed(),
    )
}

fn initial_schema_override_frame_to_event(
    raw: Bytes,
    schema_override: &NmxcSchemaOverride,
) -> Result<CollectorEvent, (CollectorEvent, HealthError)> {
    let decoded = match schema_override.decode_notification(&raw) {
        Ok(decoded) => decoded,
        Err(error) => {
            let error = HealthError::NmxcStatus(tonic::Status::internal(format!(
                "NMX-C notification decode failed: {error}"
            )));

            return Err((schema_override_failure_log(&error), error));
        }
    };

    let Some(selected) = decoded.field.as_ref() else {
        return Err(initial_schema_override_failure(
            decoded,
            schema_override,
            HealthError::NmxcStatus(tonic::Status::failed_precondition(
                "NMX-C subscribe acknowledgement missing notification payload",
            )),
        ));
    };

    if selected.number() != 1 {
        let selected_name = selected.name().to_string();

        return Err(initial_schema_override_failure(
            decoded,
            schema_override,
            HealthError::NmxcStatus(tonic::Status::failed_precondition(format!(
                "NMX-C subscribe expected subscription acknowledgement field 1, received {}",
                selected_name
            ))),
        ));
    }

    let acknowledgement = decoded.message.get_field(selected);

    let Value::Message(acknowledgement) = acknowledgement.as_ref() else {
        return Err(initial_schema_override_failure(
            decoded,
            schema_override,
            HealthError::NmxcStatus(tonic::Status::failed_precondition(
                "NMX-C subscribe acknowledgement payload is not a message",
            )),
        ));
    };

    if let Err(error) = check_dynamic_response_success(acknowledgement, "Subscribe") {
        return Err(initial_schema_override_failure(
            decoded,
            schema_override,
            error,
        ));
    }

    Ok(schema_override_notification_to_log(
        &decoded,
        schema_override,
        LogSeverity::Info,
    ))
}

fn schema_override_frame_to_event(
    item: Result<Bytes, tonic::Status>,
    schema_override: &NmxcSchemaOverride,
) -> Result<CollectorEvent, HealthError> {
    let raw = item.map_err(HealthError::NmxcStatus)?;

    match schema_override.decode_notification(&raw) {
        Ok(decoded) => Ok(schema_override_notification_to_log(
            &decoded,
            schema_override,
            LogSeverity::Info,
        )),
        Err(error) => {
            let error = HealthError::NmxcStatus(tonic::Status::internal(format!(
                "NMX-C notification decode failed: {error}"
            )));

            // Tonic has already separated this payload from the following
            // gRPC messages, so a dynamic decode failure does not corrupt the
            // stream. Report the dropped notification and keep reading.
            Ok(schema_override_failure_log(&error))
        }
    }
}

fn schema_override_notification_to_log(
    decoded: &DecodedNotification,
    schema_override: &NmxcSchemaOverride,
    severity: LogSeverity,
) -> CollectorEvent {
    let notification = decoded
        .field
        .as_ref()
        .map_or("unrecognized", FieldDescriptor::name);

    let message_args = serde_json::json!([notification]).to_string();

    let body = format!("NMX-C schema-override notification received: {notification}");

    log_record(
        severity,
        body.clone(),
        vec![
            ("notification".into(), notification.to_string()),
            ("body".into(), body),
            (
                "message_id".into(),
                "CarbideHealth.1.0.NmxcSchemaOverrideNotification".to_string(),
            ),
            ("message_args".into(), message_args),
            (
                "protobuf.message_type".into(),
                schema_override.response.full_name().to_string(),
            ),
            (
                crate::sink::LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE.into(),
                decoded.json.clone(),
            ),
        ],
    )
}

fn initial_schema_override_failure(
    decoded: DecodedNotification,
    schema_override: &NmxcSchemaOverride,
    error: HealthError,
) -> (CollectorEvent, HealthError) {
    (
        schema_override_notification_to_log(&decoded, schema_override, LogSeverity::Error),
        error,
    )
}

fn schema_override_failure_log(error: &HealthError) -> CollectorEvent {
    let message = error.to_string();

    log_record(
        LogSeverity::Error,
        message.clone(),
        vec![
            ("notification".into(), "unrecognized".to_string()),
            ("body".into(), message),
        ],
    )
}

/// Descriptor-derived information extracted from one Subscribe response frame.
struct DecodedNotification {
    message: DynamicMessage,
    field: Option<FieldDescriptor>,
    json: String,
}

fn service_not_ready(error: tonic::transport::Error) -> tonic::Status {
    tonic::Status::unknown(format!("service was not ready: {error}"))
}

fn check_dynamic_response_success(
    message: &DynamicMessage,
    operation: &str,
) -> Result<(), HealthError> {
    let Some(header_field) = message.descriptor().get_field(1) else {
        return Err(dynamic_response_error(
            operation,
            "response is missing server header field 1",
        ));
    };

    if !message.has_field(&header_field) {
        return Err(dynamic_response_error(
            operation,
            "response omitted its server header",
        ));
    }

    let header = message.get_field(&header_field);

    let Value::Message(header) = header.as_ref() else {
        return Err(dynamic_response_error(
            operation,
            "server header field is not a message",
        ));
    };

    let Some(return_code_field) = header.descriptor().get_field(4) else {
        return Err(dynamic_response_error(
            operation,
            "server header is missing return code field 4",
        ));
    };

    let Kind::Enum(return_codes) = return_code_field.kind() else {
        return Err(dynamic_response_error(
            operation,
            "server return code field is not an enum",
        ));
    };

    let Some(success) = return_codes.get_value_by_name(NMX_C_SUCCESS_RETURN_CODE) else {
        return Err(dynamic_response_error(
            operation,
            "server return code enum has no success value",
        ));
    };

    let return_code = header.get_field(&return_code_field);

    let Value::EnumNumber(actual) = return_code.as_ref() else {
        return Err(dynamic_response_error(
            operation,
            "server return code is not an enum value",
        ));
    };

    if *actual != success.number() {
        let value = return_codes
            .get_value(*actual)
            .map_or_else(|| actual.to_string(), |value| value.name().to_string());

        return Err(dynamic_response_error(
            operation,
            format!("server returned {value}"),
        ));
    }

    Ok(())
}

fn dynamic_response_error(operation: &str, message: impl std::fmt::Display) -> HealthError {
    HealthError::NmxcStatus(tonic::Status::failed_precondition(format!(
        "NMX-C {operation} {message}"
    )))
}

fn resolve_method(
    pool: &DescriptorPool,
    path: &str,
) -> Result<(MethodDescriptor, PathAndQuery), NmxcSchemaOverrideError> {
    let path_and_query = PathAndQuery::try_from(path.to_string())
        .map_err(|error| invalid_descriptor(format!("invalid gRPC path {path:?}: {error}")))?;

    let Some((service_name, method_name)) = path.strip_prefix('/').and_then(|v| v.split_once('/'))
    else {
        return Err(invalid_descriptor(format!(
            "gRPC path {path:?} must have the form /package.Service/Method"
        )));
    };

    if service_name.is_empty() || method_name.is_empty() || method_name.contains('/') {
        return Err(invalid_descriptor(format!(
            "gRPC path {path:?} must have the form /package.Service/Method"
        )));
    }

    let service = pool
        .get_service_by_name(service_name)
        .ok_or_else(|| invalid_descriptor(format!("missing service {service_name}")))?;

    let method = service
        .methods()
        .find(|method| method.name() == method_name)
        .ok_or_else(|| {
            invalid_descriptor(format!("missing method {service_name}.{method_name}"))
        })?;

    Ok((method, path_and_query))
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum NmxcSchemaOverrideError {
    #[error("failed to read NMX-C descriptor set {path}")]
    ReadDescriptor {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to decode NMX-C descriptor set")]
    DecodeDescriptor(#[from] prost_reflect::DescriptorError),

    #[error("invalid NMX-C descriptor set: {0}")]
    InvalidDescriptor(String),

    #[error("invalid [collectors.nmxc.schema_override].subscribe_fields")]
    InvalidSubscribeRequest(#[source] serde_json::Error),

    #[error("failed to decode an NMX-C schema-override notification")]
    DecodeNotification(#[source] prost::DecodeError),

    #[error("failed to serialize an NMX-C schema-override notification")]
    SerializeNotification(#[source] serde_json::Error),
}

fn invalid_descriptor(message: String) -> NmxcSchemaOverrideError {
    NmxcSchemaOverrideError::InvalidDescriptor(message)
}

fn build_hello_request(
    descriptor: MessageDescriptor,
    gateway_id: &str,
) -> Result<DynamicMessage, NmxcSchemaOverrideError> {
    let mut request = DynamicMessage::new(descriptor);

    validate_request_oneofs(&request)?;

    set_field(
        &mut request,
        1,
        Kind::String,
        Value::String(gateway_id.to_string()),
    )?;

    // Fields 1-3 are the base NMX-C Hello contract. The supplied descriptor
    // may extend the schema but must preserve these request semantics.
    set_current_enum(&mut request, 2, NMX_C_CURRENT_MAJOR_VERSION)?;

    set_current_enum(&mut request, 3, NMX_C_CURRENT_MINOR_VERSION)?;

    Ok(request)
}

fn build_subscribe_request(
    descriptor: MessageDescriptor,
    configured: &serde_json::Map<String, serde_json::Value>,
    nmxc: &NmxcCollectorOptions,
) -> Result<DynamicMessage, NmxcSchemaOverrideError> {
    for field in descriptor
        .fields()
        .filter(|field| NMX_C_CORE_REQUEST_FIELDS.contains(&field.number()))
    {
        if configured.contains_key(field.name()) || configured.contains_key(field.json_name()) {
            return Err(invalid_descriptor(format!(
                "subscribe request field {} is owned by [collectors.nmxc] and must not be repeated in schema override config",
                field.json_name()
            )));
        }
    }

    let configured = serde_json::Value::Object(configured.clone());

    let mut request = DynamicMessage::deserialize(descriptor, configured.into_deserializer())
        .map_err(NmxcSchemaOverrideError::InvalidSubscribeRequest)?;

    validate_request_oneofs(&request)?;

    set_field(
        &mut request,
        1,
        Kind::String,
        Value::String(nmxc.gateway_id.clone()),
    )?;

    set_field(
        &mut request,
        2,
        Kind::Bool,
        Value::Bool(nmxc.notify_on_self_change),
    )?;

    set_field(
        &mut request,
        3,
        Kind::Uint32,
        Value::U32(nmxc.heartbeat_rate),
    )?;

    Ok(request)
}

fn validate_request_oneofs(request: &DynamicMessage) -> Result<(), NmxcSchemaOverrideError> {
    for oneof in request.descriptor().oneofs() {
        let Some(core) = oneof
            .fields()
            .find(|field| NMX_C_CORE_REQUEST_FIELDS.contains(&field.number()))
        else {
            continue;
        };

        // Core fields must coexist regardless of their configured values. A selected
        // extension must also survive the subsequent core field assignments.
        if let Some(conflict) = oneof.fields().find(|field| {
            field.number() != core.number()
                && (NMX_C_CORE_REQUEST_FIELDS.contains(&field.number()) || request.has_field(field))
        }) {
            return Err(invalid_descriptor(format!(
                "request fields {} and {} cannot coexist in oneof {}",
                core.full_name(),
                conflict.full_name(),
                oneof.full_name()
            )));
        }
    }

    Ok(())
}

fn validate_response_header(descriptor: MessageDescriptor) -> Result<(), NmxcSchemaOverrideError> {
    let Some(header) = descriptor.get_field(1) else {
        return Err(invalid_descriptor(format!(
            "{} must define server header field 1",
            descriptor.full_name()
        )));
    };

    let Kind::Message(header_message) = header.kind() else {
        return Err(invalid_descriptor(format!(
            "{}.{} must be a message",
            descriptor.full_name(),
            header.name()
        )));
    };

    let Some(return_code) = header_message.get_field(4) else {
        return Err(invalid_descriptor(format!(
            "{} must define return code field 4",
            header_message.full_name()
        )));
    };

    let Kind::Enum(return_codes) = return_code.kind() else {
        return Err(invalid_descriptor(format!(
            "{}.{} must be an enum",
            header_message.full_name(),
            return_code.name()
        )));
    };

    if return_codes
        .get_value_by_name(NMX_C_SUCCESS_RETURN_CODE)
        .is_none()
    {
        return Err(invalid_descriptor(format!(
            "{} must define {NMX_C_SUCCESS_RETURN_CODE}",
            return_codes.full_name()
        )));
    }

    Ok(())
}

fn set_current_enum(
    message: &mut DynamicMessage,
    number: u32,
    current_value: &str,
) -> Result<(), NmxcSchemaOverrideError> {
    let field = message.descriptor().get_field(number).ok_or_else(|| {
        invalid_descriptor(format!(
            "{} is missing field {number}",
            message.descriptor().full_name()
        ))
    })?;

    let Kind::Enum(enumeration) = field.kind() else {
        return Err(invalid_descriptor(format!(
            "{}.{} must be an enum",
            message.descriptor().full_name(),
            field.name()
        )));
    };

    let value = enumeration
        .get_value_by_name(current_value)
        .ok_or_else(|| {
            invalid_descriptor(format!(
                "{} is missing enum value {current_value}",
                enumeration.full_name()
            ))
        })?;

    message
        .try_set_field(&field, Value::EnumNumber(value.number()))
        .map_err(|error| invalid_descriptor(error.to_string()))
}

fn set_field(
    message: &mut DynamicMessage,
    number: u32,
    expected_kind: Kind,
    value: Value,
) -> Result<(), NmxcSchemaOverrideError> {
    let field = message.descriptor().get_field(number).ok_or_else(|| {
        invalid_descriptor(format!(
            "{} is missing field {number}",
            message.descriptor().full_name()
        ))
    })?;

    if field.kind() != expected_kind {
        return Err(invalid_descriptor(format!(
            "{}.{} has incompatible type {:?}",
            message.descriptor().full_name(),
            field.name(),
            field.kind()
        )));
    }

    message
        .try_set_field(&field, value)
        .map_err(|error| invalid_descriptor(error.to_string()))
}

/// Yields each NMX-C `Subscribe` response frame undecoded.
#[derive(Default)]
struct RawFrameDecoder;

impl Decoder for RawFrameDecoder {
    type Item = Bytes;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<Bytes>, Status> {
        let len = src.remaining();
        Ok(Some(src.copy_to_bytes(len)))
    }
}

/// Encodes a dynamic Subscribe request and leaves response frames undecoded.
struct DynamicRpcCodec;

impl Codec for DynamicRpcCodec {
    type Encode = DynamicMessage;
    type Decode = Bytes;
    type Encoder = ProstEncoder<DynamicMessage>;
    type Decoder = RawFrameDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        ProstEncoder::new(BufferSettings::default())
    }

    fn decoder(&mut self) -> Self::Decoder {
        RawFrameDecoder
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Case, Outcome, check_cases};
    use prost::Message;
    use prost_types::field_descriptor_proto::{Label, Type};
    use prost_types::{
        EnumValueDescriptorProto, FieldDescriptorProto, FileDescriptorSet, OneofDescriptorProto,
    };
    use tempfile::NamedTempFile;

    use super::*;

    fn override_descriptor_set() -> FileDescriptorSet {
        let mut descriptor_set = FileDescriptorSet::decode(rpc::REFLECTION_API_SERVICE_DESCRIPTOR)
            .expect("repository descriptor set should decode");

        let nmx_c = descriptor_set
            .file
            .iter_mut()
            .find(|file| file.name.as_deref() == Some("nmx_c.proto"))
            .expect("repository descriptor set should contain nmx_c.proto");

        let minor = nmx_c
            .enum_type
            .iter_mut()
            .find(|enumeration| enumeration.name.as_deref() == Some("ProtoMsgMinorVersion"))
            .expect("nmx_c.proto should contain its minor version enum");

        minor
            .value
            .iter_mut()
            .find(|value| value.name.as_deref() == Some(NMX_C_CURRENT_MINOR_VERSION))
            .expect("minor version enum should contain its current value")
            .name = Some("PROTO_MSG_MINOR_VERSION_REPLACED".to_string());

        minor.value.push(EnumValueDescriptorProto {
            name: Some(NMX_C_CURRENT_MINOR_VERSION.to_string()),
            number: Some(99),
            ..Default::default()
        });

        let subscribe = nmx_c
            .message_type
            .iter_mut()
            .find(|message| message.name.as_deref() == Some("SubscribeRequest"))
            .expect("nmx_c.proto should contain SubscribeRequest");

        subscribe.field.push(FieldDescriptorProto {
            name: Some("test_option".to_string()),
            json_name: Some("testOption".to_string()),
            number: Some(4),
            label: Some(Label::Optional as i32),
            r#type: Some(Type::Bool as i32),
            ..Default::default()
        });

        let notification = nmx_c
            .message_type
            .iter_mut()
            .find(|message| message.name.as_deref() == Some("ServerNotification"))
            .expect("nmx_c.proto should contain ServerNotification");

        notification.field.push(FieldDescriptorProto {
            name: Some("test_notification".to_string()),
            json_name: Some("testNotification".to_string()),
            number: Some(12),
            label: Some(Label::Optional as i32),
            r#type: Some(Type::Message as i32),
            type_name: Some(".nmx_c.SubscriptionResponse".to_string()),
            oneof_index: Some(0),
            ..Default::default()
        });

        descriptor_set
    }

    fn load_schema_override(
        descriptor_set: &FileDescriptorSet,
        subscribe_fields: serde_json::Value,
    ) -> Result<NmxcSchemaOverride, NmxcSchemaOverrideError> {
        let descriptor_file = NamedTempFile::new().expect("descriptor temp file should open");

        fs::write(descriptor_file.path(), descriptor_set.encode_to_vec())
            .expect("descriptor temp file should be writable");

        NmxcSchemaOverride::load(
            &NmxcSchemaOverrideConfig {
                descriptor_set_path: descriptor_file.path().to_path_buf(),
                hello_rpc_path: "/nmx_c.NMX_Controller/Hello".to_string(),
                subscribe_rpc_path: "/nmx_c.NMX_Controller/Subscribe".to_string(),
                max_frame_size_bytes: 4 * 1024 * 1024,
                subscribe_fields: subscribe_fields
                    .as_object()
                    .expect("test SubscribeRequest should be an object")
                    .clone(),
            },
            &NmxcCollectorOptions {
                gateway_id: "test-gateway".to_string(),
                notify_on_self_change: true,
                heartbeat_rate: 17,
                ..Default::default()
            },
        )
    }

    fn dynamic_response(descriptor: MessageDescriptor, return_code: &str) -> DynamicMessage {
        let mut response = DynamicMessage::new(descriptor);

        let header_field = response
            .descriptor()
            .get_field(1)
            .expect("test Hello response should contain its server header");

        let Kind::Message(header_descriptor) = header_field.kind() else {
            panic!("test server header should be a message");
        };

        let mut header = DynamicMessage::new(header_descriptor);

        let return_code_field = header
            .descriptor()
            .get_field(4)
            .expect("test server header should contain its return code");

        let Kind::Enum(return_codes) = return_code_field.kind() else {
            panic!("test return code should be an enum");
        };

        let return_code = return_codes
            .get_value_by_name(return_code)
            .expect("test return code should exist");

        header
            .try_set_field(&return_code_field, Value::EnumNumber(return_code.number()))
            .expect("test return code should be assignable");

        response
            .try_set_field(&header_field, Value::Message(header))
            .expect("test server header should be assignable");

        response
    }

    fn notification_frame(
        schema_override: &NmxcSchemaOverride,
        field_number: Option<u32>,
        return_code: &str,
    ) -> Bytes {
        let mut response = DynamicMessage::new(schema_override.response.clone());

        if let Some(field_number) = field_number {
            let field = response
                .descriptor()
                .get_field(field_number)
                .expect("test notification field should exist");

            let Kind::Message(payload) = field.kind() else {
                panic!("test notification payload should be a message");
            };

            let payload = if field_number == 1 {
                dynamic_response(payload, return_code)
            } else {
                DynamicMessage::new(payload)
            };

            response
                .try_set_field(&field, Value::Message(payload))
                .expect("test notification payload should be assignable");
        }

        response.encode_to_vec().into()
    }

    #[test]
    fn schema_override_builds_versioned_hello_and_configured_subscribe_requests() {
        let schema_override = load_schema_override(
            &override_descriptor_set(),
            serde_json::json!({ "testOption": true }),
        )
        .expect("schema override should load");

        let hello = &schema_override.hello_request;
        let subscribe = &schema_override.subscribe_request;

        assert_eq!(
            hello.get_field_by_number(1).as_deref(),
            Some(&Value::String("test-gateway".to_string()))
        );

        assert_eq!(
            hello.get_field_by_number(3).as_deref(),
            Some(&Value::EnumNumber(99))
        );

        assert_eq!(
            subscribe.get_field_by_number(1).as_deref(),
            Some(&Value::String("test-gateway".to_string()))
        );

        assert_eq!(
            subscribe.get_field_by_number(2).as_deref(),
            Some(&Value::Bool(true))
        );

        assert_eq!(
            subscribe.get_field_by_number(3).as_deref(),
            Some(&Value::U32(17))
        );

        assert_eq!(
            subscribe.get_field_by_number(4).as_deref(),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn dynamic_hello_validation_uses_the_configured_descriptor() {
        let schema_override = load_schema_override(
            &override_descriptor_set(),
            serde_json::json!({ "testOption": true }),
        )
        .expect("schema override should load");

        assert!(
            check_dynamic_response_success(
                &dynamic_response(
                    schema_override.hello_response.clone(),
                    NMX_C_SUCCESS_RETURN_CODE,
                ),
                "Hello",
            )
            .is_ok()
        );

        let error = check_dynamic_response_success(
            &dynamic_response(schema_override.hello_response, "NMX_ST_BADPARAM"),
            "Hello",
        )
        .expect_err("failed Hello response should be rejected");

        assert!(error.to_string().contains("NMX_ST_BADPARAM"));
    }

    #[test]
    fn schema_override_uses_configured_rpc_paths() {
        let mut descriptor_set = override_descriptor_set();

        let nmx_c = descriptor_set
            .file
            .iter_mut()
            .find(|file| file.name.as_deref() == Some("nmx_c.proto"))
            .expect("descriptor should contain nmx_c.proto");

        let service = nmx_c
            .service
            .first_mut()
            .expect("descriptor should contain the NMX-C service");

        service.name = Some("CustomController".to_string());
        service
            .method
            .iter_mut()
            .find(|method| method.name.as_deref() == Some("Hello"))
            .expect("service should contain Hello")
            .name = Some("Open".to_string());

        service
            .method
            .iter_mut()
            .find(|method| method.name.as_deref() == Some("Subscribe"))
            .expect("service should contain Subscribe")
            .name = Some("Watch".to_string());

        let descriptor_file = NamedTempFile::new().expect("descriptor temp file should open");
        fs::write(descriptor_file.path(), descriptor_set.encode_to_vec())
            .expect("descriptor temp file should be writable");

        let schema_override = NmxcSchemaOverride::load(
            &NmxcSchemaOverrideConfig {
                descriptor_set_path: descriptor_file.path().to_path_buf(),
                hello_rpc_path: "/nmx_c.CustomController/Open".to_string(),
                subscribe_rpc_path: "/nmx_c.CustomController/Watch".to_string(),
                max_frame_size_bytes: 8 * 1024 * 1024,
                subscribe_fields: serde_json::json!({ "testOption": true })
                    .as_object()
                    .expect("test SubscribeRequest should be an object")
                    .clone(),
            },
            &NmxcCollectorOptions::default(),
        )
        .expect("configured runtime methods should load");

        assert_eq!(
            schema_override.hello_rpc_path.as_str(),
            "/nmx_c.CustomController/Open"
        );

        assert_eq!(
            schema_override.subscribe_rpc_path.as_str(),
            "/nmx_c.CustomController/Watch"
        );

        assert_eq!(schema_override.max_frame_size_bytes, 8 * 1024 * 1024);
    }

    #[test]
    fn schema_override_decodes_configured_notification() {
        let schema_override = load_schema_override(
            &override_descriptor_set(),
            serde_json::json!({ "testOption": true }),
        )
        .expect("schema override should load");

        let field = schema_override
            .response
            .get_field(12)
            .expect("extended response should contain the added notification field");

        let Kind::Message(payload_descriptor) = field.kind() else {
            panic!("extended notification should be a message");
        };

        let mut response = DynamicMessage::new(schema_override.response.clone());

        response
            .try_set_field(
                &field,
                Value::Message(DynamicMessage::new(payload_descriptor)),
            )
            .expect("extended notification should accept its payload");

        let decoded = schema_override
            .decode_notification(&response.encode_to_vec().into())
            .expect("runtime response should decode");

        let selected = decoded
            .field
            .expect("runtime response should select its oneof field");

        assert_eq!(selected.number(), 12);
        assert_eq!(selected.name(), "test_notification");

        let decoded_json: serde_json::Value =
            serde_json::from_str(&decoded.json).expect("decoded notification should be JSON");

        assert_eq!(decoded_json["testNotification"], serde_json::json!({}));
    }

    #[test]
    fn invalid_initial_responses_preserve_decoded_payload_before_failure() {
        let schema_override = load_schema_override(
            &override_descriptor_set(),
            serde_json::json!({ "testOption": true }),
        )
        .expect("schema override should load");

        let cases = [
            (
                "rejected acknowledgement",
                notification_frame(&schema_override, Some(1), "NMX_ST_BADPARAM"),
            ),
            (
                "unexpected notification",
                notification_frame(&schema_override, Some(12), NMX_C_SUCCESS_RETURN_CODE),
            ),
            (
                "missing notification",
                notification_frame(&schema_override, None, NMX_C_SUCCESS_RETURN_CODE),
            ),
        ];

        for (name, frame) in cases {
            let Err((event, _error)) =
                initial_schema_override_frame_to_event(frame, &schema_override)
            else {
                panic!("{name} should fail after emitting its decoded response");
            };

            let CollectorEvent::Log(record) = event else {
                panic!("{name} should emit its decoded response first");
            };

            assert_eq!(record.severity, LogSeverity::Error, "{name}");

            assert!(record.attributes.iter().any(|(key, value)| {
                key.as_ref() == crate::sink::LogRecord::DECODED_PROTOBUF_PAYLOAD_ATTRIBUTE
                    && serde_json::from_str::<serde_json::Value>(value).is_ok()
            }));

            assert!(record.attributes.iter().any(|(key, value)| {
                key.as_ref() == "protobuf.message_type"
                    && value == schema_override.response.full_name()
            }));
        }
    }

    #[test]
    fn malformed_frames_fail_initial_connection_but_not_connected_stream() {
        let schema_override = load_schema_override(
            &override_descriptor_set(),
            serde_json::json!({ "testOption": true }),
        )
        .expect("schema override should load");

        let malformed = Bytes::from_static(&[0x0f]);

        let Err((event, _error)) =
            initial_schema_override_frame_to_event(malformed.clone(), &schema_override)
        else {
            panic!("malformed initial acknowledgement should fail the stream");
        };

        let CollectorEvent::Log(record) = event else {
            panic!("malformed initial acknowledgement should emit an error log");
        };

        assert_eq!(record.severity, LogSeverity::Error);

        let connected = schema_override_frame_to_event(Ok(malformed), &schema_override)
            .expect("connected stream decode failure should become a log");

        let CollectorEvent::Log(record) = connected else {
            panic!("connected stream decode failure should emit a log");
        };

        assert_eq!(record.severity, LogSeverity::Error);
    }

    #[test]
    fn schema_override_rejects_invalid_subscribe_acknowledgement_at_startup() {
        let mut descriptor_set = override_descriptor_set();

        let nmx_c = descriptor_set
            .file
            .iter_mut()
            .find(|file| file.name.as_deref() == Some("nmx_c.proto"))
            .expect("descriptor should contain nmx_c.proto");

        let acknowledgement = nmx_c
            .message_type
            .iter_mut()
            .find(|message| message.name.as_deref() == Some("SubscriptionResponse"))
            .expect("descriptor should contain the acknowledgement message");

        acknowledgement
            .field
            .retain(|field| field.number != Some(1));

        let error = load_schema_override(&descriptor_set, serde_json::json!({}))
            .expect_err("invalid acknowledgement should fail at startup");

        assert!(error.to_string().contains("server header field 1"));
    }

    #[test]
    fn schema_override_rejects_values_owned_by_nmxc_config() {
        let error = load_schema_override(
            &override_descriptor_set(),
            serde_json::json!({ "gatewayId": "duplicate" }),
        )
        .expect_err("runtime-owned field should be rejected");

        assert!(error.to_string().contains("owned by [collectors.nmxc]"));
    }

    #[test]
    fn schema_override_validates_request_oneof_conflicts() {
        let cases = [
            (
                "Subscribe core fields must coexist",
                "SubscribeRequest",
                vec![1, 2, 3],
                serde_json::json!({}),
                false,
            ),
            (
                "Hello core fields must coexist",
                "ClientHello",
                vec![1, 2],
                serde_json::json!({}),
                false,
            ),
            (
                "selected extension conflicts with a core field",
                "SubscribeRequest",
                vec![1, 4],
                serde_json::json!({"testOption": true}),
                false,
            ),
            (
                "default-valued extension still selects its oneof",
                "SubscribeRequest",
                vec![1, 4],
                serde_json::json!({"test_option": false}),
                false,
            ),
            (
                "omitted extension does not conflict",
                "SubscribeRequest",
                vec![1, 4],
                serde_json::json!({}),
                true,
            ),
            (
                "null extension does not select its oneof",
                "SubscribeRequest",
                vec![1, 4],
                serde_json::json!({"testOption": null}),
                true,
            ),
            (
                "unrelated extension oneof is preserved",
                "SubscribeRequest",
                vec![4],
                serde_json::json!({"testOption": true}),
                true,
            ),
        ]
        .map(|(scenario, message, fields, configured, succeeds)| Case {
            scenario,
            input: (message, fields, configured),
            expect: Outcome::Yields(succeeds),
        });

        check_cases(cases, |(message_name, field_numbers, configured)| {
            let mut descriptor_set = override_descriptor_set();

            let request = descriptor_set
                .file
                .iter_mut()
                .flat_map(|file| &mut file.message_type)
                .find(|message| message.name.as_deref() == Some(message_name))
                .expect("request message should exist");

            request.oneof_decl.push(OneofDescriptorProto {
                name: Some("request_choice".to_string()),
                ..Default::default()
            });

            for field in &mut request.field {
                if field
                    .number
                    .is_some_and(|number| field_numbers.contains(&number))
                {
                    field.oneof_index = Some(0);
                }
            }

            let expected_option = configured
                .get("testOption")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or_default();

            let result = load_schema_override(&descriptor_set, configured);

            match result {
                Ok(schema) => {
                    assert_eq!(
                        schema.subscribe_request.get_field_by_number(1).as_deref(),
                        Some(&Value::String("test-gateway".to_string()))
                    );

                    assert_eq!(
                        schema.subscribe_request.get_field_by_number(4).as_deref(),
                        Some(&Value::Bool(expected_option))
                    );

                    Ok(true)
                }
                Err(NmxcSchemaOverrideError::InvalidDescriptor(reason)) => {
                    assert!(reason.contains(message_name), "{reason}");
                    assert!(reason.contains("request_choice"), "{reason}");
                    assert!(reason.contains("cannot coexist"), "{reason}");

                    Ok(false)
                }
                Err(error) => Err(error.to_string()),
            }
        });
    }

    #[test]
    fn schema_override_preserves_optional_core_fields() {
        let mut descriptor_set = override_descriptor_set();

        let request = descriptor_set
            .file
            .iter_mut()
            .flat_map(|file| &mut file.message_type)
            .find(|message| message.name.as_deref() == Some("SubscribeRequest"))
            .expect("SubscribeRequest should exist");

        for (index, field) in request.field.iter_mut().take(3).enumerate() {
            request.oneof_decl.push(OneofDescriptorProto {
                name: Some(format!("_{}", field.name())),
                ..Default::default()
            });

            field.oneof_index = Some(index as i32);
            field.proto3_optional = Some(true);
        }

        let schema = load_schema_override(&descriptor_set, serde_json::json!({}))
            .expect("independent optional core fields should load");

        let request = schema.subscribe_request;

        assert_eq!(
            request.get_field_by_number(1).as_deref(),
            Some(&Value::String("test-gateway".to_string()))
        );

        assert_eq!(
            request.get_field_by_number(2).as_deref(),
            Some(&Value::Bool(true))
        );

        assert_eq!(
            request.get_field_by_number(3).as_deref(),
            Some(&Value::U32(17))
        );
    }

    #[test]
    fn schema_override_rejects_incompatible_subscribe_field_types() {
        let mut descriptor_set = override_descriptor_set();

        let nmx_c = descriptor_set
            .file
            .iter_mut()
            .find(|file| file.name.as_deref() == Some("nmx_c.proto"))
            .expect("descriptor should contain nmx_c.proto");

        let subscribe = nmx_c
            .message_type
            .iter_mut()
            .find(|message| message.name.as_deref() == Some("SubscribeRequest"))
            .expect("descriptor should contain SubscribeRequest");

        subscribe
            .field
            .iter_mut()
            .find(|field| field.number == Some(3))
            .expect("SubscribeRequest should contain field 3")
            .r#type = Some(Type::String as i32);

        let error = load_schema_override(&descriptor_set, serde_json::json!({}))
            .expect_err("incompatible runtime field should be rejected");

        assert!(error.to_string().contains("incompatible type"));
    }
}
