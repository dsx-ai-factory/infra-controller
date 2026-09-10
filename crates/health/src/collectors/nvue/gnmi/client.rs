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

use std::time::Duration;

use carbide_uuid::rack::RackId;
use tokio_stream::StreamExt;
use tonic::metadata::MetadataMap;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Extensions, Request};

use super::proto::g_nmi_client::GNmiClient as TonicGnmiClient;
use super::proto::subscription_list::Mode as SubscriptionListMode;
use super::proto::{
    self, Encoding, Path, PathElem, SubscribeRequest, Subscription, SubscriptionList,
    SubscriptionMode,
};
use crate::HealthError;
use crate::config::{
    MtlsProfileConfig, NvueGnmiEncoding, NvueGnmiPaths, NvueGnmiSubscriptionConfig,
    NvueGnmiSubscriptionMode,
};

pub(super) fn nvue_subscribe_paths(paths_config: &NvueGnmiPaths) -> Vec<Path> {
    let mut paths = Vec::with_capacity(5);

    if paths_config.components_enabled {
        paths.push(Path {
            elem: vec![
                PathElem {
                    name: "components".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "component".into(),
                    key: Default::default(),
                },
            ],
            ..Default::default()
        });
    }
    if paths_config.interfaces_enabled {
        paths.push(Path {
            elem: vec![
                PathElem {
                    name: "interfaces".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "interface".into(),
                    key: Default::default(),
                },
            ],
            ..Default::default()
        });
    }

    if paths_config.platform_general_enabled {
        // `/platform-general/state` carries memory, disk, and ambient
        // temperature leaves.
        paths.push(Path {
            elem: vec![
                PathElem {
                    name: "platform-general".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "state".into(),
                    key: Default::default(),
                },
            ],
            ..Default::default()
        });
        // `/platform-general/versions` carries the OS/BMC/EROT
        // firmware version leaves
        paths.push(Path {
            elem: vec![
                PathElem {
                    name: "platform-general".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "versions".into(),
                    key: Default::default(),
                },
            ],
            ..Default::default()
        });
    }

    if paths_config.leak_sensors_enabled {
        paths.push(Path {
            elem: vec![
                PathElem {
                    name: "platform-general".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "leak-sensors".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "leak-sensor".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "state".into(),
                    key: Default::default(),
                },
                PathElem {
                    name: "state".into(),
                    key: Default::default(),
                },
            ],
            ..Default::default()
        });
    }

    paths
}

#[derive(Clone)]
pub(super) struct GnmiClient {
    switch_id: String,
    rack_id: Option<RackId>,
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    request_timeout: Duration,
    dangerously_skip_tls_verification: bool,
    tls_config: Option<MtlsProfileConfig>,
}

/// Configuration used to build one gNMI client instance.
pub(super) struct GnmiClientConfig {
    /// Switch identifier used in logs and error messages.
    pub switch_id: String,

    /// Optional rack identifier added to endpoint-scoped logs.
    pub rack_id: Option<RackId>,

    /// Switch host or IP address used for the gNMI channel.
    pub host: String,

    /// gNMI TCP port on the switch host.
    pub port: u16,

    /// Optional username sent as gNMI `username` metadata.
    pub username: Option<String>,

    /// Optional password sent as gNMI `password` metadata.
    pub password: Option<String>,

    /// Timeout applied to gNMI connection and RPC operations.
    pub request_timeout: Duration,

    /// Whether legacy non-mTLS connections accept invalid switch certificates.
    pub dangerously_skip_tls_verification: bool,

    /// mTLS profile used when opening the gNMI channel.
    pub tls_config: Option<MtlsProfileConfig>,
}

async fn configure_tls_endpoint(
    endpoint: Endpoint,
    switch_id: &str,
    dangerously_skip_tls_verification: bool,
    tls_config: Option<&MtlsProfileConfig>,
) -> Result<Endpoint, HealthError> {
    if let Some(config) = tls_config {
        // mTLS config supplies both trust roots and client identity. Use it as
        // the complete TLS policy for this channel.
        let tls_config = crate::tls::tonic_tls_config(config).await?;

        return endpoint.tls_config(tls_config).map_err(|e| {
            HealthError::GnmiError(format!("switch {switch_id}: invalid gNMI TLS config: {e}"))
        });
    }

    if !dangerously_skip_tls_verification {
        return Ok(endpoint);
    }

    // Use tonic's verifier hook (https endpoints get a strict verifier
    // otherwise). No roots on ClientTlsConfig — roots + verifier is an error.
    endpoint
        .tls_config_with_verifier(
            ClientTlsConfig::new(),
            crate::collectors::nvue::tls::accept_any_cert_verifier(),
        )
        .map_err(|e| {
            HealthError::GnmiError(format!("switch {switch_id}: invalid gNMI TLS config: {e}"))
        })
}

impl GnmiClient {
    pub(super) fn new(config: GnmiClientConfig) -> Self {
        Self {
            switch_id: config.switch_id,
            rack_id: config.rack_id,
            host: config.host,
            port: config.port,
            username: config.username,
            password: config.password,
            request_timeout: config.request_timeout,
            dangerously_skip_tls_verification: config.dangerously_skip_tls_verification,
            tls_config: config.tls_config,
        }
    }

    async fn connect(&self) -> Result<TonicGnmiClient<Channel>, HealthError> {
        let target = format!("{}:{}", self.host, self.port);

        let uri = http::Uri::builder()
            .scheme("https")
            .authority(target.as_str())
            .path_and_query("/")
            .build()
            .map_err(|e| {
                HealthError::GnmiError(format!(
                    "switch {}: invalid endpoint URI: {e}",
                    self.switch_id
                ))
            })?;

        let endpoint = configure_tls_endpoint(
            Endpoint::from(uri),
            &self.switch_id,
            self.dangerously_skip_tls_verification,
            self.tls_config.as_ref(),
        )
        .await?
        .connect_timeout(self.request_timeout)
        .timeout(self.request_timeout);

        let channel = endpoint.connect().await.map_err(|e| {
            HealthError::GnmiError(format!(
                "switch {}: connection failed to {target}: {e}",
                self.switch_id
            ))
        })?;

        if self.dangerously_skip_tls_verification {
            tracing::debug!(
                switch_id = %self.switch_id,
                target = %target,
                rack_id = self.rack_id.as_ref().map(tracing::field::display),
                "gNMI TLS channel established with certificate verification disabled"
            );
        } else {
            tracing::debug!(
                switch_id = %self.switch_id,
                target = %target,
                rack_id = self.rack_id.as_ref().map(tracing::field::display),
                "gNMI TLS channel established"
            );
        }

        Ok(TonicGnmiClient::new(channel))
    }

    /// open a gNMI SAMPLE streaming subscription
    pub(super) async fn subscribe_sample(
        &self,
        paths: &[Path],
        sample_interval_nanos: u64,
    ) -> Result<tonic::Streaming<proto::SubscribeResponse>, HealthError> {
        let subscribe_request = build_sample_subscribe_request(paths, sample_interval_nanos);
        let response = self.subscribe_request(subscribe_request).await?;

        tracing::debug!(
            switch_id = %self.switch_id,
            sample_interval_nanoseconds = sample_interval_nanos,
            rack_id = self.rack_id.as_ref().map(tracing::field::display),
            "gNMI SAMPLE stream opened"
        );

        Ok(response)
    }

    /// open a gNMI ON_CHANGE streaming subscription
    pub(super) async fn subscribe_on_change(
        &self,
        prefix: &Path,
        paths: &[Path],
    ) -> Result<tonic::Streaming<proto::SubscribeResponse>, HealthError> {
        let subscribe_request = build_on_change_subscribe_request(prefix, paths);
        let response = self.subscribe_request(subscribe_request).await?;

        tracing::debug!(
            switch_id = %self.switch_id,
            rack_id = self.rack_id.as_ref().map(tracing::field::display),
            "gNMI ON_CHANGE stream opened"
        );

        Ok(response)
    }

    pub(super) async fn subscribe_request(
        &self,
        subscribe_request: SubscribeRequest,
    ) -> Result<tonic::Streaming<proto::SubscribeResponse>, HealthError> {
        let mut client = self.connect().await?;
        let auth = build_auth_metadata(&self.username, &self.password)?;
        let stream = tokio_stream::once(subscribe_request).chain(tokio_stream::pending());
        let request = Request::from_parts(auth, Extensions::default(), stream);

        let response = client
            .subscribe(request)
            .await
            .map_err(HealthError::GnmiStatus)?;

        Ok(response.into_inner())
    }
}

pub(crate) fn system_events_prefix() -> Path {
    Path {
        target: "nvos".to_string(),
        elem: vec![PathElem {
            name: "system-events".to_string(),
            key: Default::default(),
        }],
        ..Default::default()
    }
}

/// gNMI path for ON_CHANGE system event subscriptions. An empty path subscribes
/// to all events below the `system-events` prefix.
pub(crate) fn system_events_subscribe_path() -> Vec<Path> {
    vec![Path::default()]
}

fn build_on_change_subscribe_request(prefix: &Path, paths: &[Path]) -> SubscribeRequest {
    let subscription_list = SubscriptionList {
        prefix: Some(prefix.clone()),
        subscription: paths
            .iter()
            .map(|path| Subscription {
                path: Some(path.clone()),
                mode: SubscriptionMode::OnChange.into(),
                ..Default::default()
            })
            .collect(),
        mode: SubscriptionListMode::Stream.into(),
        encoding: Encoding::Json.into(),
        updates_only: true,
        ..Default::default()
    };

    SubscribeRequest {
        request: Some(proto::subscribe_request::Request::Subscribe(
            subscription_list,
        )),
        extension: vec![],
    }
}

fn build_sample_subscribe_request(paths: &[Path], sample_interval_nanos: u64) -> SubscribeRequest {
    let subscription_list = SubscriptionList {
        prefix: Some(Path {
            target: "nvos".to_string(),
            ..Default::default()
        }),
        subscription: paths
            .iter()
            .map(|path| Subscription {
                path: Some(path.clone()),
                mode: SubscriptionMode::Sample.into(),
                sample_interval: sample_interval_nanos,
                ..Default::default()
            })
            .collect(),
        mode: SubscriptionListMode::Stream.into(),
        encoding: Encoding::Json.into(),
        ..Default::default()
    };

    SubscribeRequest {
        request: Some(proto::subscribe_request::Request::Subscribe(
            subscription_list,
        )),
        extension: vec![],
    }
}

/// Builds a fixed-STREAM request from one validated additional subscription.
pub(super) fn build_extended_subscribe_request(
    config: &NvueGnmiSubscriptionConfig,
) -> Result<SubscribeRequest, HealthError> {
    let prefix = Path {
        origin: config.origin.clone(),
        elem: path_elements(&config.prefix),
        target: config.target.clone(),
        ..Default::default()
    };

    let subscription = config
        .paths
        .iter()
        .map(|path| {
            Ok(Subscription {
                path: Some(Path {
                    elem: path_elements(path),
                    ..Default::default()
                }),
                mode: match config.mode {
                    NvueGnmiSubscriptionMode::TargetDefined => SubscriptionMode::TargetDefined,
                    NvueGnmiSubscriptionMode::OnChange => SubscriptionMode::OnChange,
                    NvueGnmiSubscriptionMode::Sample => SubscriptionMode::Sample,
                }
                .into(),
                sample_interval: duration_nanos(
                    config.sample_interval,
                    "sample_interval",
                    &config.name,
                )?,
                suppress_redundant: config.suppress_redundant,
                heartbeat_interval: duration_nanos(
                    config.heartbeat_interval,
                    "heartbeat_interval",
                    &config.name,
                )?,
            })
        })
        .collect::<Result<Vec<_>, HealthError>>()?;

    Ok(SubscribeRequest {
        request: Some(proto::subscribe_request::Request::Subscribe(
            SubscriptionList {
                prefix: Some(prefix),
                subscription,
                mode: SubscriptionListMode::Stream.into(),
                encoding: match config.encoding {
                    NvueGnmiEncoding::Json => Encoding::Json,
                    NvueGnmiEncoding::Ascii => Encoding::Ascii,
                    NvueGnmiEncoding::JsonIetf => Encoding::JsonIetf,
                }
                .into(),
                updates_only: config.updates_only,
                ..Default::default()
            },
        )),
        extension: Vec::new(),
    })
}

fn path_elements(elements: &[String]) -> Vec<PathElem> {
    elements
        .iter()
        .map(|name| PathElem {
            name: name.clone(),
            key: Default::default(),
        })
        .collect()
}

fn duration_nanos(
    duration: Option<Duration>,
    field: &str,
    subscription_name: &str,
) -> Result<u64, HealthError> {
    let Some(duration) = duration else {
        return Ok(0);
    };

    u64::try_from(duration.as_nanos()).map_err(|_| {
        HealthError::GnmiError(format!(
            "extended gNMI subscription {subscription_name:?} {field} does not fit in u64 nanoseconds"
        ))
    })
}

fn build_auth_metadata(
    username: &Option<String>,
    password: &Option<String>,
) -> Result<MetadataMap, HealthError> {
    let mut meta = MetadataMap::new();
    if let Some(username) = username {
        let value = username.parse().map_err(|e| {
            HealthError::GnmiError(format!("invalid username for gRPC metadata: {e}"))
        })?;
        meta.insert("username", value);
    }
    if let Some(password) = password {
        let value = password
            .parse()
            .map_err(|_e| HealthError::GnmiError("invalid password for gRPC metadata".into()))?;
        meta.insert("password", value);
    }
    Ok(meta)
}

/// Extract a string from a `TypedValue`, handling JSON-encoded bytes as well
/// as native string values.
#[allow(deprecated)]
pub(super) fn typed_value_to_string(val: &proto::TypedValue) -> Option<String> {
    use proto::typed_value::Value;
    match &val.value {
        Some(Value::StringVal(s)) => Some(s.clone()),
        Some(Value::JsonVal(bytes)) | Some(Value::JsonIetfVal(bytes)) => {
            let s = String::from_utf8_lossy(bytes);
            let trimmed = s.trim().trim_matches('"');
            Some(trimmed.to_string())
        }
        Some(Value::AsciiVal(s)) => Some(s.clone()),
        Some(Value::IntVal(v)) => Some(v.to_string()),
        Some(Value::UintVal(v)) => Some(v.to_string()),
        Some(Value::BoolVal(v)) => Some(v.to_string()),
        Some(Value::FloatVal(v)) => Some(v.to_string()),
        Some(Value::DoubleVal(v)) => Some(v.to_string()),
        _ => None,
    }
}

/// Extract a float from a `TypedValue`, handling JSON-encoded bytes, native
/// numeric values, and string representations.
#[allow(deprecated)]
pub(super) fn typed_value_to_f64(val: &proto::TypedValue) -> Option<f64> {
    use proto::typed_value::Value;
    match &val.value {
        Some(Value::DoubleVal(v)) => Some(*v),
        Some(Value::FloatVal(v)) => Some(*v as f64),
        Some(Value::IntVal(v)) => Some(*v as f64),
        Some(Value::UintVal(v)) => Some(*v as f64),
        Some(Value::StringVal(s)) => s.parse().ok(),
        Some(Value::JsonVal(bytes)) | Some(Value::JsonIetfVal(bytes)) => {
            let s = String::from_utf8_lossy(bytes);
            s.trim().trim_matches('"').parse().ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[derive(Debug, PartialEq)]
    enum AuthProjection {
        Metadata {
            username: Option<String>,
            password: Option<String>,
        },
        InvalidUsername,
        InvalidPassword,
    }

    #[derive(Debug, PartialEq)]
    struct ValueProjection {
        as_string: Option<String>,
        as_f64: Option<f64>,
    }

    struct AuthInput {
        username: Option<String>,
        password: Option<String>,
    }

    #[test]
    #[allow(deprecated)]
    fn typed_value_projection_cases() {
        use proto::typed_value::Value;

        check_values(
            [
                Check {
                    scenario: "absent value",
                    input: proto::TypedValue { value: None },
                    expect: ValueProjection {
                        as_string: None,
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "native non-numeric string",
                    input: proto::TypedValue {
                        value: Some(Value::StringVal("healthy".to_string())),
                    },
                    expect: ValueProjection {
                        as_string: Some("healthy".to_string()),
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "native numeric string",
                    input: proto::TypedValue {
                        value: Some(Value::StringVal("1.23".to_string())),
                    },
                    expect: ValueProjection {
                        as_string: Some("1.23".to_string()),
                        as_f64: Some(1.23),
                    },
                },
                Check {
                    scenario: "quoted non-numeric JSON",
                    input: proto::TypedValue {
                        value: Some(Value::JsonVal(b"\"degraded\"".to_vec())),
                    },
                    expect: ValueProjection {
                        as_string: Some("degraded".to_string()),
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "quoted numeric JSON",
                    input: proto::TypedValue {
                        value: Some(Value::JsonVal(b"\"1.5e-3\"".to_vec())),
                    },
                    expect: ValueProjection {
                        as_string: Some("1.5e-3".to_string()),
                        as_f64: Some(0.0015),
                    },
                },
                Check {
                    scenario: "unquoted numeric JSON",
                    input: proto::TypedValue {
                        value: Some(Value::JsonVal(b"99.9".to_vec())),
                    },
                    expect: ValueProjection {
                        as_string: Some("99.9".to_string()),
                        as_f64: Some(99.9),
                    },
                },
                Check {
                    scenario: "IETF JSON",
                    input: proto::TypedValue {
                        value: Some(Value::JsonIetfVal(b"6.25".to_vec())),
                    },
                    expect: ValueProjection {
                        as_string: Some("6.25".to_string()),
                        as_f64: Some(6.25),
                    },
                },
                Check {
                    scenario: "ASCII",
                    input: proto::TypedValue {
                        value: Some(Value::AsciiVal("port-up".to_string())),
                    },
                    expect: ValueProjection {
                        as_string: Some("port-up".to_string()),
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "signed integer",
                    input: proto::TypedValue {
                        value: Some(Value::IntVal(-5)),
                    },
                    expect: ValueProjection {
                        as_string: Some("-5".to_string()),
                        as_f64: Some(-5.0),
                    },
                },
                Check {
                    scenario: "unsigned integer",
                    input: proto::TypedValue {
                        value: Some(Value::UintVal(100)),
                    },
                    expect: ValueProjection {
                        as_string: Some("100".to_string()),
                        as_f64: Some(100.0),
                    },
                },
                Check {
                    scenario: "Boolean",
                    input: proto::TypedValue {
                        value: Some(Value::BoolVal(true)),
                    },
                    expect: ValueProjection {
                        as_string: Some("true".to_string()),
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "float",
                    input: proto::TypedValue {
                        value: Some(Value::FloatVal(1.25)),
                    },
                    expect: ValueProjection {
                        as_string: Some("1.25".to_string()),
                        as_f64: Some(1.25),
                    },
                },
                Check {
                    scenario: "double",
                    input: proto::TypedValue {
                        value: Some(Value::DoubleVal(42.5)),
                    },
                    expect: ValueProjection {
                        as_string: Some("42.5".to_string()),
                        as_f64: Some(42.5),
                    },
                },
                Check {
                    scenario: "unsupported bytes",
                    input: proto::TypedValue {
                        value: Some(Value::BytesVal(vec![0, 1])),
                    },
                    expect: ValueProjection {
                        as_string: None,
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "unsupported decimal",
                    input: proto::TypedValue {
                        value: Some(Value::DecimalVal(proto::Decimal64 {
                            digits: 125,
                            precision: 2,
                        })),
                    },
                    expect: ValueProjection {
                        as_string: None,
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "unsupported leaf list",
                    input: proto::TypedValue {
                        value: Some(Value::LeaflistVal(proto::ScalarArray {
                            element: Vec::new(),
                        })),
                    },
                    expect: ValueProjection {
                        as_string: None,
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "unsupported arbitrary protobuf",
                    input: proto::TypedValue {
                        value: Some(Value::AnyVal(prost_types::Any {
                            type_url: "type.googleapis.com/example.Value".to_string(),
                            value: vec![0, 1],
                        })),
                    },
                    expect: ValueProjection {
                        as_string: None,
                        as_f64: None,
                    },
                },
                Check {
                    scenario: "unsupported protobuf bytes",
                    input: proto::TypedValue {
                        value: Some(Value::ProtoBytes(vec![0, 1])),
                    },
                    expect: ValueProjection {
                        as_string: None,
                        as_f64: None,
                    },
                },
            ],
            |value| ValueProjection {
                as_string: typed_value_to_string(&value),
                as_f64: typed_value_to_f64(&value),
            },
        );
    }

    #[test]
    fn auth_metadata_cases() {
        check_values(
            [
                Check {
                    scenario: "no credentials",
                    input: AuthInput {
                        username: None,
                        password: None,
                    },
                    expect: AuthProjection::Metadata {
                        username: None,
                        password: None,
                    },
                },
                Check {
                    scenario: "username only",
                    input: AuthInput {
                        username: Some("admin".to_string()),
                        password: None,
                    },
                    expect: AuthProjection::Metadata {
                        username: Some("admin".to_string()),
                        password: None,
                    },
                },
                Check {
                    scenario: "password only",
                    input: AuthInput {
                        username: None,
                        password: Some("secret".to_string()),
                    },
                    expect: AuthProjection::Metadata {
                        username: None,
                        password: Some("secret".to_string()),
                    },
                },
                Check {
                    scenario: "username and password",
                    input: AuthInput {
                        username: Some("admin".to_string()),
                        password: Some("secret".to_string()),
                    },
                    expect: AuthProjection::Metadata {
                        username: Some("admin".to_string()),
                        password: Some("secret".to_string()),
                    },
                },
                Check {
                    scenario: "invalid username",
                    input: AuthInput {
                        username: Some("bad\nusername".to_string()),
                        password: Some("secret".to_string()),
                    },
                    expect: AuthProjection::InvalidUsername,
                },
                Check {
                    scenario: "invalid password is redacted",
                    input: AuthInput {
                        username: Some("admin".to_string()),
                        password: Some("bad\npassword".to_string()),
                    },
                    expect: AuthProjection::InvalidPassword,
                },
            ],
            |AuthInput { username, password }| match build_auth_metadata(&username, &password) {
                Ok(metadata) => {
                    let get = |key| {
                        metadata.get(key).map(|value| {
                            value
                                .to_str()
                                .expect("test metadata should be visible ASCII")
                                .to_string()
                        })
                    };
                    AuthProjection::Metadata {
                        username: get("username"),
                        password: get("password"),
                    }
                }
                Err(HealthError::GnmiError(message))
                    if message.starts_with("invalid username for gRPC metadata:") =>
                {
                    AuthProjection::InvalidUsername
                }
                Err(HealthError::GnmiError(message))
                    if message == "invalid password for gRPC metadata" =>
                {
                    AuthProjection::InvalidPassword
                }
                Err(error) => panic!("unexpected authentication metadata error: {error}"),
            },
        );
    }

    #[test]
    fn nvue_subscribe_path_cases() {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        enum Group {
            Components,
            Interfaces,
            PlatformGeneral,
            LeakSensors,
        }

        use Group::{Components, Interfaces, LeakSensors, PlatformGeneral};

        const COMPONENTS: &str = "components/component";
        const INTERFACES: &str = "interfaces/interface";
        const PLATFORM_STATE: &str = "platform-general/state";
        const PLATFORM_VERSIONS: &str = "platform-general/versions";
        const LEAKS: &str = "platform-general/leak-sensors/leak-sensor/state/state";

        let cases = [
            &[][..],
            &[Components][..],
            &[Interfaces][..],
            &[Components, Interfaces][..],
            &[PlatformGeneral][..],
            &[Components, PlatformGeneral][..],
            &[Interfaces, PlatformGeneral][..],
            &[Components, Interfaces, PlatformGeneral][..],
            &[LeakSensors][..],
            &[Components, LeakSensors][..],
            &[Interfaces, LeakSensors][..],
            &[Components, Interfaces, LeakSensors][..],
            &[PlatformGeneral, LeakSensors][..],
            &[Components, PlatformGeneral, LeakSensors][..],
            &[Interfaces, PlatformGeneral, LeakSensors][..],
            &[Components, Interfaces, PlatformGeneral, LeakSensors][..],
        ];

        for groups in cases {
            let config = NvueGnmiPaths {
                components_enabled: groups.contains(&Components),
                interfaces_enabled: groups.contains(&Interfaces),
                platform_general_enabled: groups.contains(&PlatformGeneral),
                leak_sensors_enabled: groups.contains(&LeakSensors),
            };

            let actual = nvue_subscribe_paths(&config)
                .into_iter()
                .map(|path| {
                    path.elem
                        .into_iter()
                        .map(|elem| elem.name)
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .collect::<Vec<_>>();

            let expected = groups
                .iter()
                .flat_map(|group| match group {
                    Components => &[COMPONENTS][..],
                    Interfaces => &[INTERFACES][..],
                    PlatformGeneral => &[PLATFORM_STATE, PLATFORM_VERSIONS][..],
                    LeakSensors => &[LEAKS][..],
                })
                .copied()
                .collect::<Vec<_>>();

            assert_eq!(actual, expected, "enabled groups: {groups:?}");
        }
    }

    #[test]
    fn test_build_sample_subscribe_request() {
        let paths = nvue_subscribe_paths(&NvueGnmiPaths::default());
        let interval_nanos = 300_000_000_000u64;

        let req = build_sample_subscribe_request(&paths, interval_nanos);

        let sub_list = match req.request {
            Some(proto::subscribe_request::Request::Subscribe(sl)) => sl,
            _ => panic!("expected Subscribe variant"),
        };

        assert_eq!(
            sub_list.mode,
            i32::from(SubscriptionListMode::Stream),
            "must use Stream mode for SAMPLE subscriptions"
        );
        assert_eq!(
            sub_list.encoding,
            i32::from(Encoding::Json),
            "encoding must be JSON"
        );

        let prefix = sub_list.prefix.expect("prefix must be set");
        assert_eq!(prefix.target, "nvos", "target must be nvos");

        assert_eq!(sub_list.subscription.len(), 4);
        for sub in &sub_list.subscription {
            assert_eq!(
                sub.mode,
                i32::from(SubscriptionMode::Sample),
                "each subscription must use Sample mode"
            );
            assert_eq!(
                sub.sample_interval, interval_nanos,
                "sample_interval must match the requested interval"
            );
            assert!(sub.path.is_some(), "each subscription must have a path");
        }
    }

    #[test]
    fn test_build_on_change_subscribe_request() {
        let prefix = system_events_prefix();
        let paths = system_events_subscribe_path();

        let req = build_on_change_subscribe_request(&prefix, &paths);

        let sub_list = match req.request {
            Some(proto::subscribe_request::Request::Subscribe(sl)) => sl,
            _ => panic!("expected Subscribe variant"),
        };

        assert_eq!(
            sub_list.mode,
            i32::from(SubscriptionListMode::Stream),
            "must use Stream mode"
        );
        assert_eq!(
            sub_list.encoding,
            i32::from(Encoding::Json),
            "encoding must be JSON"
        );
        assert!(sub_list.updates_only, "ON_CHANGE must use updates_only");

        let req_prefix = sub_list.prefix.expect("prefix must be set");
        assert_eq!(req_prefix.target, "nvos");
        assert_eq!(req_prefix.elem.len(), 1);
        assert_eq!(req_prefix.elem[0].name, "system-events");

        assert_eq!(sub_list.subscription.len(), 1);
        assert_eq!(
            sub_list.subscription[0].mode,
            i32::from(SubscriptionMode::OnChange),
            "subscription must use OnChange mode"
        );
        assert!(
            sub_list.subscription[0]
                .path
                .as_ref()
                .is_some_and(|path| path.elem.is_empty()),
            "empty path subscribes to all events under prefix"
        );
    }

    #[test]
    fn extended_subscribe_request_preserves_subscription_contract() {
        let config = NvueGnmiSubscriptionConfig {
            name: "external_metrics".to_string(),
            target: "switch".to_string(),
            origin: "openconfig".to_string(),
            prefix: vec!["interfaces".to_string()],
            encoding: NvueGnmiEncoding::JsonIetf,
            updates_only: true,
            mode: NvueGnmiSubscriptionMode::Sample,
            sample_interval: Some(Duration::from_secs(10)),
            suppress_redundant: true,
            heartbeat_interval: Some(Duration::from_secs(60)),
            paths: vec![
                vec!["interface".to_string(), "state".to_string()],
                vec!["interface".to_string(), "counter".to_string()],
            ],
            metrics: Vec::new(),
        };

        let request = build_extended_subscribe_request(&config)
            .expect("validated extended request should build");

        let Some(proto::subscribe_request::Request::Subscribe(list)) = request.request else {
            panic!("extended request should contain a subscription list");
        };

        assert_eq!(list.mode, i32::from(SubscriptionListMode::Stream));
        assert_eq!(list.encoding, i32::from(Encoding::JsonIetf));
        assert!(list.updates_only);

        let prefix = list
            .prefix
            .expect("extended request should contain a prefix");

        assert_eq!(prefix.target, "switch");
        assert_eq!(prefix.origin, "openconfig");
        assert_eq!(prefix.elem[0].name, "interfaces");

        assert_eq!(list.subscription.len(), 2);

        for subscription in &list.subscription {
            assert_eq!(subscription.mode, i32::from(SubscriptionMode::Sample));
            assert_eq!(subscription.sample_interval, 10_000_000_000);
            assert!(subscription.suppress_redundant);
            assert_eq!(subscription.heartbeat_interval, 60_000_000_000);
        }

        assert!(list.subscription.iter().all(|subscription| {
            subscription
                .path
                .as_ref()
                .is_some_and(|path| path.elem.iter().all(|element| element.key.is_empty()))
        }));
    }
}
