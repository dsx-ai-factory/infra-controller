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
mod nmxc_api;
mod response;

// Generated gRPC types and client from nmx_c.proto
pub mod nmxc_model {
    #![allow(clippy::all, non_snake_case)]
    include!(concat!(env!("OUT_DIR"), "/nmx_c.rs"));
}

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime};

use http::Uri;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Identity};
use tracing::debug;

use crate::nmxc_api::NmxcApi;
use crate::nmxc_model::nmx_controller_client::NmxControllerClient;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP/2 keepalive for established channels; see the comment in
/// [`TlsChannelConnector::connect`] for what these bound.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(15);

/// `gateway_id` sent on NMX-C gRPC requests from Carbide and the `nmxc` test client.
pub const NMX_C_GATEWAY_ID: &str = "carbide";

#[derive(thiserror::Error, Debug)]
pub enum NmxcError {
    #[error("invalid endpoint URL: {0}")]
    InvalidEndpoint(String),

    #[error("transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    #[error("gRPC status: {0}")]
    Status(#[from] tonic::Status),

    #[error("connection not initialized")]
    Uninitialized,

    #[error("NMX-C {operation} response missing server_header")]
    MissingServerHeader { operation: &'static str },

    #[error("NMX-C {operation} returned status code {return_code}")]
    NmxReturnCode {
        return_code: i32,
        operation: &'static str,
    },
}

impl NmxcError {
    /// Creates an error for invalid or missing response from the server.
    pub fn invalid_response(msg: impl Into<String>) -> Self {
        NmxcError::Status(tonic::Status::unknown(msg.into()))
    }

    /// NMX-C `server_header.return_code` when this error is [`NmxcError::NmxReturnCode`].
    pub fn nmx_return_code(&self) -> Option<i32> {
        match self {
            NmxcError::NmxReturnCode { return_code, .. } => Some(*return_code),
            _ => None,
        }
    }

    /// True when NMX-C returned `NMX_ST_RESOURCE_EXHAUSTED`.
    pub fn is_nmx_resource_exhausted(&self) -> bool {
        self.nmx_return_code() == Some(nmxc_model::StReturnCode::NmxStResourceExhausted as i32)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Base URI for the NMX-C gRPC service (e.g. `https://host:50051` or `http://localhost:50051`).
    pub uri: Uri,
}

impl Endpoint {
    pub fn new(url: impl AsRef<str>) -> Result<Self, NmxcError> {
        let uri = url
            .as_ref()
            .parse::<Uri>()
            .map_err(|e| NmxcError::InvalidEndpoint(format!("{}: {e}", url.as_ref())))?;
        Ok(Self { uri })
    }
}

/// Optional TLS paths for HTTPS connections to NMX-C.
///
/// When both `client_cert_path` and `client_key_path` are set, the client presents a certificate
/// for mutual TLS. `ca_cert_path` adds an extra CA bundle for verifying the server (system roots
/// are still used unless configured otherwise by tonic).
///
/// `authority` sets the TLS server name (SNI / certificate verification hostname).
/// A non-empty value overrides the endpoint host. If absent or empty, the endpoint
/// host is used without IPv6 brackets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NmxcTlsConfig {
    pub ca_cert_path: Option<PathBuf>,
    pub client_cert_path: Option<PathBuf>,
    pub client_key_path: Option<PathBuf>,
    pub authority: Option<String>,
}

impl NmxcTlsConfig {
    /// True when any of the TLS material files on disk was modified after
    /// `created`, i.e. a channel created at that time no longer reflects the
    /// certificates a fresh connect would load. A file whose modification time
    /// cannot be read (e.g. it was removed) does not mark the channel stale:
    /// the cached channel keeps serving until newer readable material appears,
    /// since rebuilding on that alone would tear down a working channel
    /// mid-rotation. A future-dated modification time (writer clock skew)
    /// keeps this returning true, which is bounded: every acquisition then
    /// connects fresh, the pre-cache behavior.
    async fn material_newer_than(&self, created: SystemTime) -> bool {
        let paths = [
            &self.ca_cert_path,
            &self.client_cert_path,
            &self.client_key_path,
        ];
        for path in paths.into_iter().flatten() {
            let modified = tokio::fs::metadata(path).await.and_then(|m| m.modified());
            let mtime = match modified {
                Ok(mtime) => mtime,
                Err(e) => {
                    tracing::debug!(
                        path = %path.display(),
                        error = %e,
                        "could not read NMX-C TLS material mtime; keeping the cached channel"
                    );
                    continue;
                }
            };
            if mtime > created {
                tracing::info!(
                    path = %path.display(),
                    "NMX-C TLS material changed on disk; rebuilding the channel to pick it up"
                );
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Debug)]
pub struct NmxcClientPoolBuilder {
    pub timeout: Duration,
    pub tls: Option<NmxcTlsConfig>,
}

impl Default for NmxcClientPoolBuilder {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            tls: None,
        }
    }
}

impl NmxcClientPoolBuilder {
    pub fn build(self) -> Result<NmxcClientPool, NmxcError> {
        Ok(NmxcClientPool::with_connector(Arc::new(
            TlsChannelConnector {
                timeout: self.timeout,
                tls: self.tls,
            },
        )))
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn tls(mut self, tls: NmxcTlsConfig) -> Self {
        self.tls = Some(tls);
        self
    }
}

/// Establishes the tonic [`Channel`] used to talk to one NMX-C endpoint, and
/// decides when a previously established one has gone stale.
///
/// The production connector ([`TlsChannelConnector`]) performs the TCP(+TLS)
/// connect, reading TLS material from disk on every call, and considers a
/// channel stale once newer TLS material appears on disk. Tests inject a
/// counting connector to observe how often channels are (re)built.
#[async_trait::async_trait]
trait ChannelConnector: Send + Sync + std::fmt::Debug {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Channel, NmxcError>;

    /// True when a channel created at `created` should be discarded and
    /// rebuilt instead of reused.
    async fn is_stale(&self, created: SystemTime) -> bool;
}

/// The production [`ChannelConnector`]: connects eagerly with the pool's
/// timeout and optional (m)TLS configuration.
#[derive(Debug)]
struct TlsChannelConnector {
    timeout: Duration,
    tls: Option<NmxcTlsConfig>,
}

/// Connected channels by endpoint URI, each with its creation time, shared by
/// all clones of a pool.
type ChannelCache = Arc<Mutex<HashMap<String, (Channel, SystemTime)>>>;

fn lock_channels(
    channels: &ChannelCache,
) -> MutexGuard<'_, HashMap<String, (Channel, SystemTime)>> {
    // The critical sections only look up / insert / remove map entries, so a
    // poisoned lock leaves the map usable; recover it instead of propagating
    // the panic into fabric-protocol paths.
    channels.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug)]
pub struct NmxcClientPool {
    connector: Arc<dyn ChannelConnector>,
    channels: ChannelCache,
}

impl NmxcClientPool {
    pub fn builder() -> NmxcClientPoolBuilder {
        NmxcClientPoolBuilder::default()
    }

    fn with_connector(connector: Arc<dyn ChannelConnector>) -> Self {
        Self {
            connector,
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn create_client(&self, endpoint: Endpoint) -> Result<Box<dyn Nmxc>, NmxcError> {
        let uri = endpoint.uri.to_string();
        let channel = self.channel_for(&endpoint, &uri).await?;
        let client = NmxControllerClient::new(trace_propagation::TraceInjectService::new(channel))
            .max_decoding_message_size(usize::MAX);
        Ok(Box::new(NmxcApi::new(client)))
    }

    /// Returns the endpoint's cached channel, connecting (and caching) one when
    /// none exists or the cached one has gone stale.
    ///
    /// tonic channels multiplex requests and re-establish dropped connections
    /// lazily, so one channel per endpoint serves any number of clients across
    /// calls and monitor ticks, self-healing dropped connections along the way.
    /// The connector's staleness check rebuilds the channel once the TLS
    /// material on disk changes (the same policy as the repo's gRPC clients),
    /// so certificate rotations are picked up without a restart.
    ///
    /// Concurrent misses for the same URI may each connect, last insert wins --
    /// a benign duplicate handshake. The steady-state caller is a serial
    /// monitor loop -- admin RPCs can also race in here, at worst repeating a
    /// handshake -- so per-URI in-flight serialization would be machinery
    /// without a workload.
    async fn channel_for(&self, endpoint: &Endpoint, uri: &str) -> Result<Channel, NmxcError> {
        let cached = lock_channels(&self.channels).get(uri).cloned();
        if let Some((channel, created)) = cached {
            if !self.connector.is_stale(created).await {
                return Ok(channel);
            }
            // Newer TLS material on disk: discard the stale entry and connect
            // fresh below.
            lock_channels(&self.channels).remove(uri);
        }

        // Timestamp before connecting: the connect reads TLS material from
        // disk, so material rewritten while we connect still counts as newer
        // than `created` and triggers one more (benign) rebuild.
        let created = SystemTime::now();
        let channel = self.connector.connect(endpoint).await?;
        lock_channels(&self.channels).insert(uri.to_string(), (channel.clone(), created));
        Ok(channel)
    }
}

impl TlsChannelConnector {
    async fn build_https_tls_config(&self, uri: &Uri) -> Result<ClientTlsConfig, NmxcError> {
        let default_tls = NmxcTlsConfig::default();
        let tls = self.tls.as_ref().unwrap_or(&default_tls);
        let mut config = ClientTlsConfig::new();

        if let Some(ref path) = tls.ca_cert_path {
            let pem = tokio::fs::read(path).await.map_err(|e| {
                NmxcError::InvalidEndpoint(format!(
                    "read NMX-C TLS CA cert {}: {e}",
                    path.display()
                ))
            })?;
            config = config.ca_certificate(Certificate::from_pem(pem));
        }

        match (&tls.client_cert_path, &tls.client_key_path) {
            (Some(cert_path), Some(key_path)) => {
                let cert = tokio::fs::read(cert_path).await.map_err(|e| {
                    NmxcError::InvalidEndpoint(format!(
                        "read NMX-C TLS client cert {}: {e}",
                        cert_path.display()
                    ))
                })?;
                let key = tokio::fs::read(key_path).await.map_err(|e| {
                    NmxcError::InvalidEndpoint(format!(
                        "read NMX-C TLS client key {}: {e}",
                        key_path.display()
                    ))
                })?;
                config = config.identity(Identity::from_pem(cert, key));
            }
            (None, None) => {}
            _ => {
                return Err(NmxcError::InvalidEndpoint(
                    "NMX-C TLS client cert path and key path must both be set for mTLS".to_string(),
                ));
            }
        }

        let domain = tls.authority.clone().filter(|s| !s.is_empty()).or_else(|| {
            // uri.host() retains IPv6 brackets; a TLS server name needs the bare address.
            uri.host()
                .map(|host| host.trim_matches(['[', ']']).to_string())
        });
        if let Some(d) = domain {
            config = config.domain_name(d);
        }

        Ok(config)
    }
}

#[async_trait::async_trait]
impl ChannelConnector for TlsChannelConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Channel, NmxcError> {
        let uri = &endpoint.uri;
        let scheme = uri.scheme_str().unwrap_or("http");
        // HTTP/2 keepalive pings while an RPC is in flight: a cached channel
        // can sit on a connection whose peer silently went away (chassis
        // power-off), and without pings the first RPC on it waits out the
        // kernel's TCP timeout -- minutes in which one dead endpoint stalls
        // the serial monitor loop. Pinging bounds that at interval + timeout.
        // Idle pings stay off so the client cannot trip gRPC servers' ping
        // rate limits between calls.
        let channel = if scheme.eq_ignore_ascii_case("https") {
            let endpoint_builder = tonic::transport::Endpoint::from_shared(uri.to_string())
                .map_err(|e| NmxcError::InvalidEndpoint(e.to_string()))?
                .connect_timeout(self.timeout)
                .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
                .keep_alive_timeout(KEEP_ALIVE_TIMEOUT);

            let tls_config = self.build_https_tls_config(uri).await?;
            endpoint_builder
                .tls_config(tls_config)
                .map_err(|e| NmxcError::InvalidEndpoint(e.to_string()))?
                .connect()
                .await?
        } else {
            tonic::transport::Channel::from_shared(uri.to_string())
                .map_err(|e| NmxcError::InvalidEndpoint(e.to_string()))?
                .connect_timeout(self.timeout)
                .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
                .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
                .connect()
                .await?
        };

        debug!(endpoint = %endpoint.uri, "Connected to NMX-C");
        Ok(channel)
    }

    async fn is_stale(&self, created: SystemTime) -> bool {
        // Without TLS there is no on-disk material to outdate a channel.
        match self.tls.as_ref() {
            Some(tls) => tls.material_newer_than(created).await,
            None => false,
        }
    }
}

/// Abstraction over [`NmxcClientPool`] and test doubles (e.g. `NmxcSimClient` in carbide-api).
#[async_trait::async_trait]
pub trait NmxcPool: Send + Sync + 'static {
    async fn create_client(&self, endpoint: Endpoint) -> Result<Box<dyn Nmxc>, NmxcError>;
}

#[async_trait::async_trait]
impl NmxcPool for NmxcClientPool {
    async fn create_client(&self, endpoint: Endpoint) -> Result<Box<dyn Nmxc>, NmxcError> {
        NmxcClientPool::create_client(self, endpoint).await
    }
}

#[async_trait::async_trait]
pub trait Nmxc: Send + Sync + 'static {
    /// Perform Hello handshake with the NMX-C controller.
    async fn hello(&mut self, gateway_id: &str) -> Result<nmxc_model::ServerHello, NmxcError>;

    async fn get_domain_properties(
        &mut self,
        context: Option<nmxc_model::Context>,
        gateway_id: &str,
    ) -> Result<nmxc_model::DomainProperties, NmxcError>;

    async fn get_domain_state_info(
        &mut self,
        context: Option<nmxc_model::Context>,
        gateway_id: &str,
    ) -> Result<nmxc_model::DomainStateInfo, NmxcError>;

    async fn get_topology_info(
        &mut self,
        context: Option<nmxc_model::Context>,
        gateway_id: &str,
    ) -> Result<nmxc_model::FmTopologyInfo, NmxcError>;

    async fn get_compute_node_count(
        &mut self,
        req: nmxc_model::GetComputeNodeCountRequest,
    ) -> Result<nmxc_model::GetComputeNodeCountResponse, NmxcError>;

    async fn get_compute_node_info_list(
        &mut self,
        req: nmxc_model::GetComputeNodeInfoListRequest,
    ) -> Result<nmxc_model::GetComputeNodeInfoListResponse, NmxcError>;

    async fn get_gpu_info_list(
        &mut self,
        req: nmxc_model::GetGpuInfoListRequest,
    ) -> Result<nmxc_model::GetGpuInfoListResponse, NmxcError>;

    async fn get_switch_node_count(
        &mut self,
        req: nmxc_model::GetSwitchNodeCountRequest,
    ) -> Result<nmxc_model::GetSwitchNodeCountResponse, NmxcError>;

    async fn get_switch_node_info_list(
        &mut self,
        req: nmxc_model::GetSwitchNodeInfoListRequest,
    ) -> Result<nmxc_model::GetSwitchNodeInfoListResponse, NmxcError>;

    async fn get_partition_count(
        &mut self,
        req: nmxc_model::GetPartitionCountRequest,
    ) -> Result<nmxc_model::GetPartitionCountResponse, NmxcError>;

    async fn get_partition_id_list(
        &mut self,
        req: nmxc_model::GetPartitionIdListRequest,
    ) -> Result<nmxc_model::GetPartitionIdListResponse, NmxcError>;

    async fn get_partition_info_list(
        &mut self,
        req: nmxc_model::GetPartitionInfoListRequest,
    ) -> Result<nmxc_model::GetPartitionInfoListResponse, NmxcError>;

    async fn create_partition(
        &mut self,
        req: nmxc_model::CreatePartitionRequest,
    ) -> Result<nmxc_model::CreatePartitionResponse, NmxcError>;

    async fn delete_partition(
        &mut self,
        req: nmxc_model::DeletePartitionRequest,
    ) -> Result<nmxc_model::DeletePartitionResponse, NmxcError>;

    async fn add_gpus_to_partition(
        &mut self,
        req: nmxc_model::UpdatePartitionRequest,
    ) -> Result<nmxc_model::UpdatePartitionResponse, NmxcError>;

    async fn remove_gpus_from_partition(
        &mut self,
        req: nmxc_model::UpdatePartitionRequest,
    ) -> Result<nmxc_model::UpdatePartitionResponse, NmxcError>;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, check_cases_async};

    use super::*;

    /// A [`ChannelConnector`] that counts connects and hands out lazy channels,
    /// so tests observe channel (re)builds without a live NMX-C server. Its
    /// staleness policy is the production one, driven by the optional TLS
    /// paths.
    #[derive(Debug, Default)]
    struct CountingConnector {
        connects: AtomicUsize,
        tls: Option<NmxcTlsConfig>,
    }

    impl CountingConnector {
        fn with_tls(tls: NmxcTlsConfig) -> Self {
            Self {
                connects: AtomicUsize::new(0),
                tls: Some(tls),
            }
        }

        fn connect_count(&self) -> usize {
            self.connects.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ChannelConnector for CountingConnector {
        async fn connect(&self, endpoint: &Endpoint) -> Result<Channel, NmxcError> {
            self.connects.fetch_add(1, Ordering::SeqCst);
            let channel = tonic::transport::Endpoint::from_shared(endpoint.uri.to_string())
                .map_err(|e| NmxcError::InvalidEndpoint(e.to_string()))?
                .connect_lazy();
            Ok(channel)
        }

        async fn is_stale(&self, created: SystemTime) -> bool {
            match self.tls.as_ref() {
                Some(tls) => tls.material_newer_than(created).await,
                None => false,
            }
        }
    }

    #[tokio::test]
    async fn https_tls_accepts_endpoint_hosts_and_preserves_authority_precedence() {
        check_cases_async(
            [
                Case {
                    scenario: "IPv6 without custom TLS configuration",
                    input: ("https://[2001:db8::1]:50051", None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "IPv6 with custom TLS configuration and no authority",
                    input: (
                        "https://[2001:db8::1]:50051",
                        Some(NmxcTlsConfig::default()),
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "IPv6 with an empty authority uses the endpoint host",
                    input: (
                        "https://[2001:db8::1]:50051",
                        Some(NmxcTlsConfig {
                            authority: Some(String::new()),
                            ..NmxcTlsConfig::default()
                        }),
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "IPv4 endpoint",
                    input: ("https://192.0.2.1:50051", None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "hostname endpoint",
                    input: ("https://nmxc.example:50051", None),
                    expect: Yields(()),
                },
                Case {
                    scenario: "invalid explicit authority is not replaced by the endpoint host",
                    input: (
                        "https://[2001:db8::1]:50051",
                        Some(NmxcTlsConfig {
                            authority: Some("invalid server name".to_string()),
                            ..NmxcTlsConfig::default()
                        }),
                    ),
                    expect: Fails,
                },
            ],
            |(url, tls)| async move {
                let uri: Uri = url.parse().expect("endpoint URI");
                let connector = TlsChannelConnector {
                    timeout: DEFAULT_TIMEOUT,
                    tls,
                };
                let config = connector
                    .build_https_tls_config(&uri)
                    .await
                    .expect("TLS configuration");

                tonic::transport::Endpoint::from(uri)
                    .tls_config(config)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            },
        )
        .await;
    }

    #[tokio::test]
    async fn same_endpoint_reuses_cached_channel() {
        let connector = Arc::new(CountingConnector::default());
        let pool = NmxcClientPool::with_connector(connector.clone());
        let endpoint = Endpoint::new("http://127.0.0.1:50051").expect("endpoint");

        pool.create_client(endpoint.clone())
            .await
            .expect("first client");
        pool.create_client(endpoint).await.expect("second client");

        assert_eq!(
            connector.connect_count(),
            1,
            "AFTER: repeat create_client calls share the cached channel"
        );
    }

    #[tokio::test]
    async fn distinct_endpoints_connect_separately() {
        let connector = Arc::new(CountingConnector::default());
        let pool = NmxcClientPool::with_connector(connector.clone());

        pool.create_client(Endpoint::new("http://127.0.0.1:50051").expect("endpoint"))
            .await
            .expect("first client");
        pool.create_client(Endpoint::new("http://127.0.0.1:50052").expect("endpoint"))
            .await
            .expect("second client");

        assert_eq!(
            connector.connect_count(),
            2,
            "each endpoint gets its own channel"
        );
    }

    #[tokio::test]
    async fn newer_tls_material_on_disk_rebuilds_the_channel() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cert_path = dir.path().join("client-cert.pem");
        std::fs::write(&cert_path, "cert material v1").expect("write cert");

        let connector = Arc::new(CountingConnector::with_tls(NmxcTlsConfig {
            client_cert_path: Some(cert_path.clone()),
            ..NmxcTlsConfig::default()
        }));
        let pool = NmxcClientPool::with_connector(connector.clone());
        let endpoint = Endpoint::new("http://127.0.0.1:50051").expect("endpoint");

        pool.create_client(endpoint.clone())
            .await
            .expect("first client");
        pool.create_client(endpoint.clone())
            .await
            .expect("second client");
        assert_eq!(
            connector.connect_count(),
            1,
            "unchanged TLS material keeps the cached channel"
        );

        // Rotate the certificate: give the file a modification time strictly
        // after the cached channel's creation. An explicit future timestamp
        // avoids depending on the filesystem's mtime resolution.
        let rotated = SystemTime::now() + Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&cert_path)
            .expect("open cert")
            .set_modified(rotated)
            .expect("set cert mtime");

        pool.create_client(endpoint)
            .await
            .expect("client after rotation");
        assert_eq!(
            connector.connect_count(),
            2,
            "AFTER: newer TLS material on disk rebuilds the channel"
        );
    }
}
