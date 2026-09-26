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

use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use carbide_uuid::rack::RackId;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
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

const GNMI_HTTP2_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(300);
const GNMI_HTTP2_KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(30);

/// Owns both directions of a Subscribe RPC until the subscription is dropped.
pub(super) struct GnmiSubscription {
    // Drop the request sender before the response stream so the server can
    // finish a request whose response direction was abandoned.
    _request_sender: mpsc::Sender<SubscribeRequest>,
    responses: tonic::Streaming<proto::SubscribeResponse>,
}

impl Deref for GnmiSubscription {
    type Target = tonic::Streaming<proto::SubscribeResponse>;

    fn deref(&self) -> &Self::Target {
        &self.responses
    }
}

impl DerefMut for GnmiSubscription {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.responses
    }
}

impl Stream for GnmiSubscription {
    type Item = Result<proto::SubscribeResponse, tonic::Status>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.get_mut().responses).poll_next(cx)
    }
}

/// Builds the paths for the primary NVUE gNMI SAMPLE stream.
///
/// A configured interface selection uses another stream, so the primary stream
/// must omit the broad interface subtree or it would still receive counters.
pub(super) fn nvue_subscribe_paths(paths_config: &NvueGnmiPaths) -> Vec<Path> {
    let mut paths = Vec::with_capacity(4);

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

    if paths_config.interfaces_enabled && paths_config.interface_paths.is_none() {
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

    paths
}

/// Builds exact interface leaves for the optional independent SAMPLE stream.
///
/// The unkeyed `interface` element selects each leaf for every interface.
pub(super) fn nvue_interface_subscribe_paths(paths_config: &NvueGnmiPaths) -> Vec<Path> {
    paths_config
        .interface_paths
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|tail| Path {
            elem: [
                PathElem {
                    name: "interfaces".into(),
                    ..Default::default()
                },
                PathElem {
                    name: "interface".into(),
                    ..Default::default()
                },
            ]
            .into_iter()
            .chain(path_elements(tail))
            .collect(),
            ..Default::default()
        })
        .collect()
}

/// Builds the path for the independent leak-sensor SAMPLE stream.
pub(super) fn nvue_leak_sensor_subscribe_path() -> Path {
    Path {
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
    }
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

    /// Timeout applied independently to connection establishment and RPC opening.
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
        .timeout(self.request_timeout)
        // Periodic HTTP/2 PINGs make transport loss observable when a peer
        // stops acknowledging frames without closing the Subscribe stream.
        .http2_keep_alive_interval(GNMI_HTTP2_KEEPALIVE_INTERVAL)
        .keep_alive_timeout(GNMI_HTTP2_KEEPALIVE_TIMEOUT)
        .keep_alive_while_idle(true);

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
    ) -> Result<GnmiSubscription, HealthError> {
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
    ) -> Result<GnmiSubscription, HealthError> {
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
    ) -> Result<GnmiSubscription, HealthError> {
        let mut client = self.connect().await?;
        let auth = build_auth_metadata(&self.username, &self.password)?;
        let (request_sender, request_receiver) = mpsc::channel(1);

        // Keep the request body open while responses arrive. Dropping the
        // subscription drops the sender, ending the abandoned request body.
        let stream =
            tokio_stream::once(subscribe_request).chain(ReceiverStream::new(request_receiver));

        let request = Request::from_parts(auth, Extensions::default(), stream);

        let response = client
            .subscribe(request)
            .await
            .map_err(HealthError::GnmiStatus)?;

        Ok(GnmiSubscription {
            _request_sender: request_sender,
            responses: response.into_inner(),
        })
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
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};

    use carbide_test_support::{Check, check_values};
    use prometheus::{Counter, Gauge, Histogram, HistogramOpts, IntGauge};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_stream::wrappers::TcpListenerStream;
    use tonic::transport::server::Connected;
    use tonic::transport::{Identity, Server, ServerTlsConfig};

    use super::*;
    use crate::collectors::nvue::gnmi::sample_processor::{
        GnmiSampleProcessor, NVUE_GNMI_SAMPLE_STREAM_ID,
    };
    use crate::collectors::nvue::gnmi::subscriber::GnmiStreamMetrics;
    use crate::endpoint::test_support::test_endpoint;
    use crate::otlp::convert::build_metrics_export_request;
    use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

    #[derive(Default)]
    struct RecordingMetricSink(StdMutex<Vec<(EventContext, MetricSample)>>);

    impl DataSink for RecordingMetricSink {
        fn sink_type(&self) -> &'static str {
            "recording_metric"
        }

        fn try_handle_event(
            &self,
            context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            if let CollectorEvent::Metric(sample) = event {
                self.0
                    .lock()
                    .expect("recording sink lock")
                    .push((context.clone(), (**sample).clone()));
            }

            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct TestGnmiService {
        synchronized: Arc<AtomicBool>,
        selected_updates: Arc<AtomicBool>,
        active_requests: Arc<AtomicUsize>,
        requested_paths: Arc<StdMutex<Vec<String>>>,
    }

    #[tonic::async_trait]
    impl proto::g_nmi_server::GNmi for TestGnmiService {
        async fn capabilities(
            &self,
            _request: tonic::Request<proto::CapabilityRequest>,
        ) -> Result<tonic::Response<proto::CapabilityResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("capabilities"))
        }

        async fn get(
            &self,
            _request: tonic::Request<proto::GetRequest>,
        ) -> Result<tonic::Response<proto::GetResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("get"))
        }

        async fn set(
            &self,
            _request: tonic::Request<proto::SetRequest>,
        ) -> Result<tonic::Response<proto::SetResponse>, tonic::Status> {
            Err(tonic::Status::unimplemented("set"))
        }

        type SubscribeStream = ReceiverStream<Result<proto::SubscribeResponse, tonic::Status>>;

        async fn subscribe(
            &self,
            request: tonic::Request<tonic::Streaming<proto::SubscribeRequest>>,
        ) -> Result<tonic::Response<Self::SubscribeStream>, tonic::Status> {
            let mut requests = request.into_inner();

            let Some(initial) = requests.message().await? else {
                return Err(tonic::Status::invalid_argument("missing subscription"));
            };

            let paths = match initial.request {
                Some(proto::subscribe_request::Request::Subscribe(list)) => list
                    .subscription
                    .into_iter()
                    .filter_map(|subscription| subscription.path)
                    .collect::<Vec<_>>(),
                _ => return Err(tonic::Status::invalid_argument("missing subscription list")),
            };

            *self.requested_paths.lock().expect("requested path lock") = paths
                .iter()
                .map(|path| {
                    path.elem
                        .iter()
                        .map(|element| element.name.as_str())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .collect();

            let (responses, receiver) = mpsc::channel(4);
            self.active_requests.fetch_add(1, Ordering::SeqCst);

            if self.selected_updates.load(Ordering::SeqCst) {
                send_selected_updates(paths, &responses).await;
            }

            let active_requests = self.active_requests.clone();

            // Keep the server RPC alive until the client ends its request body.
            tokio::spawn(async move {
                while let Ok(Some(_)) = requests.message().await {}
                active_requests.fetch_sub(1, Ordering::SeqCst);
                drop(responses);
            });

            if self.selected_updates.load(Ordering::SeqCst) {
                return Ok(tonic::Response::new(ReceiverStream::new(receiver)));
            }

            if self.synchronized.load(Ordering::SeqCst) {
                let (updates, update_receiver) = mpsc::channel(4);

                let initial = proto::SubscribeResponse {
                    response: Some(proto::subscribe_response::Response::SyncResponse(true)),
                    ..Default::default()
                };

                let _ = updates.send(Ok(initial)).await;

                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_millis(20));
                    let mut timestamp = 0;

                    loop {
                        interval.tick().await;
                        timestamp += 1;

                        let response = proto::SubscribeResponse {
                            response: Some(proto::subscribe_response::Response::Update(
                                proto::Notification {
                                    timestamp,
                                    ..Default::default()
                                },
                            )),
                            ..Default::default()
                        };

                        if updates.send(Ok(response)).await.is_err() {
                            break;
                        }
                    }
                });

                return Ok(tonic::Response::new(ReceiverStream::new(update_receiver)));
            }

            Ok(tonic::Response::new(ReceiverStream::new(receiver)))
        }
    }

    async fn send_selected_updates(
        paths: Vec<Path>,
        responses: &mpsc::Sender<Result<proto::SubscribeResponse, tonic::Status>>,
    ) {
        let paths = if paths.iter().any(|path| {
            path.elem.len() == 2
                && path.elem[0].name == "interfaces"
                && path.elem[1].name == "interface"
        }) {
            [
                vec!["state", "oper-status"],
                vec!["phy-diag", "state", "raw-ber"],
                vec!["state", "counters", "in-errors"],
            ]
            .into_iter()
            .map(|tail| Path {
                elem: ["interfaces", "interface"]
                    .into_iter()
                    .chain(tail)
                    .map(|name| PathElem {
                        name: name.to_string(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect()
        } else {
            paths
        };

        let updates = paths
            .into_iter()
            .filter_map(|path| {
                let leaf = path.elem.last()?.name.as_str();

                let value = match leaf {
                    "oper-status" => proto::typed_value::Value::StringVal("UP".into()),
                    "raw-ber" => proto::typed_value::Value::DoubleVal(0.000001),
                    "in-errors" => proto::typed_value::Value::UintVal(7),
                    _ => return None,
                };

                Some(proto::Update {
                    path: Some(proto::Path {
                        elem: path.elem.into_iter().skip(2).collect(),
                        ..Default::default()
                    }),
                    val: Some(proto::TypedValue { value: Some(value) }),
                    ..Default::default()
                })
            })
            .collect();

        let notification = proto::Notification {
            timestamp: 42,
            prefix: Some(proto::Path {
                elem: vec![
                    PathElem {
                        name: "interfaces".into(),
                        ..Default::default()
                    },
                    PathElem {
                        name: "interface".into(),
                        key: [("name".into(), "nvl0".into())].into(),
                    },
                ],
                ..Default::default()
            }),
            update: updates,
            ..Default::default()
        };

        responses
            .send(Ok(proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::Update(
                    notification.clone(),
                )),
                ..Default::default()
            }))
            .await
            .expect("selective notification receiver");

        responses
            .send(Ok(proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::SyncResponse(true)),
                ..Default::default()
            }))
            .await
            .expect("selective sync receiver");

        responses
            .send(Ok(proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::Update(
                    proto::Notification {
                        timestamp: 43,
                        ..notification
                    },
                )),
                ..Default::default()
            }))
            .await
            .expect("selective telemetry receiver");
    }

    // Count actual server sockets so response-stream tests cannot miss an RPC
    // whose request direction remains open after the client drops it.
    struct CountingTcpStream {
        stream: TcpStream,
        sockets: Arc<AtomicUsize>,
    }

    impl Drop for CountingTcpStream {
        fn drop(&mut self) {
            self.sockets.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Connected for CountingTcpStream {
        type ConnectInfo = ();

        fn connect_info(&self) -> Self::ConnectInfo {}
    }

    impl AsyncRead for CountingTcpStream {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for CountingTcpStream {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.stream).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.stream).poll_shutdown(cx)
        }
    }

    async fn wait_for_transport_shutdown(service: &TestGnmiService, sockets: &AtomicUsize) {
        assert!(
            tokio::time::timeout(Duration::from_secs(2), async {
                while service.active_requests.load(Ordering::SeqCst) != 0
                    || sockets.load(Ordering::SeqCst) != 0
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok(),
            "abandoned RPC and socket must close before reconnect: active_requests={}, sockets={}",
            service.active_requests.load(Ordering::SeqCst),
            sockets.load(Ordering::SeqCst),
        );
    }

    async fn assert_broad_counter_available(
        client: &GnmiClient,
        service: &TestGnmiService,
        sockets: &AtomicUsize,
    ) {
        let broad_config = NvueGnmiPaths {
            components_enabled: false,
            platform_general_enabled: false,
            ..Default::default()
        };

        let mut broad = client
            .subscribe_sample(&nvue_subscribe_paths(&broad_config), 1_000_000)
            .await
            .expect("open broad interface subscription");

        let baseline = broad
            .next()
            .await
            .expect("broad response")
            .expect("broad status");

        let Some(proto::subscribe_response::Response::Update(baseline)) = baseline.response else {
            panic!("broad subscription should receive an update");
        };

        assert_eq!(baseline.update.len(), 3);

        assert!(baseline.update.iter().any(|update| {
            update
                .path
                .as_ref()
                .is_some_and(|path| path.elem.iter().any(|element| element.name == "in-errors"))
        }));

        drop(broad);
        wait_for_transport_shutdown(service, sockets).await;
    }

    async fn assert_selective_output(
        client: &GnmiClient,
        service: &TestGnmiService,
        sockets: &AtomicUsize,
    ) {
        service.selected_updates.store(true, Ordering::SeqCst);

        assert_broad_counter_available(client, service, sockets).await;

        let selective_config = NvueGnmiPaths {
            interface_paths: Some(vec![
                vec!["state".into(), "oper-status".into()],
                vec!["phy-diag".into(), "state".into(), "raw-ber".into()],
            ]),
            ..Default::default()
        };

        let selective_paths = nvue_interface_subscribe_paths(&selective_config);

        let mut subscription = client
            .subscribe_sample(&selective_paths, 1_000_000)
            .await
            .expect("open selected interface subscription");

        assert_eq!(
            *service.requested_paths.lock().expect("requested path lock"),
            [
                "interfaces/interface/state/oper-status",
                "interfaces/interface/phy-diag/state/raw-ber"
            ]
        );

        let update = tokio::time::timeout(Duration::from_secs(2), subscription.next())
            .await
            .expect("selected update timeout")
            .expect("selected update stream")
            .expect("selected update status");

        let Some(proto::subscribe_response::Response::Update(notification)) = &update.response
        else {
            panic!("selected subscription should receive an update");
        };

        assert_eq!(notification.update.len(), 2);
        assert_eq!(notification.timestamp, 42);

        let endpoint = test_endpoint(
            "55:66:77:88:99:cc"
                .parse()
                .expect("test endpoint MAC address"),
        );

        let sink = Arc::new(RecordingMetricSink::default());

        let processor = GnmiSampleProcessor {
            data_sink: Some(sink.clone()),
            event_context: EventContext::from_endpoint(&endpoint, NVUE_GNMI_SAMPLE_STREAM_ID),
            switch_id: "test-switch".into(),
            diagnostic_stream: Some("interfaces"),
        };

        let stream_metrics = GnmiStreamMetrics {
            connection_state: IntGauge::new("test_connection", "test").expect("connection gauge"),
            connected: IntGauge::new("test_connected", "test").expect("connected gauge"),
            synchronized: IntGauge::new("test_synchronized", "test").expect("sync gauge"),
            reconnections_total: Counter::new("test_reconnections", "test").expect("reconnects"),
            server_initiated_closures_total: Counter::new("test_closures", "test")
                .expect("closures"),
            connection_established_timestamp: Gauge::new("test_established", "test")
                .expect("established gauge"),
            notifications_received_total: Counter::new("test_notifications", "test")
                .expect("notifications"),
            last_notification_timestamp: Gauge::new("test_last_notification", "test")
                .expect("last notification"),
            notification_processing_seconds: Histogram::with_opts(HistogramOpts::new(
                "test_processing",
                "test",
            ))
            .expect("processing histogram"),
            stream_errors_total: Counter::new("test_errors", "test").expect("errors"),
            monitored_entities: Gauge::new("test_entities", "test").expect("entities"),
        };

        processor.process_subscribe_response(&update, &stream_metrics);

        let sync = subscription
            .next()
            .await
            .expect("sync stream")
            .expect("sync status");

        assert!(matches!(
            sync.response,
            Some(proto::subscribe_response::Response::SyncResponse(true))
        ));

        let later = subscription
            .next()
            .await
            .expect("later telemetry stream")
            .expect("later telemetry status");

        assert!(matches!(
            later.response,
            Some(proto::subscribe_response::Response::Update(
                proto::Notification { timestamp: 43, .. }
            ))
        ));

        assert_eq!(service.active_requests.load(Ordering::SeqCst), 1);

        let samples = sink.0.lock().expect("recorded metric lock").clone();
        assert_eq!(samples.len(), 3);

        assert!(samples.iter().all(|(context, sample)| {
            context.collector_type == NVUE_GNMI_SAMPLE_STREAM_ID
                && sample
                    .labels
                    .iter()
                    .any(|(key, value)| key == "interface_name" && value == "nvl0")
        }));

        let export = build_metrics_export_request(&samples, 42, "carbide_hardware_health");
        let metrics = &export.resource_metrics[0].scope_metrics[0].metrics;

        let names = metrics
            .iter()
            .map(|metric| metric.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            [
                "carbide_hardware_health_nvue_gnmi_interface_oper_status_state",
                "carbide_hardware_health_nvue_gnmi_interface_oper_status_state",
                "carbide_hardware_health_nvue_gnmi_interface_raw_ber_ratio",
            ]
        );

        assert!(metrics.iter().all(|metric| match &metric.data {
            Some(crate::otlp::metrics::metric::Data::Gauge(gauge)) =>
                gauge.data_points.iter().all(|point| {
                    point
                        .attributes
                        .iter()
                        .any(|attribute| attribute.key == "interface_name")
                }),
            _ => false,
        }));

        drop(subscription);
        wait_for_transport_shutdown(service, sockets).await;
    }

    #[tokio::test]
    async fn abandoned_subscriptions_release_real_transport_connections() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("test certificate");

        let identity = Identity::from_pem(
            certificate.cert.pem(),
            certificate.signing_key.serialize_pem(),
        );

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind gNMI server");

        let port = listener.local_addr().expect("server address").port();
        let service = TestGnmiService::default();

        let sockets = Arc::new(AtomicUsize::new(0));
        let accepted_sockets = sockets.clone();

        let incoming = TcpListenerStream::new(listener).map(move |accepted| {
            accepted.map(|stream| {
                accepted_sockets.fetch_add(1, Ordering::SeqCst);
                CountingTcpStream {
                    stream,
                    sockets: accepted_sockets.clone(),
                }
            })
        });

        let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();

        let server = Server::builder()
            .tls_config(ServerTlsConfig::new().identity(identity))
            .expect("server TLS")
            .add_service(proto::g_nmi_server::GNmiServer::new(service.clone()))
            .serve_with_incoming_shutdown(incoming, async move {
                let _ = shutdown_receiver.await;
            });

        let server_task = tokio::spawn(server);

        let client = GnmiClient::new(GnmiClientConfig {
            switch_id: "test-switch".to_string(),
            rack_id: None,
            host: "127.0.0.1".to_string(),
            port,
            username: None,
            password: None,
            request_timeout: Duration::from_secs(2),
            dangerously_skip_tls_verification: true,
            tls_config: None,
        });

        let prefix = system_events_prefix();
        let paths = system_events_subscribe_path();

        let extended_request = build_extended_subscribe_request(&NvueGnmiSubscriptionConfig {
            name: "test-extra".to_string(),
            paths: vec![vec!["state".to_string()]],
            ..Default::default()
        })
        .expect("additional subscription request");

        for mode in 0..3 {
            for _retry in 0..3 {
                let mut subscription = match mode {
                    0 => client.subscribe_sample(&paths, 1_000_000).await,
                    1 => client.subscribe_on_change(&prefix, &paths).await,
                    _ => client.subscribe_request(extended_request.clone()).await,
                }
                .expect("open subscription");

                assert!(
                    tokio::time::timeout(Duration::from_millis(70), subscription.next())
                        .await
                        .is_err(),
                    "unsynchronized subscription must time out"
                );

                drop(subscription);
                wait_for_transport_shutdown(&service, &sockets).await;
            }
        }

        service.synchronized.store(true, Ordering::SeqCst);

        for mode in 0..3 {
            let mut subscription = match mode {
                0 => client.subscribe_sample(&paths, 1_000_000).await,
                1 => client.subscribe_on_change(&prefix, &paths).await,
                _ => client.subscribe_request(extended_request.clone()).await,
            }
            .expect("open healthy subscription");

            let first = tokio::time::timeout(Duration::from_secs(2), subscription.next())
                .await
                .expect("synchronization response")
                .expect("open response stream")
                .expect("successful response");

            assert!(matches!(
                first.response,
                Some(proto::subscribe_response::Response::SyncResponse(true))
            ));

            for expected_timestamp in 1..=2 {
                let update = tokio::time::timeout(Duration::from_secs(2), subscription.next())
                    .await
                    .expect("telemetry response")
                    .expect("open response stream")
                    .expect("successful response");

                assert!(matches!(
                    update.response,
                    Some(proto::subscribe_response::Response::Update(proto::Notification {
                        timestamp,
                        ..
                    })) if timestamp == expected_timestamp
                ));
            }

            assert_eq!(service.active_requests.load(Ordering::SeqCst), 1);
            assert_eq!(sockets.load(Ordering::SeqCst), 1);
            drop(subscription);
            wait_for_transport_shutdown(&service, &sockets).await;
        }

        assert_selective_output(&client, &service, &sockets).await;

        shutdown_sender.send(()).expect("stop server");
        server_task
            .await
            .expect("server task")
            .expect("server result");
    }

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
    fn primary_subscribe_paths_exclude_leak_sensors() {
        check_values(
            [
                Check {
                    scenario: "no primary paths",
                    input: NvueGnmiPaths {
                        components_enabled: false,
                        interfaces_enabled: false,
                        interface_paths: None,
                        platform_general_enabled: false,
                        leak_sensors_enabled: true,
                    },
                    expect: String::new(),
                },
                Check {
                    scenario: "components only",
                    input: NvueGnmiPaths {
                        components_enabled: true,
                        interfaces_enabled: false,
                        interface_paths: None,
                        platform_general_enabled: false,
                        leak_sensors_enabled: true,
                    },
                    expect: "components/component".to_string(),
                },
                Check {
                    scenario: "interfaces only",
                    input: NvueGnmiPaths {
                        components_enabled: false,
                        interfaces_enabled: true,
                        interface_paths: None,
                        platform_general_enabled: false,
                        leak_sensors_enabled: true,
                    },
                    expect: "interfaces/interface".to_string(),
                },
                Check {
                    scenario: "platform general only",
                    input: NvueGnmiPaths {
                        components_enabled: false,
                        interfaces_enabled: false,
                        interface_paths: None,
                        platform_general_enabled: true,
                        leak_sensors_enabled: true,
                    },
                    expect: "platform-general/state,platform-general/versions".to_string(),
                },
                Check {
                    scenario: "all primary paths",
                    input: NvueGnmiPaths {
                        leak_sensors_enabled: true,
                        ..Default::default()
                    },
                    expect: "components/component,interfaces/interface,platform-general/state,platform-general/versions".to_string(),
                },
            ],
            |config| {
                nvue_subscribe_paths(&config)
                    .into_iter()
                    .map(|path| {
                        path.elem
                            .into_iter()
                            .map(|elem| elem.name)
                            .collect::<Vec<_>>()
                            .join("/")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            },
        );
    }

    #[test]
    fn selected_interface_paths_replace_the_primary_interface_subtree() {
        let config = NvueGnmiPaths {
            interface_paths: Some(vec![
                vec!["state".into(), "oper-status".into()],
                vec!["phy-diag".into(), "state".into(), "raw-ber".into()],
            ]),
            ..Default::default()
        };

        let names = |paths: Vec<Path>| {
            paths
                .into_iter()
                .map(|path| {
                    format!(
                        "/{}",
                        path.elem
                            .into_iter()
                            .map(|element| element.name)
                            .collect::<Vec<_>>()
                            .join("/")
                    )
                })
                .collect::<Vec<_>>()
        };

        let primary = names(nvue_subscribe_paths(&config));
        let interface = names(nvue_interface_subscribe_paths(&config));

        assert_eq!(
            primary,
            [
                "/components/component",
                "/platform-general/state",
                "/platform-general/versions"
            ]
        );

        assert_eq!(
            interface,
            [
                "/interfaces/interface/state/oper-status",
                "/interfaces/interface/phy-diag/state/raw-ber"
            ]
        );
    }

    #[test]
    fn leak_sensor_path_is_built_separately() {
        let actual = nvue_leak_sensor_subscribe_path()
            .elem
            .into_iter()
            .map(|elem| elem.name)
            .collect::<Vec<_>>()
            .join("/");

        assert_eq!(
            actual,
            "platform-general/leak-sensors/leak-sensor/state/state"
        );
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
