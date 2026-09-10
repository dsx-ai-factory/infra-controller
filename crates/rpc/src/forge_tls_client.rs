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
use std::io;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eyre::Result;
use forge_http_connector::connector::ForgeHttpConnector;
use forge_http_connector::resolver::{ForgeResolver, ForgeResolverOpts};
use forge_tls::client_config::ClientCert;
use forge_tls::dummy_tls_verifier::DummyTlsVerifier;
use hickory_resolver::config::ResolverConfig;
use hyper::body::Incoming;
use hyper_util::client::legacy;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ClientConfig, RootCertStore};
use tonic::body::Body;
use tonic::transport::Uri;
use tower::ServiceExt;
use tower::util::BoxCloneService;
use tryhard::backoff_strategies::FixedBackoff;
use tryhard::{NoOnRetry, RetryFutureConfig};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::forge::VersionRequest;
use crate::forge_resolver::resolver::ResolverError;
use crate::forge_tls_client::ConfigurationError::CouldNotReadRootCa;
use crate::node_jwt::{BearerAuthService, NodeJwtMinter, NodeTokenProvider};
use crate::protos::forge::forge_client::ForgeClient;
use crate::protos::nmx_c::nmx_controller_client::NmxControllerClient;
use crate::{forge_resolver, protos};

/// Formats an error as `"{top}: {root}"` using the deepest source in its chain,
/// since `Display` alone doesn't walk `source()` and would hide the root cause.
fn format_error_chain<E: std::error::Error + ?Sized>(err: &E) -> String {
    // Bound the walk so a cyclic or pathologically deep `source()` chain can't hang us.
    let max_depth = 16;
    let out = err.to_string();
    let source = std::iter::successors(err.source(), |e| e.source())
        .take(max_depth)
        .last()
        .map(|e| e.to_string())
        .unwrap_or_else(|| out.clone());

    if out != source {
        format!("{out}: {source}")
    } else {
        out
    }
}

/// Concrete, non-trait-object wrapper around a transport-level `tower::BoxError` (which includes
/// a request having hit `REQUEST_TIMEOUT`). `ForgeClientT`/`NmxCClientT` carry this as their
/// `BoxCloneService` error type instead of a bare `tower::BoxError` (`Box<dyn Error + Send +
/// Sync>`) alias, because that raw trait-object alias trips a known rustc/async_trait HRTB
/// inference bug ("implementation of `From` is not general enough", rust-lang/rust#102211) in
/// every `#[async_trait]` fn that returns `Result<ForgeClientT, _>` (e.g.
/// `ConnectionProvider::provide_connection`). A concrete newtype sidesteps it.
#[derive(Debug)]
pub struct TransportError(tower::BoxError);

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        // The wrapped error itself, not its own source -- skipping straight to
        // `self.0.source()` would hide `self.0` from any downstream source-chain walk (e.g.
        // tonic's `Status::try_from_error`, which downcasts each link looking for a nested
        // `tonic::Status` to recover its gRPC code). A transparent wrapper must expose what it
        // wraps as the very next link, not jump past it.
        Some(&*self.0)
    }
}

impl From<tower::BoxError> for TransportError {
    fn from(e: tower::BoxError) -> Self {
        Self(e)
    }
}

pub type NmxCClientT = NmxControllerClient<
    BoxCloneService<hyper::Request<Body>, hyper::Response<Incoming>, TransportError>,
>;

pub type ForgeClientT =
    ForgeClient<BoxCloneService<hyper::Request<Body>, hyper::Response<Incoming>, TransportError>>;

/// Default ceiling on connection acquisition + time-to-response-headers for a single gRPC
/// request, applied at the transport layer so a connection that's accepted but never answered
/// can't block a caller (and its admission-retry budget) forever. Overridable via
/// [`ForgeClientConfig::request_timeout`] for callers that need to exceed it (e.g. a
/// server-side `pending_timeout` configured above this default -- see that field's doc comment).
///
/// Doesn't bound the body/trailer phase after headers arrive, and the HTTP/2 keepalive PING
/// above doesn't either: PING is connection-scoped, so a peer can keep acking it while one
/// individual gRPC stream on that connection stalls after sending its headers. Set comfortably
/// above the largest admission-retry cumulative backoff used anywhere in admin-cli (120s) and
/// above legitimate large-batch call latency, so it should never fire against a healthy,
/// just-slow server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

pub const DEFAULT_DOMAIN: &str = "forge.local";

const VRF_NAME: &str = "mgmt";

#[derive(Clone, Debug, Default)]
pub struct ForgeClientConfig {
    pub root_ca_path: String,
    pub client_cert: Option<ClientCert>,
    pub enforce_tls: bool,
    #[cfg(feature = "test-support")]
    pub suppress_insecure_tls_warning: bool,
    pub use_mgmt_vrf: bool,
    pub max_decoding_message_size: Option<usize>,
    /// Overrides [`REQUEST_TIMEOUT`]. `None` (the default) keeps the built-in ceiling. Exists
    /// because `api_admission_control.pending_timeout` (`crates/api-core/src/cfg`) has no
    /// enforced upper bound -- an operator can legally configure a server-side admission wait
    /// longer than the client's default transport timeout, which would otherwise cancel the
    /// call before the server's own deadline.
    ///
    /// [`ForgeClientConfig::new`] seeds this from `FORGE_CLIENT_REQUEST_TIMEOUT_SECS` (whole
    /// seconds; zero and non-numeric values are rejected -- logged and ignored, falling back to
    /// [`REQUEST_TIMEOUT`] -- since a zero timeout would fail every request immediately). A
    /// value assigned directly to this field after construction takes precedence over the
    /// environment, same as every other knob on this struct.
    pub request_timeout: Option<Duration>,
    pub socks_proxy: Option<String>,
    pub connect_retries_max: Option<u32>,
    pub connect_retries_interval: Option<Duration>,
    /// Optional node-auth token provider (issue #355). When set, each request
    /// carries an `Authorization: Bearer <jwt>` — either self-signed with the
    /// client certificate's own private key ([`NodeJwtMinter`]) or fetched
    /// from the dpu-agent's local API (`SocketTokenSource`). Independent of
    /// mTLS: the channel may present a client cert, a token, or both.
    pub node_token_provider: Option<Arc<dyn NodeTokenProvider>>,
}

impl ForgeClientConfig {
    pub fn new(root_ca_path: String, client_cert: Option<ClientCert>) -> Self {
        let mut disabled = std::env::var("DISABLE_TLS_ENFORCEMENT").is_ok();
        if client_cert.is_none() {
            disabled = true;
        }
        let max_decoding_message_size = std::env::var("TONIC_MAX_DECODING_MESSAGE_SIZE")
            .ok()
            .and_then(|ms| ms.parse::<usize>().ok());
        let request_timeout = std::env::var("FORGE_CLIENT_REQUEST_TIMEOUT_SECS")
            .ok()
            .and_then(|raw| match raw.parse::<u64>() {
                Ok(secs) if secs > 0 => Some(Duration::from_secs(secs)),
                _ => {
                    tracing::warn!(
                        env_var = "FORGE_CLIENT_REQUEST_TIMEOUT_SECS",
                        value = %raw,
                        default_secs = REQUEST_TIMEOUT.as_secs(),
                        "ignoring invalid request timeout override, a zero or malformed value \
                         would fail every request immediately"
                    );
                    None
                }
            });

        Self {
            root_ca_path,
            client_cert,
            enforce_tls: !disabled,
            #[cfg(feature = "test-support")]
            suppress_insecure_tls_warning: false,
            use_mgmt_vrf: false,
            max_decoding_message_size,
            request_timeout,
            socks_proxy: None,

            // Default connect retry configuration to start.
            // We can change this if needed, or just make it
            // easier to set at initialization time (callers
            // can also call set_connect_retries_max and
            // set_connect_retries_interval on the ForgeHttpConnector
            // to override).
            //
            // TODO(chet): Really, what would be nice here is,
            // when I go and clean up the previous retry_build
            // stuff, to leverage the prevalance of ApiConfig
            // across the codebase (which has a RetryConfig), and
            // leverage that as the driver for this config, which
            // was the point anyway. It'l be cleaner as a separate
            // MR though, I think.
            connect_retries_max: Some(3),
            connect_retries_interval: Some(Duration::from_secs(20)),
            node_token_provider: None,
        }
    }

    /// Restores server-certificate validation on a config built without a
    /// client certificate.
    ///
    /// [`ForgeClientConfig::new`] disables TLS enforcement whenever
    /// `client_cert` is `None`, which predates node-auth: back then "no client
    /// cert" meant "no credentials at all", and such callers were not expected
    /// to reach the API over verified TLS. A node-token client does hold
    /// credentials — a bearer JWT brokered by the dpu-agent (issue #355) — and
    /// must still authenticate the server against `root_ca_path` rather than
    /// fall back to [`DummyTlsVerifier`]. Callers that never opt in keep the
    /// previous behavior.
    ///
    /// `DISABLE_TLS_ENFORCEMENT` continues to win, so local-development
    /// overrides work the same as they do for mTLS clients.
    #[must_use]
    pub fn require_tls_enforcement(mut self) -> Self {
        self.enforce_tls = std::env::var("DISABLE_TLS_ENFORCEMENT").is_err();
        self
    }

    /// Enables node-auth JWTs: requests built from this config mint and carry
    /// short-lived bearer tokens signed with the configured client cert's key.
    /// A no-op when no client cert is configured.
    #[must_use]
    pub fn with_node_jwt(mut self) -> Self {
        self.node_token_provider = self.client_cert.as_ref().map(|client_cert| {
            NodeJwtMinter::new(client_cert.cert_path.clone(), client_cert.key_path.clone())
                as Arc<dyn NodeTokenProvider>
        });
        self
    }

    /// Attaches an explicit node-auth token provider — e.g. a pre-built
    /// [`NodeJwtMinter`] the caller also serves through the agent's local
    /// API, or a `SocketTokenSource` in a process that holds no key at all.
    ///
    /// Implies [`require_tls_enforcement`](Self::require_tls_enforcement).
    /// A bearer token IS the client's credential, so the server must always
    /// be authenticated before one is handed over — a token-only config
    /// (no client cert) would otherwise sit on [`DummyTlsVerifier`] and
    /// present its token to whatever answered. Enforcing it here rather than
    /// asking every caller to remember the pairing keeps the type hard to
    /// misuse; `DISABLE_TLS_ENFORCEMENT` still wins for local development.
    #[must_use]
    pub fn with_token_provider(mut self, provider: Arc<dyn NodeTokenProvider>) -> Self {
        self.node_token_provider = Some(provider);
        self.require_tls_enforcement()
    }

    /// This is required when using `ForgeTlsConfig` on a DPU to communicate with site-controller.
    /// The mgmt interface exists in the mgmt VRF. `use_mgmt_vrf` sets the
    /// `SO_BINDTODEVICE` socket option on the client socket used when performing DNS queries
    /// and establishing a TCP connection with site-controller.
    pub fn use_mgmt_vrf(self) -> Result<Self, eyre::Report> {
        let ignore_mgmt_vrf = std::env::var("IGNORE_MGMT_VRF").is_ok();

        let use_mgmt_vrf = match ignore_mgmt_vrf {
            true => {
                log::debug!("ignore_mgmt_vrf is {ignore_mgmt_vrf} not using mgmt vrf: {VRF_NAME}");
                false
            }

            false => {
                log::debug!("ignore_mgmt_vrf is {ignore_mgmt_vrf} using mgmt vrf: {VRF_NAME}");
                true
            }
        };

        let max_decoding_message_size = std::env::var("TONIC_MAX_DECODING_MESSAGE_SIZE")
            .ok()
            .and_then(|ms| ms.parse::<usize>().ok());

        let res = Self {
            use_mgmt_vrf,
            max_decoding_message_size,
            ..self
        };

        log::debug!("ForgeClientConfig {res:?}");

        Ok(res)
    }

    pub fn client_cert_expiry(&self) -> Option<i64> {
        if let Some((client_certs, _key)) = self.read_client_cert() {
            if let Some(client_public_key) = client_certs.first() {
                if let Ok((_rem, cert)) = X509Certificate::from_der(client_public_key) {
                    Some(cert.validity.not_after.timestamp())
                } else {
                    None // couldn't parse certificate to x509
                }
            } else {
                None // no cert in client certs vec
            }
        } else {
            None // no certs parsed from disk
        }
    }

    pub fn read_client_cert(
        &self,
    ) -> Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
        if let Some(client_cert) = self.client_cert.as_ref() {
            let certs = {
                let fd = match std::fs::File::open(&client_cert.cert_path) {
                    Ok(fd) => fd,
                    Err(_) => return None,
                };
                let mut buf = std::io::BufReader::new(&fd);

                let mut errors = vec![];

                let valid_certificates = rustls_pemfile::certs(&mut buf)
                    .filter_map(|result| match result {
                        Ok(v) => Some(Ok(v)),
                        Err(err) => match err.kind() {
                            std::io::ErrorKind::InvalidData => {
                                errors.push(err);
                                None
                            }
                            _ => Some(Err(err)),
                        },
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap_or_else(|err| {
                        errors.push(err);
                        vec![]
                    });

                if !errors.is_empty() {
                    tracing::warn!(error = ?errors, "Found error parsing one or more certificates");
                }

                valid_certificates
            };

            let key = {
                let fd = match std::fs::File::open(&client_cert.key_path) {
                    Ok(fd) => fd,
                    Err(_) => return None,
                };
                let mut buf = std::io::BufReader::new(&fd);

                use rustls_pemfile::Item;

                match rustls_pemfile::read_one(&mut buf) {
                    Ok(Some(item)) => match item {
                        Item::Pkcs1Key(key) => Some(key.into()),
                        Item::Pkcs8Key(key) => Some(key.into()),
                        Item::Sec1Key(key) => Some(key.into()),
                        _ => None,
                    },
                    _ => None,
                }
            };

            let key = match key {
                Some(key) => key,
                None => {
                    // tracing::error!("Rustls error: no keys?");
                    return None;
                }
            };

            Some((certs, key))
        } else {
            None
        }
    }

    pub fn socks_proxy(&mut self, socks_proxy: Option<String>) {
        self.socks_proxy = socks_proxy;
    }
}

// RetryConfig is intended to be a generic
// set of parameters used for defining retries.
// Since the use cases right now all seem to fit
// into a fixed retry interval, this supports
// as such. If this ends up evolving into
// something where we also want exponential
// backoff, we can add it.
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    pub retries: u32,
    pub interval: Duration,
}

impl Default for RetryConfig {
    // default returns the default retry configuration,
    // which is 10 second intervals up to 60 times.
    // The initial use case for this was connect failures,
    // where if we're in a situation with connection
    // failures, we don't want to be overly aggressive
    // with retries (but probably want to be persistent).
    fn default() -> Self {
        Self {
            retries: 60,
            interval: Duration::from_secs(10),
        }
    }
}

// ApiConfig holds configuration used to connect
// to a given Carbide API URL, including the client
// configuration itself, as well as retry config.
#[derive(Debug, Clone, Copy)]
pub struct ApiConfig<'a> {
    pub url: &'a str,
    pub additional_urls: &'a [String],
    pub client_config: &'a ForgeClientConfig,
    pub retry_config: RetryConfig,
}

impl<'a> ApiConfig<'a> {
    // new creates a new ApiConfig, for the given
    // Carbide API URL and ForgeClientConfig, with
    // a default retry configuration.
    pub fn new(url: &'a str, client_config: &'a ForgeClientConfig) -> Self {
        Self {
            url,
            additional_urls: &[],
            client_config,
            retry_config: RetryConfig::default(),
        }
    }

    pub fn new_with_multiple_urls(
        url: &'a str,
        additional_urls: &'a [String],
        client_config: &'a ForgeClientConfig,
        retry_config: RetryConfig,
    ) -> Self {
        Self {
            url,
            additional_urls,
            client_config,
            retry_config,
        }
    }

    // with_retry_config allows a caller to set their
    // own RetryConfig beyond the default.
    pub fn with_retry_config(self, retry_config: RetryConfig) -> Self {
        Self {
            url: self.url,
            additional_urls: self.additional_urls,
            client_config: self.client_config,
            retry_config,
        }
    }

    // retry_config converts the generic RetryConfig into the
    // implementation-specific retry type, which as of now is
    // a tryhard::RetryFutureConfig.
    fn retry_config(&self) -> RetryFutureConfig<FixedBackoff, NoOnRetry> {
        RetryFutureConfig::new(self.retry_config.retries).fixed_backoff(self.retry_config.interval)
    }
}

#[derive(Clone, Debug)]
pub struct ForgeTlsClient<'a> {
    forge_client_config: &'a ForgeClientConfig,
}

impl<'a> ForgeTlsClient<'a> {
    pub fn new(forge_client_config: &'a ForgeClientConfig) -> Self {
        Self {
            forge_client_config,
        }
    }

    /// retry_build creates a new ForgeTlsClient from
    /// the given API URL and ForgeClientConfig, then attempts to build
    /// and return a client, integrating retries into the
    /// building attempts.
    pub async fn retry_build(api_config: &ApiConfig<'a>) -> ForgeTlsClientResult<ForgeClientT> {
        // In the retrying function, if the ForgeTlsClient just fails to even build, return _that_
        // error early by putting it in the Ok(Err(e)) variant, so that tryhard doesn't keep
        // retrying a configuration error.
        let result: Result<Result<ForgeClientT, ForgeTlsClientError>, ForgeTlsClientError> =
            tryhard::retry_fn(|| async move {
                let mut client = match ForgeTlsClient::new(api_config.client_config)
                    .build(api_config.url)
                    .await
                {
                    Ok(client) => client,
                    // Don't let tryhard retry this, just push the error into the Ok variant
                    Err(e) => return Ok(Err(e)),
                };

                // The thing we actually want to retry is a test connection
                client
                    .version(tonic::Request::new(VersionRequest {
                        display_config: false,
                    }))
                    .await
                    .inspect_err(|err| {
                        tracing::error!(
                            url = %api_config.url,
                            error = %format_error_chain(err),
                            "Failed to connect client to Forge API; retrying",
                        );
                    })
                    .map_err(|e| ForgeTlsClientError::Connection(format_error_chain(&e)))?;

                // ok, ok
                Ok(Ok(client))
            })
            .with_config(api_config.retry_config())
            .await
            .inspect_err(|err| {
                tracing::error!(
                    url = %api_config.url,
                    retries = api_config.retry_config.retries,
                    error = %err,
                    "Failed to connect client to Forge API after retries",
                );
            });

        match result {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e),
        }
    }

    /// Builds a new Client for for the Forge API which uses a HTTPS/TLS connector
    /// and appropriate certificates for connecting to the API server.
    ///
    /// Note that calling this API will not establish any connection.
    /// The connection attempt happens lazily at the first request.
    /// Note also that if TLS certificates would not change, only a single client
    /// would be required for the whole application - since hyper already manages
    /// connection establishment internally.
    /// However using a fresh client could avoid getting a stale connection from
    /// a pool.
    pub async fn build<S: AsRef<str>>(&self, url: S) -> ForgeTlsClientResult<ForgeClientT> {
        let uri = Uri::from_str(url.as_ref()).map_err(|e| ConfigurationError::InvalidUri {
            uri_string: url.as_ref().to_string(),
            error: e,
        })?;

        // A bearer token is the client's credential, so it may only be sent to
        // a server this client has authenticated. Both ways that can fail are
        // checked here rather than in the builders: `enforce_tls` and
        // `node_token_provider` are public fields, so a caller can clear
        // enforcement after `with_token_provider` or build the struct
        // literally, and the handshake only happens at all for an https:// URL
        // — a plaintext one skips TLS setup entirely while `BearerAuthService`
        // below still stamps the token. `DISABLE_TLS_ENFORCEMENT` is honored
        // for parity with the rest of this config so local development works.
        if self.forge_client_config.node_token_provider.is_some()
            && std::env::var("DISABLE_TLS_ENFORCEMENT").is_err()
        {
            if uri.scheme() != Some(&tonic::codegen::http::uri::Scheme::HTTPS) {
                return Err(ConfigurationError::BearerTokenOverPlaintext {
                    uri_string: url.as_ref().to_string(),
                }
                .into());
            }
            if !self.forge_client_config.enforce_tls {
                return Err(ConfigurationError::BearerTokenWithoutTlsEnforcement {
                    uri_string: url.as_ref().to_string(),
                }
                .into());
            }
        }

        let connector = self.build_https_client(url.as_ref()).await?;

        // ping interval + ping timeout should add up to less than tcp_user_timeout,
        // so that the application gets a chance to fix things before the kernel.
        let hyper_client = legacy::Client::builder(TokioExecutor::new())
            .http2_only(true)
            // Send a PING frame every this
            .http2_keep_alive_interval(Some(Duration::from_secs(10)))
            // The server will have this much time to respond with a PONG
            .http2_keep_alive_timeout(Duration::from_secs(15))
            // Send PING even when no active http2 streams
            .http2_keep_alive_while_idle(true)
            // How many connections will be kept open, per host.
            // We never make more than a single connection to carbide at a time.
            .pool_max_idle_per_host(2)
            .timer(TokioTimer::new())
            .build(connector);
        // Stamp a freshly-minted node-auth bearer token onto each request when
        // configured (issue #355). A `None` minter is a transparent pass-through,
        // so the boxed service type is identical in both modes.
        let hyper_client = BearerAuthService::new(
            hyper_client,
            self.forge_client_config.node_token_provider.clone(),
        );
        // Inject the issuing span's W3C trace context into every request this client sends
        // (issue #2438). Wrapping before `boxed_clone` keeps the erased `BoxCloneService` type.
        let hyper_client = trace_propagation::TraceInjectService::new(hyper_client);
        let request_timeout = self
            .forge_client_config
            .request_timeout
            .unwrap_or(REQUEST_TIMEOUT);
        let hyper_client: BoxCloneService<
            hyper::Request<Body>,
            hyper::Response<Incoming>,
            TransportError,
        > = tower::timeout::Timeout::new(hyper_client, request_timeout)
            .map_err(TransportError)
            .boxed_clone();

        let mut forge_client = ForgeClient::with_origin(hyper_client, uri);

        if let Some(max_decoding_message_size) = self.forge_client_config.max_decoding_message_size
        {
            forge_client = forge_client.max_decoding_message_size(max_decoding_message_size);
        }

        Ok(forge_client)
    }

    pub async fn build_https_client<S: AsRef<str>>(
        &self,
        url: S,
    ) -> ForgeHttpsClientResult<hyper_rustls::HttpsConnector<ForgeHttpConnector>> {
        let mut roots = RootCertStore::empty();
        let uri = Uri::from_str(url.as_ref()).map_err(|e| ConfigurationError::InvalidUri {
            uri_string: url.as_ref().to_string(),
            error: e,
        })?;

        // Only check the root and client certs if the uri we were given is actually HTTPS.
        // That lets tests and plaintext-HTTP environments function properly.
        if let Some(scheme) = uri.scheme()
            && scheme == &tonic::codegen::http::uri::Scheme::HTTPS
        {
            // TODO: by reading the pemfile every time, we're automatically getting hot-reload
            // TODO: -- but we could use inotify in order to make this more performant.
            match tokio::fs::read(&self.forge_client_config.root_ca_path).await {
                Ok(pem_file) => {
                    let mut cert_cursor = std::io::Cursor::new(&pem_file[..]);
                    let (_added, _ignored) = roots.add_parsable_certificates(
                        rustls_pemfile::certs(&mut cert_cursor).filter_map(|cert| cert.ok()),
                    );
                }
                Err(error) => {
                    return Err(CouldNotReadRootCa {
                        path: self.forge_client_config.root_ca_path.clone(),
                        error,
                    }
                    .into());
                }
            }

            if let Some(cert_expiry) = self.forge_client_config.client_cert_expiry() {
                let start = SystemTime::now();
                let current_time = start
                    .duration_since(UNIX_EPOCH)
                    .expect("Time went backwards");
                if u64::try_from(cert_expiry)
                    .ok()
                    .is_none_or(|v| current_time.as_secs() > v)
                {
                    tracing::error!(
                        "Client certificate is expired, perhaps you need to regenerate your cert?"
                    );
                    return Err(ConfigurationError::InvalidClientCert(
                        rustls::Error::InvalidCertificate(rustls::CertificateError::Expired),
                    )
                    .into());
                }
            }
        }

        let base_config_builder = || {
            ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::aws_lc_rs::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
        };

        let tls = {
            let builder = || {
                if self.forge_client_config.enforce_tls {
                    base_config_builder().with_root_certificates(roots)
                } else {
                    #[cfg(feature = "test-support")]
                    let verifier = if self.forge_client_config.suppress_insecure_tls_warning {
                        DummyTlsVerifier::new_for_tests()
                    } else {
                        DummyTlsVerifier::new_for_prod()
                    };
                    #[cfg(not(feature = "test-support"))]
                    let verifier = DummyTlsVerifier::new_for_prod();
                    base_config_builder()
                        .dangerous()
                        .with_custom_certificate_verifier(Arc::new(verifier))
                }
            };

            if let Some((certs, key)) = self.forge_client_config.read_client_cert() {
                builder()
                    .with_client_auth_cert(certs, key)
                    .map_err(ConfigurationError::InvalidClientCert)?
            } else {
                builder().with_no_client_auth()
            }
        };

        let forge_resolv_config =
            forge_resolver::resolver::ForgeResolveConf::with_system_resolv_conf()
                .map_err(ConfigurationError::Resolver)?;
        let forge_resolver_config = forge_resolver::resolver::into_forge_resolver_config(
            forge_resolv_config.parsed_configuration(),
        )
        .map_err(ConfigurationError::Resolver)?;

        let resolver_config = ResolverConfig::from_parts(
            forge_resolver_config.0.domain,
            forge_resolver_config.0.search_domain,
            forge_resolver_config.0.inner,
        );
        // Five seconds is the default, but setting anyway for documentation and future proofing
        let mut resolver_opts = ForgeResolverOpts::default().timeout(Duration::from_secs(5));
        if self.forge_client_config.use_mgmt_vrf {
            resolver_opts = resolver_opts.use_mgmt_vrf();
        }
        let resolver = ForgeResolver::with_config_and_options(resolver_config, resolver_opts);
        let mut http = ForgeHttpConnector::new_with_resolver(resolver);
        if self.forge_client_config.use_mgmt_vrf {
            http.set_interface("mgmt".to_string());
        }
        http.set_socks5_proxy(self.forge_client_config.socks_proxy.clone());
        http.enforce_http(false);

        // Wait this long for `connect` syscall to return.
        // Hyper implements this by wrapping the call in `tokio::time::timeout`.
        http.set_connect_timeout(Some(Duration::from_secs(5)));

        // Set TCP timeouts. The interactions are non-obvious, but here are the basics:
        // - An established socket with in-flight data will timeout exactly TCP_USER_TIMEOUT
        // after data is first lost.
        // - An idle socket will send its first probe when it's been idle for TCP_KEEPIDLE. If
        // the probe is not ACKed, it will timeout about TCP_USER_TIMEOUT after first data loss.
        // - This formula should be maintained: TCP_USER_TIMEOUT < TCP_KEEPIDLE + TCP_KEEPINTVL * TCP_KEEPCNT
        // where `<` means "just slightly lower than".
        //
        // The values below mean:
        // - Disconnect broken active sockets after 30s
        // - Disconnect broken idle sockets after 32s (first retry wakeup that's > tcp_user_time)
        //
        // If HTTP/2 PING (further down) is working the keepalive should never trigger, but if tokio borks the
        // kernel should unwedge the socket.
        //
        // All the details: https://blog.cloudflare.com/when-tcp-sockets-refuse-to-die/
        http.set_tcp_user_timeout(Some(Duration::from_secs(30)));
        http.set_keepalive_time(Some(Duration::from_secs(20))); // TCP_KEEPIDLE
        http.set_keepalive_interval(Some(Duration::from_secs(4)));
        http.set_keepalive_retries(Some(3)); // initial probe at 20s, then 24s, 28s and 32s

        http.set_connect_retries_max(self.forge_client_config.connect_retries_max);
        http.set_connect_retries_interval(self.forge_client_config.connect_retries_interval);

        let connector = tower::ServiceBuilder::new()
            .layer_fn(move |s| {
                let tls = tls.clone();

                hyper_rustls::HttpsConnectorBuilder::new()
                    .with_tls_config(tls)
                    .https_or_http()
                    .enable_http2()
                    .wrap_connector(s)
            })
            .service(http);
        Ok(connector)
    }

    /// Builds a new Client for the NMX-C API which uses a HTTPS/TLS connector
    ///
    /// Note that calling this API will not establish any connection.
    /// The connection attempt happens lazily at the first request.
    /// Note also that if TLS certificates would not change, only a single client
    /// would be required for the whole application - since hyper already manages
    /// connection establishment internally.
    /// However using a fresh client could avoid getting a stale connection from
    /// a pool.
    pub async fn build_nmx_c_client<S: AsRef<str>>(
        &self,
        url: S,
    ) -> ForgeTlsClientResult<NmxCClientT> {
        let uri = Uri::from_str(url.as_ref()).map_err(|e| ConfigurationError::InvalidUri {
            uri_string: url.as_ref().to_string(),
            error: e,
        })?;
        let connector = self.build_https_client(url.as_ref()).await?;

        // ping interval + ping timeout should add up to less than tcp_user_timeout,
        // so that the application gets a chance to fix things before the kernel.
        let hyper_client = legacy::Client::builder(TokioExecutor::new())
            .http2_only(true)
            // Send a PING frame every this
            .http2_keep_alive_interval(Some(Duration::from_secs(10)))
            // The server will have this much time to respond with a PONG
            .http2_keep_alive_timeout(Duration::from_secs(15))
            // Send PING even when no active http2 streams
            .http2_keep_alive_while_idle(true)
            // How many connections will be kept open, per host.
            // We never make more than a single connection to carbide at a time.
            .pool_max_idle_per_host(2)
            .timer(TokioTimer::new())
            .build(connector);
        // Inject the issuing span's W3C trace context into every request this client sends
        // (issue #2438). Wrapping before `boxed_clone` keeps the erased `BoxCloneService` type.
        let hyper_client = trace_propagation::TraceInjectService::new(hyper_client);
        let request_timeout = self
            .forge_client_config
            .request_timeout
            .unwrap_or(REQUEST_TIMEOUT);
        let hyper_client: BoxCloneService<
            hyper::Request<Body>,
            hyper::Response<Incoming>,
            TransportError,
        > = tower::timeout::Timeout::new(hyper_client, request_timeout)
            .map_err(TransportError)
            .boxed_clone();

        let mut nmx_c_client = NmxControllerClient::with_origin(hyper_client, uri);

        if let Some(max_decoding_message_size) = self.forge_client_config.max_decoding_message_size
        {
            nmx_c_client = nmx_c_client.max_decoding_message_size(max_decoding_message_size);
        }

        Ok(nmx_c_client)
    }

    pub async fn retry_build_nmx_c(
        api_config: &ApiConfig<'a>,
    ) -> ForgeTlsClientResult<NmxCClientT> {
        // In the retrying function, if the ForgeTlsClient just fails to even build, return _that_
        // error early by putting it in the Ok(Err(e)) variant, so that tryhard doesn't keep
        // retrying a configuration error.
        let result: Result<Result<NmxCClientT, ForgeTlsClientError>, ForgeTlsClientError> =
            tryhard::retry_fn(|| async move {
                let mut client = match ForgeTlsClient::new(api_config.client_config)
                    .build_nmx_c_client(api_config.url)
                    .await
                {
                    Ok(client) => client,
                    // Don't let tryhard retry this, just push the error into the Ok variant
                    Err(e) => return Ok(Err(e)),
                };

                // The thing we actually want to retry is a test connection
                client
                    .hello(tonic::Request::new(protos::nmx_c::ClientHello {
                        gateway_id: "".to_string(),
                        major_version: i32::from(
                            protos::nmx_c::ProtoMsgMajorVersion::ProtoMsgMajorVersion,
                        ),
                        minor_version: i32::from(
                            protos::nmx_c::ProtoMsgMinorVersion::ProtoMsgMinorVersion,
                        ),
                    }))
                    .await
                    .inspect_err(|err| {
                        tracing::error!(
                            url = %api_config.url,
                            error = %format_error_chain(err),
                            "Failed to connect client to NMX-C API; retrying",
                        );
                    })
                    .map_err(|e| ForgeTlsClientError::Connection(format_error_chain(&e)))?;

                // ok, ok
                Ok(Ok(client))
            })
            .with_config(api_config.retry_config())
            .await
            .inspect_err(|err| {
                tracing::error!(
                    url = %api_config.url,
                    retries = api_config.retry_config.retries,
                    error = %err,
                    "Failed to connect client to NMX-C API after retries",
                );
            });

        match result {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ForgeTlsClientError {
    #[error("ConnectError error: {0}")]
    Connection(String),
    #[error("configuration error: {0}")]
    Configuration(#[from] ConfigurationError),
}

#[derive(thiserror::Error, Debug)]
pub enum ConfigurationError {
    #[error("invalid URI {uri_string}: {error}")]
    InvalidUri {
        uri_string: String,
        error: hyper::http::uri::InvalidUri,
    },
    #[error("could not read root CA cert at {path}: {error}")]
    CouldNotReadRootCa { path: String, error: io::Error },
    #[error(
        "refusing to send node-auth bearer tokens to non-HTTPS URL {uri_string}: \
         the token would travel in cleartext"
    )]
    BearerTokenOverPlaintext { uri_string: String },
    #[error(
        "refusing to send node-auth bearer tokens to {uri_string} with TLS enforcement \
         disabled: the server's certificate would not be verified, so the token could \
         be handed to an impersonator"
    )]
    BearerTokenWithoutTlsEnforcement { uri_string: String },
    #[error("invalid client cert: {0}")]
    InvalidClientCert(rustls::Error),
    #[error("error configuring resolver: {0}")]
    Resolver(#[from] ResolverError),
}

impl From<ForgeTlsClientError> for tonic::Status {
    fn from(value: ForgeTlsClientError) -> Self {
        tonic::Status::unavailable(value.to_string())
    }
}

pub type ForgeTlsClientResult<T> = Result<T, ForgeTlsClientError>;
pub type ForgeHttpsClientResult<T> = Result<T, ForgeTlsClientError>;

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::net::SocketAddr;

    use carbide_test_support::value_scenarios;
    use forge_http_connector::connector::ConnectorMetrics;
    use hyper_rustls::HttpsConnector;

    use super::*;

    /// A node-token client presents no client certificate, which by itself
    /// drops the channel onto `DummyTlsVerifier`. `require_tls_enforcement`
    /// puts it back on root-CA validation.
    #[test]
    fn require_tls_enforcement_restores_validation_without_a_client_cert() {
        if std::env::var("DISABLE_TLS_ENFORCEMENT").is_ok() {
            // The override wins by design; the assertions below would not hold.
            return;
        }

        let config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None);
        assert!(
            !config.enforce_tls,
            "a config without a client cert starts out unenforced"
        );

        let config = config.require_tls_enforcement();
        assert!(
            config.enforce_tls,
            "opting in must restore server certificate validation"
        );
    }

    #[derive(Debug)]
    struct FixedToken;

    impl NodeTokenProvider for FixedToken {
        fn current(&self) -> Option<String> {
            Some("a.b.c".to_string())
        }
    }

    /// The bearer token IS the credential, so the server must be authenticated
    /// before one is sent. Callers must not have to remember to pair
    /// `with_token_provider` with `require_tls_enforcement` — forgetting it
    /// would hand tokens to whatever answered the connection.
    #[test]
    fn with_token_provider_enforces_tls_without_being_asked() {
        if std::env::var("DISABLE_TLS_ENFORCEMENT").is_ok() {
            // The override wins by design; the assertion below would not hold.
            return;
        }

        let config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None)
            .with_token_provider(Arc::new(FixedToken));

        assert!(
            config.enforce_tls,
            "attaching a token provider must restore server validation by itself"
        );
    }

    /// TLS setup only runs for an https:// URL, so `enforce_tls` alone does not
    /// cover a plaintext one — the token would still be stamped and travel in
    /// the clear. Building such a client has to fail.
    #[tokio::test]
    async fn bearer_tokens_are_refused_over_plaintext() {
        if std::env::var("DISABLE_TLS_ENFORCEMENT").is_ok() {
            // The override deliberately permits plaintext development.
            return;
        }

        let config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None)
            .with_token_provider(Arc::new(FixedToken));

        let error = ForgeTlsClient::new(&config)
            .build("http://carbide-api.local:8080")
            .await
            .expect_err("a token client must refuse a plaintext endpoint");

        assert!(
            error.to_string().contains("cleartext"),
            "the error should explain the refusal, got: {error}"
        );
    }

    /// `enforce_tls` and `node_token_provider` are both public, so the builder
    /// pairing is not the last word: a caller can clear enforcement afterwards
    /// or build the struct literally. Over https:// that still selects
    /// `DummyTlsVerifier`, so the check has to live at client construction.
    #[tokio::test]
    async fn bearer_tokens_are_refused_when_tls_enforcement_is_cleared() {
        if std::env::var("DISABLE_TLS_ENFORCEMENT").is_ok() {
            // The override deliberately permits unverified development setups.
            return;
        }

        let mut config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None)
            .with_token_provider(Arc::new(FixedToken));
        // Exactly what a caller ordering the builders the other way round, or
        // assigning the public field, would end up with.
        config.enforce_tls = false;

        let error = ForgeTlsClient::new(&config)
            .build("https://carbide-api.local:8080")
            .await
            .expect_err("an unverified token client must be refused");

        assert!(
            error.to_string().contains("TLS enforcement disabled"),
            "the error should explain the refusal, got: {error}"
        );
    }

    /// The same client over https:// must still build — the guard above is
    /// scoped to the scheme, not to token clients in general.
    #[tokio::test]
    async fn bearer_tokens_are_allowed_over_https() {
        let config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None)
            .with_token_provider(Arc::new(FixedToken));

        // Reaching a root-CA read means the plaintext guard let it through;
        // the file itself need not exist in a unit test.
        let result = ForgeTlsClient::new(&config)
            .build("https://carbide-api.local:8080")
            .await;

        if let Err(error) = result {
            assert!(
                !error.to_string().contains("cleartext"),
                "https must not trip the plaintext guard, got: {error}"
            );
        }
    }

    #[tokio::test]
    // test_max_retries builds up an instance of hyper client using
    // the ForgeHttpConnector, which is the same configuration used
    // for creating a ForgeTlsClient. In this case, it is NOT
    // used to create a ForgeTlsClient, but instead is used directly
    // to make an HTTP call (so we maintain access to the underlying
    // connector for querying retry count.
    async fn test_max_retries() {
        let max_retries = 3; // 4 total attempts

        // Set up all of the resolver config stuff
        // to pass to the ForgeHttpConnector.
        let forge_resolv_config =
            forge_resolver::resolver::ForgeResolveConf::with_system_resolv_conf().unwrap();
        let forge_resolver_config = forge_resolver::resolver::into_forge_resolver_config(
            forge_resolv_config.parsed_configuration(),
        )
        .unwrap();

        let resolver_config = ResolverConfig::from_parts(
            forge_resolver_config.0.domain,
            forge_resolver_config.0.search_domain,
            forge_resolver_config.0.inner,
        );

        let resolver_opts = ForgeResolverOpts::default().timeout(Duration::from_secs(5));
        let resolver = ForgeResolver::with_config_and_options(resolver_config, resolver_opts);

        // Create the ConnectorMetrics instance used for
        // collecting some stats for connections that go
        // through the ForgeHttpConnector.
        let mut metrics = ConnectorMetrics::default();

        // Now create the ForgeHttpConnector, setting our
        // test-specific `max_retries` with a 1 second interval,
        // and passing it our Connectormetrics.
        let mut http = ForgeHttpConnector::new_with_resolver(resolver);
        http.set_connect_retries_max(Some(max_retries));
        http.set_connect_retries_interval(Some(Duration::from_secs(1)));
        http.set_metrics(metrics.clone());

        // And now make our new connector, which is an
        // implementation of tower_service::Service.
        let connector = tower::ServiceBuilder::new()
            .layer_fn(move |s| {
                let tls = ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::aws_lc_rs::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DummyTlsVerifier::new_for_tests()))
                .with_no_client_auth();

                hyper_rustls::HttpsConnectorBuilder::new()
                    .with_tls_config(tls)
                    .https_or_http()
                    .enable_http2()
                    .wrap_connector(s)
            })
            .service(http);

        // And then create a new hyper HTTP client with the connector.
        let hyper_client: legacy::Client<HttpsConnector<ForgeHttpConnector>, Body> =
            legacy::Client::builder(TokioExecutor::new()).build(connector);

        // We're finally here. Fire off an HTTP request. Behind he scenes,
        // the ForgeHttpConnector is going to attempt to connect, fail, and
        // subsequently fire off 3 retries. This assumes you don't have
        // anything listening on :12345. If you do, this test will obviously
        // fail, because the connection will be successful. :P
        let uri = "http://localhost:12345".parse::<Uri>().unwrap();
        let _ = hyper_client.get(uri).await;

        // If you're curious to see what metrics are collected,
        // uncomment this when you run the test with --nocapture.
        // println!("{:?}", metrics.lock(unwrapped.attempts_by_addr.get).unwrap());

        // Make sure attempts, errors, and successes are all as expected.
        assert_eq!(metrics.get_total_attempts(), max_retries + 1);
        assert_eq!(metrics.get_total_errors(), max_retries + 1);
        assert_eq!(metrics.get_total_successes(), 0);

        // And make sure by_addr metrics are working as well. This
        // assumes localhost resolves to 127.0.0.1.
        let addr = SocketAddr::from_str("127.0.0.1:12345").unwrap();
        let attempts_for_addr = metrics.get_attempts_for_addr(&addr);
        let successes_for_addr = metrics.get_successes_for_addr(&addr);
        let errors_for_addr = metrics.get_errors_for_addr(&addr);

        assert!(attempts_for_addr.is_some());
        assert!(errors_for_addr.is_some());

        // This one *is* none!
        assert!(successes_for_addr.is_none());

        assert_eq!(attempts_for_addr.unwrap(), max_retries + 1);
        assert_eq!(errors_for_addr.unwrap(), max_retries + 1);
    }

    #[test]
    fn format_error_chain_formats_each_error() {
        #[derive(thiserror::Error, Debug)]
        #[error("invalid peer certificate: UnknownIssuer")]
        struct Inner;

        #[derive(thiserror::Error, Debug)]
        #[error("client error (connect)")]
        struct Outer(#[from] Inner);

        #[derive(thiserror::Error, Debug)]
        #[error("only message")]
        struct Plain;

        // `format_error_chain` appends the deepest `source()` when it differs
        // from the top-level message, otherwise returns the top message alone.
        value_scenarios!(
            run = |err| format_error_chain(err.as_ref());
            "walks source chain to the root cause" {
                Box::new(Outer::from(Inner)) as Box<dyn std::error::Error> => "client error (connect): invalid peer certificate: UnknownIssuer"
                .to_string(),
            }

            "no source returns top message" {
                Box::new(Plain) as Box<dyn std::error::Error> => "only message".to_string(),
            }
        );
    }

    /// Binds a listener that accepts TCP connections and then holds each one open forever
    /// without writing a single byte back -- no HTTP/2 preface ack, no headers, nothing. Used to
    /// prove `REQUEST_TIMEOUT`/`request_timeout` actually bounds a real RPC against a peer that
    /// never answers, not just client construction.
    async fn spawn_silent_peer() -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binding a loopback listener must not fail in a test");
        let addr = listener
            .local_addr()
            .expect("bound listener has a local addr");
        tokio::spawn(async move {
            // Hold each socket open (never write, never close) so the client's read side
            // blocks indefinitely -- exactly the "accepted but never answered" scenario
            // REQUEST_TIMEOUT exists for. Leaking each connection is fine: the listener task,
            // and every connection it's holding, dies with the test process.
            while let Ok((socket, _)) = listener.accept().await {
                std::mem::forget(socket);
            }
        });
        addr
    }

    /// A silent peer must not hang a Forge RPC forever -- `request_timeout` (here overridden far
    /// below the 300s default so the test itself stays fast) has to fire and return an error
    /// within roughly that bound.
    #[tokio::test]
    async fn version_call_times_out_against_a_silent_peer() {
        let addr = spawn_silent_peer().await;
        let mut config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None);
        config.request_timeout = Some(Duration::from_millis(300));

        let mut client = ForgeTlsClient::new(&config)
            .build(format!("http://{addr}"))
            .await
            .expect("building a client against a plaintext loopback URL must not fail");

        let started = tokio::time::Instant::now();
        let result = client
            .version(tonic::Request::new(VersionRequest {
                display_config: false,
            }))
            .await;
        let elapsed = started.elapsed();

        let error = result.expect_err("a peer that never answers must not yield a response");
        // Bounded on both ends and keyed to the timeout's own error text -- an upper bound
        // alone would also pass for an unrelated early transport/handshake failure, which
        // wouldn't prove `request_timeout` is what's actually stopping the call.
        assert!(
            elapsed >= Duration::from_millis(250) && elapsed < Duration::from_secs(5),
            "expected the 300ms request_timeout override to bound the call, took {elapsed:?}: {error}"
        );
        assert!(
            format_error_chain(&error).contains("timed out"),
            "the failure must come from the transport deadline (tower::timeout::error::Elapsed), got: {error}"
        );
    }

    /// Same guarantee as `version_call_times_out_against_a_silent_peer`, for the NMX-C client
    /// built by `build_nmx_c_client` -- CodeRabbit review r3962056313 asked for coverage on both
    /// builders, not just the Forge one, since they duplicate the same timeout-wrapping logic.
    #[tokio::test]
    async fn hello_call_times_out_against_a_silent_peer() {
        let addr = spawn_silent_peer().await;
        let mut config = ForgeClientConfig::new("/etc/carbide/root-ca.pem".to_string(), None);
        config.request_timeout = Some(Duration::from_millis(300));

        let mut client = ForgeTlsClient::new(&config)
            .build_nmx_c_client(format!("http://{addr}"))
            .await
            .expect("building a client against a plaintext loopback URL must not fail");

        let started = tokio::time::Instant::now();
        let result = client
            .hello(tonic::Request::new(protos::nmx_c::ClientHello {
                gateway_id: String::new(),
                major_version: i32::from(protos::nmx_c::ProtoMsgMajorVersion::ProtoMsgMajorVersion),
                minor_version: i32::from(protos::nmx_c::ProtoMsgMinorVersion::ProtoMsgMinorVersion),
            }))
            .await;
        let elapsed = started.elapsed();

        let error = result.expect_err("a peer that never answers must not yield a response");
        // Bounded on both ends and keyed to the timeout's own error text -- an upper bound
        // alone would also pass for an unrelated early transport/handshake failure, which
        // wouldn't prove `request_timeout` is what's actually stopping the call.
        assert!(
            elapsed >= Duration::from_millis(250) && elapsed < Duration::from_secs(5),
            "expected the 300ms request_timeout override to bound the call, took {elapsed:?}: {error}"
        );
        assert!(
            format_error_chain(&error).contains("timed out"),
            "the failure must come from the transport deadline (tower::timeout::error::Elapsed), got: {error}"
        );
    }

    /// `TransportError::source()` must expose the wrapped error itself as the next link in the
    /// chain, not skip past it to *its* source -- otherwise a `tonic::Status` boxed inside
    /// `tower::BoxError` (as tonic's own transport-error path can produce) becomes unreachable
    /// to anything walking `Error::source()`, e.g. tonic's own `Status::try_from_error`, which
    /// downcasts each link looking for a nested `Status` to recover its original gRPC code.
    #[test]
    fn transport_error_source_exposes_the_wrapped_error() {
        let status = tonic::Status::unavailable("backend down");
        let boxed: tower::BoxError = Box::new(status.clone());
        let wrapped = TransportError::from(boxed);

        let source = wrapped
            .source()
            .expect("a non-empty TransportError must expose its wrapped error as its source");
        let recovered = source
            .downcast_ref::<tonic::Status>()
            .expect("the wrapped tonic::Status must be reachable via source(), not skipped over");
        assert_eq!(recovered.code(), status.code());
        assert_eq!(recovered.message(), status.message());
    }
}
