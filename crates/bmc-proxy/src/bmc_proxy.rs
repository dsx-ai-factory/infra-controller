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
use std::net::{AddrParseError, IpAddr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::middleware::{Next, from_fn_with_state};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use bytes::Bytes;
use carbide_authn::SpiffeContext;
use carbide_authn::middleware::{
    AuthContext, Authorization, CertDescriptionMiddleware, ConnectionAttributes, Principal,
};
use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use carbide_utils::HostPortPair;
use forge_tls::client_config::ClientCert;
use futures::future::BoxFuture;
use http::uri::PathAndQuery;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri, header,
};
use http_body_util::LengthLimitError;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use mac_address::{MacAddress, MacParseError};
use moka::future::Cache as MokaCache;
use rpc::forge;
use rpc::forge::find_bmc_ips_request::LookupBy;
use rpc::forge_api_client::ForgeApiClient;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, rustls};
use tokio_util::sync::CancellationToken;
use tower_http::add_extension::AddExtensionLayer;
use trace_propagation::{is_propagated_header, set_span_parent_from_headers};
use tracing::Instrument;

use crate::cache::{
    CacheKey, CachePolicy, CachedResponse, FetchOutcome, FetchRequest, Freshness, HoldOffReason,
    ResponseCache, UpstreamReply, is_cacheable_query,
};
use crate::class::{DEFAULT_UPSTREAM_TIMEOUT, RequestClass};
use crate::config::{AuthConfig, TlsConfig};
use crate::metrics::{
    AuthContextMissing, CacheLookupCompleted, CacheOutcome, MethodLabel, PrincipalAllowListDenied,
    RequestAclDenied, UpstreamAuthRetried, UpstreamRequestCompleted, UpstreamStatus,
};
use crate::span_isolation::SpanIsolationMiddleware;

const TLS_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Redfish's session token header, per DMTF. Applied on egress and stripped
/// on ingress by [`copy_request_headers`].
const REDFISH_AUTH_TOKEN_HEADER: &str = "X-Auth-Token";
/// Bodies up to this size are buffered and forwarded with the exact framing
/// BMCs have always seen. Anything larger -- Redfish multipart firmware
/// pushes, mainly -- is streamed through instead of being rejected, which the
/// old 8 MiB hard cap did. (8 MiB matches nginx ingress controller defaults.)
const MAX_BUFFERED_BODY_SIZE: usize = 8 * 1024 * 1024;
/// How long a request whose stored response can stand in waits for a fetch
/// before that response is served instead. The fetch continues on its own
/// task; the wait only decides what this caller sees. Callers' own timeouts
/// are longer than this, and a BMC that is slow to fail should not make them
/// wait for the failure when a usable response is at hand.
const SLOW_FETCH_FALLBACK_WAIT: Duration = Duration::from_secs(10);
/// Floor transfer rate used to scale a streamed upload's timeout from its
/// declared size, mirroring libredfish's firmware-upload heuristic. The
/// shared client's 60-second default would abort any large image mid-push.
const MIN_UPLOAD_BANDWIDTH_BYTES_PER_SEC: u64 = 10_000;
/// Ceiling on a streamed upload's scaled timeout. The declared length is
/// caller-supplied, so without a cap a lying `Content-Length` would let a
/// stalled request pin a proxy task and a BMC connection indefinitely. Four
/// hours covers any real firmware image at the floor rate.
const MAX_UPLOAD_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
/// Response header naming how the response cache answered a `GET` in a cached
/// class. Its values are the `outcome` label of the cache lookup metric.
const CACHE_STATUS_HEADER: HeaderName = HeaderName::from_static("x-nico-cache");
/// Upper bound on stored response bodies per replica. Bodies are at most
/// [`MAX_BUFFERED_BODY_SIZE`] each, and the fleet's cacheable resources are
/// small JSON documents, so this leaves the pod's memory limit untouched.
const RESPONSE_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(thiserror::Error, Debug)]
pub(crate) enum BmcProxyError {
    #[error("error resolving BMC information through carbide API: {0}")]
    Api(String),
    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("internal error proxying request: {0}")]
    InternalProxying(String),
    #[error("no credentials found for BMC IP address: {0}")]
    NoCredentials(IpAddr),
    #[error("error spawning listener: {0}")]
    Listen(std::io::Error),
    #[error("error loading TLS config: {0}")]
    TlsConfig(String),
}

pub(crate) struct BmcProxyParams {
    pub(crate) config: Arc<crate::Config>,
}

#[derive(Clone)]
struct BmcProxyState {
    config: Arc<crate::Config>,
    api_client: ForgeApiClient,
    credential_cache: CredentialCache,
    /// One client for every upstream: reqwest pools connections per host
    /// internally, so per-BMC clients bought nothing and grew without bound.
    http_client: reqwest_middleware::ClientWithMiddleware,
    ip_cache: LookupToIpCache,
    /// Stored `GET` responses for request classes with a cache policy.
    response_cache: Arc<ResponseCache>,
}

/// Cached BMC credentials by IP. Correctness comes from the 401/403 eviction
/// in [`proxy_request`]. Expiry is idle-based, not lifetime-based: an entry
/// a caller keeps using stays served even through a long nico-api outage
/// (the availability the pre-cache-bound proxy provided), while an entry for
/// a machine that stopped existing falls out once nothing asks for it. The
/// capacity bounds memory.
type CredentialCache = MokaCache<IpAddr, BmcCredentials>;
/// Resolved `Forwarded: mac=`/`serial=` targets. A BMC that moves to a new
/// IP produces connection errors, not 401s, so no request-path signal evicts
/// these -- the TTL is what heals a stale resolution.
type LookupToIpCache = MokaCache<LookupBy, IpAddr>;

/// How long a resolved BMC IP may be served before the API is asked again.
const IP_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// How long unused cached credentials linger before falling out.
const CREDENTIAL_CACHE_IDLE_TTL: Duration = Duration::from_secs(60 * 60);
/// Upper bound on cached entries; sized far above any realistic BMC fleet.
const CACHE_MAX_ENTRIES: u64 = 100_000;

fn bounded_cache<K, V>(ttl: Duration) -> MokaCache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    MokaCache::builder()
        .time_to_live(ttl)
        .max_capacity(CACHE_MAX_ENTRIES)
        .build()
}

/// Like [`bounded_cache`], but expiry counts from last use rather than from
/// insertion, so entries under active traffic never expire.
fn idle_bounded_cache<K, V>(idle_ttl: Duration) -> MokaCache<K, V>
where
    K: std::hash::Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    MokaCache::builder()
        .time_to_idle(idle_ttl)
        .max_capacity(CACHE_MAX_ENTRIES)
        .build()
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum ForwardedTarget<'a> {
    Ip(IpAddr),
    Mac(MacAddress),
    Serial(&'a str),
}

#[derive(thiserror::Error, Debug)]
enum ForwardedHeaderParseError {
    #[error("invalid IP in forwarded host header: {0}")]
    Ip(#[from] AddrParseError),
    #[error("invalid MAC address in forwarded host header: {0}")]
    Mac(#[from] MacParseError),
}

impl BmcProxyState {
    /// The request's principal identifiers when its ACLs allow the request,
    /// or `None` after recording the denial.
    fn authorized_principals(&self, request: &Request<Body>) -> Option<Vec<String>> {
        let Some(auth_context) = request.extensions().get::<AuthContext<()>>() else {
            emit(AuthContextMissing::RequestAcl {
                method_label: request.method().into(),
            });
            return None;
        };

        let principal_ids = request_principal_ids(auth_context);
        let allowed = principal_ids.iter().any(|principal| {
            self.config
                .auth
                .acls
                .allows(principal, request.method(), request.uri().path())
        });

        if !allowed {
            emit(RequestAclDenied::new(
                request.method(),
                format!("{principal_ids:?}"),
                request.uri().path().to_string(),
            ));
            return None;
        }

        Some(principal_ids)
    }
}

pub(crate) async fn start(
    params: BmcProxyParams,
    cancel_token: CancellationToken,
    join_set: &mut JoinSet<()>,
) -> Result<(), BmcProxyError> {
    // Destructure params to save typing
    let BmcProxyParams { config } = params;

    tracing::info!(
        listen_address = config.listen.to_string(),
        build_version = carbide_version::v!(build_version),
        build_date = carbide_version::v!(build_date),
        rust_version = carbide_version::v!(rust_version),
        "Start carbide BMC proxy",
    );

    let listener = crate::net::bind_with_ipv4_fallback(config.listen)
        .await
        .map_err(BmcProxyError::Listen)?;

    let client_config = ForgeClientConfig::new(
        config.carbide_api.root_ca.clone(),
        Some(ClientCert {
            cert_path: config.carbide_api.client_cert.clone(),
            key_path: config.carbide_api.client_key.clone(),
        }),
    );
    let api_config = ApiConfig::new(config.carbide_api.api_url.as_str(), &client_config);
    let api_client = ForgeApiClient::new(&api_config);

    let state = BmcProxyState {
        config,
        api_client,
        credential_cache: idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL),
        http_client: build_http_client()?,
        ip_cache: bounded_cache(IP_CACHE_TTL),
        response_cache: Arc::new(ResponseCache::new(RESPONSE_CACHE_MAX_BYTES)),
    };

    let app = Router::new()
        .route("/", get(root_url))
        .route("/{*path}", any(proxy_request))
        .with_state(state.clone())
        .layer(from_fn_with_state(state.clone(), authorize_proxy_request))
        .layer(cert_description_layer::<()>(&state.config.auth)?);

    let tls_acceptor = RefreshableTlsAcceptor::new(state.config.tls.clone()).await?;

    let bmc_proxy = BmcProxy {
        app,
        listener,
        state,
        tls_acceptor,
    };

    join_set
        .build_task()
        .name("bmc-proxy listener")
        .spawn(bmc_proxy.run(cancel_token))
        // Safety: will only fail if outside tokio runtime
        .expect("Error spawning bmc-proxy listener");

    Ok(())
}

#[derive(Clone)]
struct RefreshableTlsAcceptor {
    acceptor: TlsAcceptor,
    refreshed_at: Instant,
}

impl RefreshableTlsAcceptor {
    fn is_fresh(&self) -> bool {
        self.refreshed_at.elapsed() < TLS_REFRESH_INTERVAL
    }

    async fn new(config: TlsConfig) -> Result<Self, BmcProxyError> {
        tokio::task::Builder::new()
            .name("get_tls_acceptor refresh")
            .spawn_blocking(move || get_tls_acceptor(&config))
            .expect("Failed to spawn blocking task")
            .await
            .expect("task panicked")
    }
}

/// An inbound connection was accepted from the listener, before it is served.
/// Counted, never logged.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_tls_connection_attempted",
    metric_name = "carbide_bmc_proxy_tls_connection_attempted_total",
    component = "nico-bmc-proxy",
    log = off,
    metric = counter,
    describe = "Number of inbound TLS connection attempts"
)]
struct TlsConnectionAttempted;

/// The TLS handshake completed and the connection was handed to the HTTP
/// stack. Counted, never logged.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_tls_connection_succeeded",
    metric_name = "carbide_bmc_proxy_tls_connection_success_total",
    component = "nico-bmc-proxy",
    log = off,
    metric = counter,
    describe = "Number of successful TLS connections"
)]
struct TlsConnectionSucceeded;

/// Why an inbound connection failed, as the bounded `reason` label. The
/// rendered strings are the metric's contract: each variant renders to the
/// snake_case value the counter has always reported, byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum ConnectionFailReason {
    /// The TCP accept itself errored.
    TcpConnectionFailure,
    /// The TLS acceptor could not be reloaded from disk.
    TlsCertificateInvalid,
    /// The TLS handshake errored.
    TlsConnectionFailure,
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_bmc_proxy_tls_connection_fail_total",
    kind = counter,
    component = "nico-bmc-proxy",
    describe = "Number of failed inbound connections, by failure reason"
)]
struct BmcProxyTlsConnectionFail {
    reason: ConnectionFailReason,
}

/// `TcpAcceptFailed` records a listener error before a peer connection exists.
/// It increments the existing `tcp_connection_failure` series while keeping
/// the per-attempt error in log-only context.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_tcp_accept_failed",
    metric_family = BmcProxyTlsConnectionFail,
    log = error,
    message = "Error accepting connection"
)]
struct TcpAcceptFailed {
    #[label]
    reason: ConnectionFailReason,
    #[context]
    error: String,
}

/// `TlsCertificateReloadFailed` records a failure to rebuild the acceptor from
/// the on-disk TLS configuration. It shares the existing failure counter, but
/// keeps the reload error out of metric labels.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_tls_certificate_reload_failed",
    metric_family = BmcProxyTlsConnectionFail,
    log = error,
    message = "Error reloading TLS certificate, will retry"
)]
struct TlsCertificateReloadFailed {
    #[label]
    reason: ConnectionFailReason,
    #[context]
    error: String,
}

/// `TlsConnectionFailed` records a handshake error after the listener knows
/// the peer. It shares the failure counter with the accept and reload events,
/// while `peer_address` and `error` remain diagnostic context.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_tls_connection_failed",
    metric_family = BmcProxyTlsConnectionFail,
    log = error,
    message = "error accepting tls connection"
)]
struct TlsConnectionFailed {
    #[label]
    reason: ConnectionFailReason,
    #[context]
    error: String,
    #[context]
    peer_address: SocketAddr,
}

struct BmcProxy {
    app: Router,
    listener: TcpListener,
    state: BmcProxyState,
    tls_acceptor: RefreshableTlsAcceptor,
}

impl BmcProxy {
    async fn run(mut self, cancel_token: CancellationToken) {
        let http = auto::Builder::new(TokioExecutor::new());

        while let Some(incoming_connection) = cancel_token
            .run_until_cancelled(self.listener.accept())
            .await
        {
            emit(TlsConnectionAttempted);
            let (conn, addr) = match incoming_connection {
                Ok(incoming) => incoming,
                Err(e) => {
                    emit(TcpAcceptFailed {
                        reason: ConnectionFailReason::TcpConnectionFailure,
                        error: e.to_string(),
                    });
                    continue;
                }
            };

            let tls_acceptor = if self.tls_acceptor.is_fresh() {
                self.tls_acceptor.acceptor.clone()
            } else {
                self.tls_acceptor =
                    match RefreshableTlsAcceptor::new(self.state.config.tls.clone()).await {
                        Ok(acceptor) => acceptor,
                        Err(e) => {
                            emit(TlsCertificateReloadFailed {
                                reason: ConnectionFailReason::TlsCertificateInvalid,
                                error: e.to_string(),
                            });
                            continue;
                        }
                    };
                self.tls_acceptor.acceptor.clone()
            };

            // Spawn task to handle request
            let http = http.clone();
            let app = self.app.clone();

            tokio::task::Builder::new()
                .name("http conn handler")
                .spawn(async move {
                    match tls_acceptor.accept(conn).await {
                        Ok(conn) => {
                            let conn = TokioIo::new(conn);
                            emit(TlsConnectionSucceeded);

                            let (_, session) = conn.inner().get_ref();
                            let connection_attributes = {
                                let peer_address = addr;
                                let peer_certificates =
                                    session.peer_certificates().unwrap_or_default().to_vec();
                                Arc::new(ConnectionAttributes {
                                    peer_address,
                                    peer_certificates,
                                })
                            };
                            let conn_attrs_extension_layer =
                                AddExtensionLayer::new(connection_attributes);

                            let app_with_ext = tower::ServiceBuilder::new()
                                .layer(conn_attrs_extension_layer)
                                .service(app);

                            if let Err(error) = http
                                .serve_connection(conn, TowerToHyperService::new(app_with_ext))
                                .await
                            {
                                tracing::debug!(
                                    %error,
                                    error_debug = ?error,
                                    "error servicing tls http request",
                                );
                            }
                        }
                        Err(error) => {
                            emit(TlsConnectionFailed {
                                reason: ConnectionFailReason::TlsConnectionFailure,
                                error: error.to_string(),
                                peer_address: addr,
                            });
                        }
                    }
                })
                // Safety: This only fails if run outside the tokio runtime
                .expect("could not spawn task to handle HTTP connection");
        }

        tracing::info!("nico-bmc-proxy shutting down");
    }
}

fn get_tls_acceptor(tls_config: &TlsConfig) -> Result<RefreshableTlsAcceptor, BmcProxyError> {
    let certs = {
        let fd = match std::fs::File::open(&tls_config.identity_pemfile_path) {
            Ok(fd) => fd,
            Err(e) => {
                return Err(BmcProxyError::TlsConfig(format!(
                    "Could not open identity PEM at {}: {}",
                    tls_config.identity_pemfile_path, e
                )));
            }
        };
        let mut buf = std::io::BufReader::new(&fd);
        rustls_pemfile::certs(&mut buf).collect::<Result<Vec<_>, _>>()
    }
    .map_err(|e| {
        BmcProxyError::TlsConfig(format!(
            "Error loading identity PEM at {}: {}",
            tls_config.identity_pemfile_path, e
        ))
    })?;

    let key = std::fs::File::open(&tls_config.identity_keyfile_path)
        .map_err(|e| {
            BmcProxyError::TlsConfig(format!(
                "Could not open key file at {}: {}",
                tls_config.identity_keyfile_path, e
            ))
        })
        .and_then(|fd| {
            let mut buf = std::io::BufReader::new(&fd);
            rustls_pemfile::ec_private_keys(&mut buf)
                .next()
                .ok_or_else(|| {
                    BmcProxyError::TlsConfig(format!(
                        "No keys found in key file at {}",
                        tls_config.identity_keyfile_path
                    ))
                })
        })?
        .map_err(|e| {
            BmcProxyError::TlsConfig(format!(
                "Error parsing key file at {}: {}",
                tls_config.identity_keyfile_path, e
            ))
        })?;

    let crypto_provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

    let roots = {
        let mut roots = RootCertStore::empty();
        let pem_file = std::fs::read(&tls_config.root_cafile_path).map_err(|e| {
            BmcProxyError::TlsConfig(format!(
                "error reading root ca cert file at {}: {}",
                tls_config.root_cafile_path, e
            ))
        })?;
        let mut cert_cursor = std::io::Cursor::new(&pem_file[..]);
        let certs_to_add = rustls_pemfile::certs(&mut cert_cursor)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                BmcProxyError::TlsConfig(format!(
                    "error parsing root ca cert file at {}: {}",
                    tls_config.root_cafile_path, e
                ))
            })?;
        let (_added, _ignored) = roots.add_parsable_certificates(certs_to_add);

        if let Ok(pem_file) = std::fs::read(&tls_config.admin_root_cafile_path) {
            let mut cert_cursor = std::io::Cursor::new(&pem_file[..]);
            let certs_to_add = rustls_pemfile::certs(&mut cert_cursor)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| {
                    BmcProxyError::TlsConfig(format!(
                        "error parsing admin ca cert file at {}: {}",
                        tls_config.admin_root_cafile_path, error
                    ))
                })?;
            let (_added, _ignored) = roots.add_parsable_certificates(certs_to_add);
        }
        Arc::new(roots)
    };

    let client_cert_verifier =
        WebPkiClientVerifier::builder_with_provider(roots, crypto_provider.clone())
            .allow_unauthenticated()
            .allow_unknown_revocation_status()
            .build()
            .map_err(|e| {
                BmcProxyError::TlsConfig(format!(
                    "Could not build client cert verifier. Does root CA file at {} contain no root trust anchors? {}",
                    tls_config.root_cafile_path,
                    e
                ))
            })?;

    let mut tls = ServerConfig::builder_with_provider(crypto_provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(client_cert_verifier)
        .with_single_cert(certs, rustls_pki_types::PrivateKeyDer::Sec1(key))
        .map_err(|e| {
            BmcProxyError::TlsConfig(format!("Rustls error building server config: {e}",))
        })?;

    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    let acceptor = TlsAcceptor::from(Arc::new(tls));
    Ok(RefreshableTlsAcceptor {
        acceptor,
        refreshed_at: Instant::now(),
    })
}

fn cert_description_layer<AZ: Authorization>(
    auth_config: &AuthConfig,
) -> Result<CertDescriptionMiddleware<AZ>, BmcProxyError> {
    tracing::info!(trust_config = ?auth_config.trust, "TrustConfig rendered from config");
    let spiffe_context = SpiffeContext::try_from(auth_config.trust.clone()).map_err(|e| {
        BmcProxyError::InvalidConfiguration(format!(
            "Invalid trust config in bmc-proxy config toml file: {e}"
        ))
    })?;

    Ok(CertDescriptionMiddleware::new(
        auth_config.cli_certs.clone(),
        spiffe_context,
    ))
}

async fn root_url() -> &'static str {
    const ROOT_CONTENTS: &str = if carbide_version::literal!(build_version).is_empty() {
        "Carbide BMC proxy development build\n"
    } else {
        concat!(
            "Carbide BMC proxy ",
            carbide_version::literal!(build_version),
            "\n"
        )
    };
    ROOT_CONTENTS
}

async fn proxy_request(
    State(state): State<BmcProxyState>,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let request_span = bmc_proxy_request_span(&request);

    let result = proxy_request_inner(state, request)
        .instrument(request_span.clone())
        .await;
    let status = match &result {
        Ok(response) | Err(response) => response.status(),
    };
    request_span.record("http.response.status_code", status.as_u16());
    request_span.record("otel.status_code", span_status(status));
    result
}

fn bmc_proxy_request_span<B>(request: &Request<B>) -> tracing::Span {
    let request_span = tracing::info_span!(
        parent: None,
        "bmc_proxy_request",
        http.request.method = %request.method(),
        url.path = %request.uri().path(),
        http.response.status_code = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
        bmc.ip_address = tracing::field::Empty,
        bmc_proxy.class = tracing::field::Empty,
        logfmt.suppress = true,
    );
    set_span_parent_from_headers(&request_span, request.headers());
    request_span
}

/// The OpenTelemetry status for a proxied request that answered with `status`.
///
/// Only a 5xx marks the span failed: a rejected or malformed request is the caller's error, and
/// counting it against the proxy would bury the hops that actually broke.
fn span_status(status: StatusCode) -> &'static str {
    if status.is_server_error() {
        "error"
    } else {
        "ok"
    }
}

async fn proxy_request_inner(
    state: BmcProxyState,
    request: Request<Body>,
) -> Result<Response<Body>, Response<Body>> {
    let Some(principals) = state.authorized_principals(&request) else {
        return Ok(error_response((StatusCode::FORBIDDEN, "Forbidden").into()));
    };
    let (parts, body) = request.into_parts();
    let forwarded_target = forwarded_header_value(&parts.headers)
        .map_err(|e| error_response((StatusCode::BAD_REQUEST, e.to_string()).into()))?
        .ok_or_else(|| {
            error_response(
                (
                    StatusCode::BAD_REQUEST,
                    "missing Forwarded host/mac/serial in request header",
                )
                    .into(),
            )
        })?;

    let target_ip = match ip_for_forwarded_target(&forwarded_target, &state).await {
        Ok(Some(ip)) => ip,
        Ok(None) => {
            return Err(error_response(
                (
                    StatusCode::BAD_REQUEST,
                    "Could not find BMC from forwarded header",
                )
                    .into(),
            ));
        }
        Err(e) => {
            return Err(error_response(
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Failure looking up BMC IP from target: {e}"),
                )
                    .into(),
            ));
        }
    };

    tracing::Span::current().record("bmc.ip_address", target_ip.to_string());

    let class = state
        .config
        .classes
        .classify(&parts.method, parts.uri.path(), &principals);
    tracing::Span::current().record("bmc_proxy.class", class.name.as_str());

    let path_and_query = parts
        .uri
        .clone()
        .into_parts()
        .path_and_query
        .ok_or_else(|| error_response((StatusCode::BAD_REQUEST, "missing path").into()))?;

    if parts.method == Method::GET
        && let Some(policy) = &class.cache
    {
        return serve_cached_get(state, target_ip, &parts, &class, policy, path_and_query).await;
    }

    // Buffer a replayable body once so a stale-credential rejection can be
    // retried; streamed (large) bodies are sent as-is and never replayed.
    let mut upstream_body = if method_supports_body(&parts.method) {
        UpstreamBody::prepare(&parts.headers, body)
            .await
            .map_err(|e| error_response((StatusCode::BAD_REQUEST, e.to_string()).into()))?
    } else {
        UpstreamBody::None
    };

    let forwarded = forward_with_auth_retry(
        &state,
        target_ip,
        &parts.method,
        &parts.headers,
        path_and_query,
        &mut upstream_body,
        class.upstream_timeout,
    )
    .await;

    // A write may have changed what the cache holds for the BMC unless the
    // BMC definitely rejected it or the request never reached it. A response
    // lost after sending proves nothing either way, so it invalidates too.
    let may_have_changed_bmc = match &forwarded {
        Ok(response) => !response.status().is_client_error(),
        Err(failure) => failure.reached_bmc,
    };
    if is_write(&parts.method) && may_have_changed_bmc {
        state.response_cache.invalidate_for_write(
            target_ip,
            &parts.method,
            parts.uri.path(),
            &state.config.classes,
            tokio::time::Instant::now(),
        );
    }

    Ok(respond_from_upstream(forwarded))
}

/// The caller's response for one forward: the upstream response with its
/// body streamed back, or the error the forward produced.
fn respond_from_upstream(forwarded: Result<reqwest::Response, ForwardError>) -> Response<Body> {
    match forwarded {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            build_response(status, &headers, Body::from_stream(response.bytes_stream()))
        }
        Err(failure) => error_response(failure.error),
    }
}

/// Forwards once, and once more with freshly resolved credentials when the
/// BMC rejected the cached ones and the body can be replayed, so callers
/// never see a stale-session 401. A second rejection is returned as-is.
async fn forward_with_auth_retry(
    state: &BmcProxyState,
    target_ip: IpAddr,
    method: &Method,
    headers: &HeaderMap,
    path_and_query: PathAndQuery,
    upstream_body: &mut UpstreamBody,
    timeout: Duration,
) -> Result<reqwest::Response, ForwardError> {
    let mut upstream_response = send_upstream(
        state,
        target_ip,
        method,
        headers,
        path_and_query.clone(),
        upstream_body,
        timeout,
    )
    .await?;

    let rejected = |status: reqwest::StatusCode| {
        status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN
    };
    if rejected(upstream_response.status()) && upstream_body.is_replayable() {
        evict_cached_credentials(target_ip, &state.credential_cache).await;
        emit(UpstreamAuthRetried {
            method: MethodLabel::from(method),
            bmc_ip_address: target_ip.to_string(),
        });
        upstream_response = send_upstream(
            state,
            target_ip,
            method,
            headers,
            path_and_query,
            upstream_body,
            timeout,
        )
        .await?;
    }

    if rejected(upstream_response.status()) {
        evict_cached_credentials(target_ip, &state.credential_cache).await;
    }

    Ok(upstream_response)
}

/// Answers a `GET` in a cached class, going to the BMC only when nothing
/// stored can be served.
///
/// While a recent write holds the class for the BMC, the request is
/// forwarded without the store. Otherwise a fresh entry is served as-is and
/// a stale one is served while one refresh runs behind it. Failing those,
/// this request fetches, or joins the fetch already in flight for the same
/// resource; if that fetch fails and an entry within its `stale_if_error`
/// window exists, the entry stands in. After a fetch that yields nothing
/// storable the key is held off for a short while, during which an entry
/// that can stand in is served and a request without one is forwarded
/// directly. `Cache-Control: no-cache`, `no-store`, or `max-age=0`, or
/// `Pragma: no-cache`, skips the stored entry but the fetched response is
/// still stored, because the cache is shared and protects the BMC rather
/// than the caller. Every response leaves through here with the outcome
/// named on it.
async fn serve_cached_get(
    state: BmcProxyState,
    target_ip: IpAddr,
    parts: &http::request::Parts,
    class: &Arc<RequestClass>,
    policy: &CachePolicy,
    path_and_query: PathAndQuery,
) -> Result<Response<Body>, Response<Body>> {
    let now = tokio::time::Instant::now();
    if state.response_cache.is_held(target_ip, &class.name, now) {
        // A recent write may still be taking effect on the BMC, so nothing
        // read now may be stored or served later.
        return forward_uncached(
            &state,
            target_ip,
            parts,
            class,
            path_and_query,
            CacheOutcome::Held,
        )
        .await;
    }

    if !is_cacheable_query(&path_and_query) {
        // A query the cache does not key names the same resource to the BMC
        // but would be a distinct entry here, so it is not stored.
        return forward_uncached(
            &state,
            target_ip,
            parts,
            class,
            path_and_query,
            CacheOutcome::Uncacheable,
        )
        .await;
    }

    let key = CacheKey::new(target_ip, class.name.clone(), &path_and_query);
    let bypass = requests_fresh_response(&parts.headers);
    let existing = state.response_cache.lookup(&key).await;
    // Built once, run only by the request that starts a fetch; a joiner drops
    // it unused.
    let make_fetch = || {
        upstream_fetch(
            state.clone(),
            target_ip,
            &parts.headers,
            path_and_query.clone(),
            class.upstream_timeout,
        )
    };

    if !bypass && let Some(entry) = &existing {
        match entry.freshness(policy, now) {
            Freshness::Fresh => {
                return Ok(answer_from_cache(
                    class,
                    CacheOutcome::Hit,
                    entry,
                    &parts.headers,
                    now,
                ));
            }
            Freshness::Stale => {
                if state
                    .response_cache
                    .hold_off_reason(&key, now)
                    .await
                    .is_none()
                {
                    state
                        .response_cache
                        .spawn_refresh(key, class, Arc::clone(entry), make_fetch);
                }
                return Ok(answer_from_cache(
                    class,
                    CacheOutcome::Stale,
                    entry,
                    &parts.headers,
                    now,
                ));
            }
            Freshness::Expired => {}
        }
    }

    // The stored entry that may stand in for a fetch the BMC fails, unless
    // the caller asked for a live answer.
    let fallback = if bypass {
        None
    } else {
        existing
            .as_ref()
            .filter(|entry| entry.usable_on_error(policy, now))
    };

    // A fetch for this resource yielded nothing storable moments ago: after
    // an error the fallback is served without asking the BMC again, and
    // otherwise the request goes straight to the BMC instead of through
    // another doomed fetch, so callers see what the BMC answers.
    if let Some(reason) = state.response_cache.hold_off_reason(&key, now).await {
        if reason == HoldOffReason::Error
            && let Some(entry) = fallback
        {
            return Ok(answer_from_cache(
                class,
                CacheOutcome::StaleIfError,
                entry,
                &parts.headers,
                now,
            ));
        }
        let outcome = if bypass {
            CacheOutcome::Bypass
        } else {
            CacheOutcome::Miss
        };
        return forward_uncached(&state, target_ip, parts, class, path_and_query, outcome).await;
    }

    let fetch = state
        .response_cache
        .fetch(key, class, existing.clone(), make_fetch);
    let (outcome, joined) = match fallback {
        // A BMC slow to answer, or slow to fail, must not make a caller wait
        // out the whole budget when a usable response is at hand; the fetch
        // goes on without this caller.
        Some(entry) => match tokio::time::timeout(SLOW_FETCH_FALLBACK_WAIT, fetch).await {
            Ok(result) => result,
            Err(_elapsed) => {
                return Ok(answer_from_cache(
                    class,
                    CacheOutcome::StaleIfError,
                    entry,
                    &parts.headers,
                    tokio::time::Instant::now(),
                ));
            }
        },
        None => fetch.await,
    };
    let fetched = if bypass {
        CacheOutcome::Bypass
    } else if joined {
        CacheOutcome::Coalesced
    } else {
        CacheOutcome::Miss
    };
    let now = tokio::time::Instant::now();
    let stale_fallback = || {
        existing
            .as_ref()
            .filter(|entry| entry.usable_on_error(policy, now))
    };

    match outcome {
        FetchOutcome::Fetched(entry) | FetchOutcome::Revalidated(entry) => Ok(answer_from_cache(
            class,
            fetched,
            &entry,
            &parts.headers,
            now,
        )),
        FetchOutcome::Passthrough {
            status,
            headers,
            body,
        } => {
            if status.is_server_error()
                && let Some(entry) = stale_fallback()
            {
                return Ok(answer_from_cache(
                    class,
                    CacheOutcome::StaleIfError,
                    entry,
                    &parts.headers,
                    now,
                ));
            }
            emit(CacheLookupCompleted {
                class: class.name.clone(),
                outcome: fetched,
            });
            let mut response = response_with_headers(status, &headers, Body::from(body));
            set_cache_status(&mut response, fetched);
            Ok(response)
        }
        FetchOutcome::TooLarge => {
            // The resource does not fit the store, so this caller forwards
            // for itself and streams the body, as an uncached class would.
            forward_uncached(&state, target_ip, parts, class, path_and_query, fetched).await
        }
        FetchOutcome::Failed { status, message } => {
            if let Some(entry) = stale_fallback() {
                return Ok(answer_from_cache(
                    class,
                    CacheOutcome::StaleIfError,
                    entry,
                    &parts.headers,
                    now,
                ));
            }
            emit(CacheLookupCompleted {
                class: class.name.clone(),
                outcome: fetched,
            });
            let mut response = error_response((status, message).into());
            set_cache_status(&mut response, fetched);
            Err(response)
        }
    }
}

/// Forwards a `GET` in a cached class for one caller without the store,
/// streaming the body back, and names `outcome` on the response whether the
/// forward succeeded or not.
async fn forward_uncached(
    state: &BmcProxyState,
    target_ip: IpAddr,
    parts: &http::request::Parts,
    class: &RequestClass,
    path_and_query: PathAndQuery,
    outcome: CacheOutcome,
) -> Result<Response<Body>, Response<Body>> {
    emit(CacheLookupCompleted {
        class: class.name.clone(),
        outcome,
    });
    let mut body = UpstreamBody::None;
    let forwarded = forward_with_auth_retry(
        state,
        target_ip,
        &parts.method,
        &parts.headers,
        path_and_query,
        &mut body,
        class.upstream_timeout,
    )
    .await;
    let mut response = respond_from_upstream(forwarded);
    set_cache_status(&mut response, outcome);
    Ok(response)
}

/// The fetch the response cache runs for one resource: a `GET` carrying the
/// initiating caller's headers, made conditional on the stored `ETag` when
/// refreshing, with the body read in full so it can be stored and shared.
fn upstream_fetch(
    state: BmcProxyState,
    target_ip: IpAddr,
    request_headers: &HeaderMap,
    path_and_query: PathAndQuery,
    timeout: Duration,
) -> impl FnOnce(FetchRequest) -> BoxFuture<'static, UpstreamReply> + Send + 'static {
    let headers = fetch_request_headers(request_headers);
    move |request: FetchRequest| {
        Box::pin(async move {
            let mut headers = headers;
            if let Some(etag) = request.if_none_match {
                headers.insert(header::IF_NONE_MATCH, etag);
            }
            let mut body = UpstreamBody::None;
            let response = match forward_with_auth_retry(
                &state,
                target_ip,
                &Method::GET,
                &headers,
                path_and_query,
                &mut body,
                timeout,
            )
            .await
            {
                Ok(response) => response,
                Err(failure) => {
                    return UpstreamReply::Failed {
                        status: failure.error.status,
                        message: failure.error.message,
                    };
                }
            };
            let status = response.status();
            let response_headers = forwardable_response_headers(response.headers());
            match read_bounded_body(response, MAX_BUFFERED_BODY_SIZE).await {
                BodyRead::Complete(body) => UpstreamReply::Response {
                    status,
                    headers: response_headers,
                    body,
                },
                BodyRead::TooLarge => UpstreamReply::TooLarge,
                BodyRead::Failed(message) => UpstreamReply::Failed {
                    status: StatusCode::BAD_GATEWAY,
                    message,
                },
            }
        })
    }
}

/// The caller's headers as the cache's own fetch sends them: without the
/// conditional and cache directives the cache answers itself, and without
/// content negotiation, which one canonical representation replaces so that
/// every caller can consume what one fetch stored.
fn fetch_request_headers(request_headers: &HeaderMap) -> HeaderMap {
    let mut headers = request_headers.clone();
    for name in [
        header::IF_NONE_MATCH,
        header::IF_MODIFIED_SINCE,
        header::IF_MATCH,
        header::IF_UNMODIFIED_SINCE,
        header::IF_RANGE,
        header::RANGE,
        header::CACHE_CONTROL,
        header::PRAGMA,
        header::ACCEPT,
        header::ACCEPT_ENCODING,
        header::ACCEPT_LANGUAGE,
        header::ACCEPT_CHARSET,
        header::COOKIE,
    ] {
        headers.remove(name);
    }
    headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    headers
}

/// Whether the caller asked to skip the stored response, with
/// `Cache-Control: no-cache`, `no-store`, or `max-age=0`, or the HTTP/1.0
/// `Pragma: no-cache`.
fn requests_fresh_response(headers: &HeaderMap) -> bool {
    let directive_present = |name: HeaderName, directives: &[&str]| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(|directive| directive.trim().to_ascii_lowercase())
            .any(|directive| directives.contains(&directive.as_str()))
    };
    directive_present(
        header::CACHE_CONTROL,
        &["no-cache", "no-store", "max-age=0"],
    ) || directive_present(header::PRAGMA, &["no-cache"])
}

/// Names how the cache answered on a response leaving the cache path.
fn set_cache_status(response: &mut Response<Body>, outcome: CacheOutcome) {
    response.headers_mut().insert(
        CACHE_STATUS_HEADER,
        HeaderValue::from_str(outcome.label_value().as_str())
            .expect("cache outcome labels are lowercase ASCII"),
    );
}

/// Builds the caller's response from a stored entry: `304` when the caller's
/// `If-None-Match` names the stored `ETag`, else the stored `200`. Both carry
/// the entry's age and how the cache answered.
fn answer_from_cache(
    class: &RequestClass,
    outcome: CacheOutcome,
    entry: &CachedResponse,
    request_headers: &HeaderMap,
    now: tokio::time::Instant,
) -> Response<Body> {
    emit(CacheLookupCompleted {
        class: class.name.clone(),
        outcome,
    });

    let mut response = if if_none_match_names(request_headers, entry.etag()) {
        let mut response = Response::builder().status(StatusCode::NOT_MODIFIED);
        if let Some(etag) = entry.etag() {
            response = response.header(header::ETAG, etag);
        }
        response.body(Body::empty())
    } else {
        let mut response = Response::builder().status(StatusCode::OK);
        for (name, value) in &entry.headers {
            response = response.header(name, value);
        }
        response.body(Body::from(entry.body.clone()))
    }
    // The headers were received from a BMC as valid header values, and the
    // status is a constant, so the builder cannot fail.
    .expect("a stored response rebuilds from its validated headers");

    response
        .headers_mut()
        .insert(header::AGE, HeaderValue::from(entry.age(now).as_secs()));
    set_cache_status(&mut response, outcome);
    response
}

/// Whether the caller's `If-None-Match` names `etag`, comparing weakly as
/// RFC 9110 requires for `GET`: `W/"x"` and `"x"` match, and `*` matches any.
fn if_none_match_names(request_headers: &HeaderMap, etag: Option<&HeaderValue>) -> bool {
    let Some(etag) = etag.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let weak = |tag: &str| tag.strip_prefix("W/").unwrap_or(tag).to_string();
    request_headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|candidate| candidate == "*" || weak(candidate) == weak(etag))
}

/// How reading an upstream body for the store ended.
enum BodyRead {
    Complete(Bytes),
    /// The body exceeds the store's limit; it was not read in full.
    TooLarge,
    Failed(String),
}

/// Reads an upstream body in full, stopping at `limit` bytes so a
/// misbehaving BMC cannot grow the cache without bound.
async fn read_bounded_body(response: reqwest::Response, limit: usize) -> BodyRead {
    if let Some(length) = response.content_length()
        && length > limit as u64
    {
        return BodyRead::TooLarge;
    }
    match axum::body::to_bytes(Body::from_stream(response.bytes_stream()), limit).await {
        Ok(body) => BodyRead::Complete(body),
        Err(error) => {
            let error = error.into_inner();
            if error.downcast_ref::<LengthLimitError>().is_some() {
                BodyRead::TooLarge
            } else {
                BodyRead::Failed(format!("error reading upstream response: {error}"))
            }
        }
    }
}

/// Whether `method` writes to the BMC, as far as the cache is concerned.
/// Other methods, such as `OPTIONS`, cannot change a resource.
fn is_write(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// The caller's request body, in a form the proxy can attach to an upstream
/// request -- and, when buffered, attach again for one retry.
enum UpstreamBody {
    None,
    /// Small or unsized bodies, buffered up to [`MAX_BUFFERED_BODY_SIZE`].
    Buffered(bytes::Bytes),
    /// A large body with a declared length is streamed straight through and
    /// can be sent only once; `None` once consumed.
    Streamed {
        body: Option<Body>,
        declared_length: u64,
    },
}

impl UpstreamBody {
    async fn prepare(headers: &HeaderMap, body: Body) -> Result<Self, axum::Error> {
        let declared_length = headers
            .get(axum::http::header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        match declared_length {
            Some(length) if length > MAX_BUFFERED_BODY_SIZE as u64 => Ok(Self::Streamed {
                body: Some(body),
                declared_length: length,
            }),
            _ => Ok(Self::Buffered(
                axum::body::to_bytes(body, MAX_BUFFERED_BODY_SIZE).await?,
            )),
        }
    }

    fn is_replayable(&self) -> bool {
        matches!(self, Self::None | Self::Buffered(_))
    }

    /// Attaches the body to `request`. A streamed body is handed over on the
    /// first call; a later call finds it consumed and fails.
    fn attach(
        &mut self,
        request: reqwest_middleware::RequestBuilder,
    ) -> Result<reqwest_middleware::RequestBuilder, ProxyError> {
        match self {
            Self::None => Ok(request),
            Self::Buffered(bytes) => Ok(request.body(bytes.clone())),
            Self::Streamed {
                body,
                declared_length,
            } => {
                // A streamed body cannot be replayed, so reqwest's redirect
                // layer forwards a BMC 307/308 to the caller as-is instead of
                // following it the way buffered requests do. Callers pushing
                // firmware should use the canonical UpdateService URI rather
                // than rely on redirects.
                let body = body.take().ok_or_else(|| {
                    ProxyError::from((
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "streamed request body already consumed".to_string(),
                    ))
                })?;
                Ok(request
                    .header(axum::http::header::CONTENT_LENGTH, *declared_length)
                    .timeout(sized_upload_timeout(*declared_length))
                    .body(reqwest::Body::wrap_stream(body.into_data_stream())))
            }
        }
    }
}

/// One forwarding attempt: resolve credentials for `target_ip` (cached or
/// freshly minted), build the upstream request from the caller's `method`
/// and `headers`, attach the body, and send within `timeout`. Records the
/// per-attempt upstream metric.
async fn send_upstream(
    state: &BmcProxyState,
    target_ip: IpAddr,
    method: &Method,
    headers: &HeaderMap,
    path_and_query: PathAndQuery,
    upstream_body: &mut UpstreamBody,
    timeout: Duration,
) -> Result<reqwest::Response, ForwardError> {
    let mut bmc_client_info = create_client(
        target_ip,
        &state.api_client,
        &state.credential_cache,
        state.http_client.clone(),
        &state.config.bmc_proxy,
    )
    .await
    .map_err(|e| ForwardError::before_send((StatusCode::BAD_GATEWAY, e.to_string())))?;

    copy_request_headers(headers, &mut bmc_client_info.header_map);

    let mut upstream_uri_parts = bmc_client_info.base_upstream_uri.into_parts();
    upstream_uri_parts.path_and_query = Some(path_and_query);
    let upstream_uri = Uri::from_parts(upstream_uri_parts)
        .map_err(|e| ForwardError::before_send((StatusCode::BAD_REQUEST, e.to_string())))?;

    let upstream_request = bmc_client_info
        .http_client
        .request(method.clone(), upstream_uri.to_string())
        .headers(bmc_client_info.header_map)
        // The class budget. A streamed upload replaces it with one scaled to
        // its declared size when the body attaches.
        .timeout(timeout);
    let upstream_request = bmc_client_info
        .credentials
        .apply_to_request(upstream_request)
        .map_err(|e| {
            ForwardError::before_send((
                StatusCode::BAD_GATEWAY,
                format!("invalid credentials: {e}"),
            ))
        })?;
    let upstream_request = upstream_body
        .attach(upstream_request)
        .map_err(ForwardError::before_send)?;

    let started = Instant::now();
    let upstream_result = upstream_request.send().await;
    emit(UpstreamRequestCompleted {
        method: MethodLabel::from(method),
        status: UpstreamStatus::from_result(&upstream_result),
        took: started.elapsed(),
    });
    upstream_result.map_err(|e| ForwardError {
        reached_bmc: request_may_have_reached_bmc(&e),
        error: ProxyError::from((StatusCode::BAD_GATEWAY, e.to_string())),
    })
}

/// A forward that produced no response, and whether the request may have
/// reached the BMC before it failed. A write that never left the proxy
/// cannot have changed anything the cache holds.
struct ForwardError {
    error: ProxyError,
    reached_bmc: bool,
}

impl ForwardError {
    fn before_send(error: impl Into<ProxyError>) -> Self {
        Self {
            error: error.into(),
            reached_bmc: false,
        }
    }
}

/// Whether a failed send may have delivered the request: a connection that
/// never opened or a request that never built did not, anything later may.
fn request_may_have_reached_bmc(error: &reqwest_middleware::Error) -> bool {
    match error {
        reqwest_middleware::Error::Reqwest(error) => !error.is_connect() && !error.is_builder(),
        reqwest_middleware::Error::Middleware(_) => true,
    }
}

async fn ip_for_forwarded_target(
    forwarded_target: &ForwardedTarget<'_>,
    state: &BmcProxyState,
) -> Result<Option<IpAddr>, tonic::Status> {
    let lookup_by = match forwarded_target {
        ForwardedTarget::Ip(ip) => {
            // No need to look up
            return Ok(Some(*ip));
        }
        ForwardedTarget::Mac(mac) => LookupBy::MacAddress(mac.to_string()),
        ForwardedTarget::Serial(serial) => LookupBy::Serial(serial.to_string()),
    };

    if let Some(ip) = state.ip_cache.get(&lookup_by).await {
        return Ok(Some(ip));
    }

    let lookup_by_str = match &lookup_by {
        LookupBy::Serial(serial) => format!("Serial number {serial}"),
        LookupBy::MacAddress(mac) => format!("MAC address {mac}"),
    };

    let ips = state
        .api_client
        .find_bmc_ips(forge::FindBmcIpsRequest {
            lookup_by: Some(lookup_by.clone()),
        })
        .await?
        .bmc_ips
        .iter()
        .filter_map(|s| {
            IpAddr::from_str(s)
                .inspect_err(|e| tracing::error!(error = %e, "Invalid IP address returned by API"))
                .ok()
        })
        .collect::<Vec<_>>();

    if ips.is_empty() {
        return Ok(None);
    }

    let (v4_ips, v6_ips): (Vec<IpAddr>, Vec<IpAddr>) = ips.into_iter().partition(|ip| ip.is_ipv4());

    let ip = match (v4_ips.len(), v6_ips.len()) {
        (0, 1..) => {
            if v6_ips.len() > 1 {
                tracing::warn!(
                    lookup_by = %lookup_by_str,
                    ip_addresses = ?v6_ips,
                    "Multiple IPv6 BMC IP's found, using first one",
                );
            }
            v6_ips.into_iter().next()
        }
        _ => {
            // TODO: We may want to be smart about when to pick IPv6 vs IPv4, but for now just pick IPv4
            // first, in case of broken dual-stack setups.
            if v4_ips.len() > 1 {
                tracing::warn!(
                    lookup_by = %lookup_by_str,
                    ip_addresses = ?v4_ips,
                    "Multiple IPv4 BMC IP's found, using first one",
                );
            }
            v4_ips.into_iter().next()
        }
    };

    if let Some(ip) = ip {
        state.ip_cache.insert(lookup_by, ip).await;
    }
    Ok(ip)
}

async fn authorize_proxy_request(
    State(state): State<BmcProxyState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response<Body>, StatusCode> {
    authorize_principal_allow_list(&state, &request)?;
    Ok(next.run(request).await)
}

fn authorize_principal_allow_list(
    state: &BmcProxyState,
    request: &Request<Body>,
) -> Result<(), StatusCode> {
    let auth_context = request
        .extensions()
        .get::<AuthContext<()>>()
        .ok_or_else(|| {
            emit(AuthContextMissing::PrincipalAllowList {
                method_label: request.method().into(),
            });
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let present_principals = request_principal_ids(auth_context);

    let allowed = present_principals
        .iter()
        .any(|principal| state.config.allowed_principals.contains(principal));

    if allowed {
        Ok(())
    } else {
        emit(PrincipalAllowListDenied::new(
            request.method(),
            format!("{:?}", state.config.allowed_principals),
            format!("{present_principals:?}"),
            request.uri().path().to_string(),
        ));
        Err(StatusCode::FORBIDDEN)
    }
}

fn request_principal_ids(auth_context: &AuthContext<()>) -> Vec<String> {
    let mut principals = auth_context
        .principals
        .iter()
        .map(Principal::as_identifier)
        .collect::<Vec<_>>();
    principals.push(Principal::Anonymous.as_identifier());
    principals
}

fn build_response(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: Body,
) -> Response<Body> {
    let mut response = Response::builder().status(status);
    for (name, value) in forwardable_response_header_pairs(headers) {
        response = response.header(name, value);
    }
    response.body(body).unwrap()
}

/// A response carrying `headers` verbatim; for headers already filtered by
/// [`forwardable_response_headers`].
fn response_with_headers(status: StatusCode, headers: &HeaderMap, body: Body) -> Response<Body> {
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        response = response.header(name, value);
    }
    response.body(body).unwrap()
}

/// The upstream response headers a caller receives: everything but
/// hop-by-hop headers and `Content-Length`, which the proxy's own framing of
/// the body replaces.
fn forwardable_response_header_pairs(
    headers: &HeaderMap,
) -> impl Iterator<Item = (&HeaderName, &HeaderValue)> {
    headers.iter().filter(|(name, _)| {
        !is_hop_by_hop_header(name.as_str()) && **name != header::CONTENT_LENGTH
    })
}

/// [`forwardable_response_header_pairs`] as an owned map, for storing.
fn forwardable_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut forwardable = HeaderMap::new();
    for (name, value) in forwardable_response_header_pairs(headers) {
        forwardable.append(name.clone(), value.clone());
    }
    forwardable
}

fn copy_request_headers(source: &HeaderMap, dest: &mut HeaderMap) {
    for (name, value) in source {
        if is_hop_by_hop_header(name.as_str())
            // Trace context describes the caller's hop; the upstream client's tracing middleware
            // re-injects the proxy's own hop on egress.
            || is_propagated_header(name.as_str())
            || *name == axum::http::header::HOST
            || *name == axum::http::header::AUTHORIZATION
            || name.as_str().eq_ignore_ascii_case(REDFISH_AUTH_TOKEN_HEADER)
            || name.as_str().eq_ignore_ascii_case("forwarded")
            || *name == axum::http::header::CONTENT_LENGTH
        {
            continue;
        }
        dest.append(name.clone(), value.clone());
    }
}

fn method_supports_body(method: &Method) -> bool {
    // Redfish services can accept DELETE payloads, so only the methods this
    // proxy treats as bodyless are excluded.
    !matches!(*method, Method::GET | Method::HEAD)
}

/// Time allowed for a streamed upload of `length` bytes: the ordinary
/// request budget plus the transfer itself at worst-case OOB bandwidth,
/// bounded by [`MAX_UPLOAD_TIMEOUT`] because `length` is caller-supplied.
fn sized_upload_timeout(length: u64) -> Duration {
    (DEFAULT_UPSTREAM_TIMEOUT + Duration::from_secs(length / MIN_UPLOAD_BANDWIDTH_BYTES_PER_SEC))
        .min(MAX_UPLOAD_TIMEOUT)
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

fn forwarded_header_value(
    headers: &HeaderMap,
) -> Result<Option<ForwardedTarget<'_>>, ForwardedHeaderParseError> {
    let values = headers.get_all("forwarded");
    for raw_value in values {
        let Ok(raw_value) = raw_value.to_str() else {
            continue;
        };
        for element in raw_value.split(',') {
            for pair in element.split(';') {
                let Some((key, value)) = pair.trim().split_once('=') else {
                    continue;
                };
                let key = key.trim();
                if key.eq_ignore_ascii_case("host") {
                    return Ok(Some(ForwardedTarget::Ip(parse_forwarded_host_value(
                        value.trim(),
                    )?)));
                } else if key.eq_ignore_ascii_case("mac") {
                    return Ok(Some(ForwardedTarget::Mac(MacAddress::from_str(
                        value.trim(),
                    )?)));
                } else if key.eq_ignore_ascii_case("serial") {
                    return Ok(Some(ForwardedTarget::Serial(value.trim())));
                }
            }
        }
    }
    Ok(None)
}

fn parse_forwarded_host_value(value: &str) -> Result<IpAddr, AddrParseError> {
    let value = value.trim_matches('"');

    let result = IpAddr::from_str(value);
    if let Ok(ip) = result {
        return Ok(ip);
    }

    // If it failed to parse, maybe it's a bracked ipv6 address, support that
    if let Some(rest) = value.strip_prefix('[')
        && let Some((host, _)) = rest.split_once(']')
    {
        IpAddr::from_str(host)
    } else {
        // Nope, just return the failure
        result
    }
}

fn error_response(error: ProxyError) -> Response<Body> {
    (error.status, error.message).into_response()
}

#[derive(Debug)]
struct ProxyError {
    status: StatusCode,
    message: String,
}

impl From<(StatusCode, String)> for ProxyError {
    fn from((status, message): (StatusCode, String)) -> Self {
        Self { status, message }
    }
}

impl From<(StatusCode, &'static str)> for ProxyError {
    fn from((status, message): (StatusCode, &'static str)) -> Self {
        Self {
            status,
            message: message.to_string(),
        }
    }
}

struct BmcClientInfo {
    http_client: reqwest_middleware::ClientWithMiddleware,
    header_map: HeaderMap,
    credentials: BmcCredentials,
    base_upstream_uri: Uri,
}

#[derive(Clone, PartialEq, Eq)]
enum BmcCredentials {
    UsernamePassword { username: String, password: String },
    SessionToken { token: String },
}

impl BmcCredentials {
    fn apply_to_request(
        self,
        request: reqwest_middleware::RequestBuilder,
    ) -> Result<reqwest_middleware::RequestBuilder, http::header::InvalidHeaderValue> {
        match self {
            Self::UsernamePassword { username, password } => {
                Ok(request.basic_auth(username, Some(password)))
            }
            Self::SessionToken { token } => Ok(request.header(
                REDFISH_AUTH_TOKEN_HEADER,
                http::HeaderValue::from_str(&token)?,
            )),
        }
    }
}

impl TryFrom<forge::BmcCredentials> for BmcCredentials {
    type Error = BmcProxyError;

    fn try_from(value: forge::BmcCredentials) -> Result<Self, Self::Error> {
        match value.r#type {
            Some(forge::bmc_credentials::Type::UsernamePassword(value)) => {
                Ok(Self::UsernamePassword {
                    username: value.username,
                    password: value.password,
                })
            }
            Some(forge::bmc_credentials::Type::SessionToken(value)) => {
                Ok(Self::SessionToken { token: value.token })
            }
            None => Err(BmcProxyError::Api(
                "missing credential type in API response".to_string(),
            )),
        }
    }
}

/// Format a host as a URI authority component, bracketing bare IPv6 literals
/// and appending the port when present.
///
/// A bare IPv6 address such as `2001:db8::1` is not a valid URI authority — it
/// must be bracketed (`[2001:db8::1]`). Without brackets, `http::uri::Authority`
/// parsing (used by the caller to build the upstream URI) rejects the host, and
/// an appended port is misparsed as part of the address.
///
/// The parse guard here covers operator-supplied override hosts, which are
/// genuinely strings; the BMC's own typed `IpAddr` is bracketed off its enum
/// variant by the caller and passes through unchanged (as do IPv4 addresses
/// and hostnames).
fn build_authority(host: Cow<'_, str>, port: Option<u16>) -> Cow<'_, str> {
    let host = if host.parse::<Ipv6Addr>().is_ok() {
        Cow::Owned(format!("[{host}]"))
    } else {
        host
    };
    match port {
        Some(port) => Cow::Owned(format!("{host}:{port}")),
        None => host,
    }
}

async fn create_client(
    ip: IpAddr,
    api_client: &ForgeApiClient,
    credential_cache: &CredentialCache,
    http_client: reqwest_middleware::ClientWithMiddleware,
    bmc_proxy: &Option<HostPortPair>,
) -> Result<BmcClientInfo, BmcProxyError> {
    // Bracket the BMC's own IP off its typed variant (IPv4 renders unchanged),
    // mirroring health::BmcAddr::to_url() and the nv-redfish client.
    let bmc_host = match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let (host, port, add_custom_header) = match bmc_proxy {
        // No override
        None => (Cow::<str>::Owned(bmc_host), None, false),
        // Override the host and port
        Some(HostPortPair::HostAndPort(h, p)) => (Cow::Borrowed(h.as_str()), Some(*p), true),
        // Only override the host
        Some(HostPortPair::HostOnly(h)) => (Cow::Borrowed(h.as_str()), None, true),
        // Only override the port
        Some(HostPortPair::PortOnly(p)) => (Cow::Owned(bmc_host), Some(*p), false),
    };
    let mut header_map = HeaderMap::new();
    if add_custom_header {
        header_map.insert("forwarded", format!("host={ip}").parse().unwrap());
    }
    let credentials = get_bmc_credentials(ip, api_client, credential_cache).await?;

    let base_authority = build_authority(host, port);

    let base_upstream_uri = Uri::builder()
        .scheme("https")
        .authority(base_authority.as_ref())
        .path_and_query("/")
        .build()
        .map_err(|e| {
            BmcProxyError::InternalProxying(format!("Error building upstream URI: {e}"))
        })?;

    Ok(BmcClientInfo {
        http_client,
        header_map,
        credentials,
        base_upstream_uri,
    })
}

async fn get_bmc_credentials(
    ip: IpAddr,
    api_client: &ForgeApiClient,
    credential_cache: &CredentialCache,
) -> Result<BmcCredentials, BmcProxyError> {
    if let Some(credentials) = credential_cache.get(&ip).await {
        tracing::debug!(bmc_ip_address = %ip, "Using cached BMC credentials");
        return Ok(credentials);
    }

    tracing::debug!(bmc_ip_address = %ip, "Fetching BMC credentials from Carbide API");
    let bmc_mac_address = api_client
        .find_mac_address_by_bmc_ip(forge::BmcIp {
            bmc_ip: ip.to_string(),
        })
        .await
        .map_err(|e| BmcProxyError::Api(e.to_string()))?
        .mac_address;

    let credentials: BmcCredentials = api_client
        .get_bmc_credentials(forge::GetBmcCredentialsRequest {
            mac_addr: bmc_mac_address,
        })
        .await
        .map_err(|e| BmcProxyError::Api(e.to_string()))?
        .credentials
        .ok_or(BmcProxyError::NoCredentials(ip))?
        .try_into()?;

    credential_cache.insert(ip, credentials.clone()).await;
    Ok(credentials)
}

fn build_http_client() -> Result<reqwest_middleware::ClientWithMiddleware, BmcProxyError> {
    let client = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(std::time::Duration::from_secs(5)) // Limit connections to 5 seconds
        .timeout(DEFAULT_UPSTREAM_TIMEOUT) // Limit the overall request; classes and uploads override per request
        .pool_max_idle_per_host(4)
        .build()
        .map_err(|err| {
            tracing::error!(error = %err, "build_http_client");
            BmcProxyError::InternalProxying(format!("Http building failed: {err}"))
        })?;
    Ok(reqwest_middleware::ClientBuilder::new(client)
        .with(reqwest_tracing::TracingMiddleware::default())
        .with(SpanIsolationMiddleware)
        .build())
}

async fn evict_cached_credentials(ip: IpAddr, credential_cache: &CredentialCache) {
    if credential_cache.remove(&ip).await.is_some() {
        tracing::info!(bmc_ip_address = %ip, "Evicted cached BMC credentials after upstream auth failure");
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::convert::Infallible;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
    use axum::response::IntoResponse;
    use axum::routing::any;
    use axum_server::tls_rustls::RustlsConfig;
    use bytes::Bytes;
    use carbide_authn::middleware::{AuthContext, ExternalUserInfo, Principal};
    use carbide_instrument::LabelValue;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{
        Case, Check, check_cases_async, check_values, scenarios, value_scenarios,
    };
    use carbide_utils::HostPortPair;
    use http_body_util::BodyExt;
    use mac_address::MacAddress;
    use rpc::forge;
    use rpc::forge::find_bmc_ips_request::LookupBy;
    use rpc::forge_api_client::ForgeApiClient;
    use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_stream::iter;

    use super::{
        BmcCredentials, BmcProxyState, CREDENTIAL_CACHE_IDLE_TTL, ConnectionFailReason,
        CredentialCache, ForwardedTarget, IP_CACHE_TTL, MAX_BUFFERED_BODY_SIZE, MethodLabel,
        RESPONSE_CACHE_MAX_BYTES, ResponseCache, TcpAcceptFailed, TlsCertificateReloadFailed,
        TlsConnectionFailed, UpstreamBody, authorize_principal_allow_list, bmc_proxy_request_span,
        bounded_cache, build_authority, build_http_client, build_response, copy_request_headers,
        create_client, evict_cached_credentials, forwarded_header_value, idle_bounded_cache,
        ip_for_forwarded_target, is_hop_by_hop_header, method_supports_body,
        parse_forwarded_host_value, proxy_request_inner, request_may_have_reached_bmc,
        request_principal_ids, span_status,
    };

    const TEST_CONFIG: &str = r#"
        [tls]
        identity_pemfile_path = ""
        identity_keyfile_path = ""
        root_cafile_path = ""
        admin_root_cafile_path = ""

        [auth]
    "#;

    const AUTHORIZATION_TEST_CONFIG: &str = r#"
        allowed_principals = ["spiffe-service-id/forge-system/carbide-api"]

        [tls]
        identity_pemfile_path = ""
        identity_keyfile_path = ""
        root_cafile_path = ""
        admin_root_cafile_path = ""

        [auth]

        [auth.acls]
        "spiffe-service-id/forge-system/carbide-api" = ["GET /redfish/v1/**"]
    "#;

    const AUTHORIZATION_DENIED_METRIC: &str = "carbide_bmc_proxy_authorization_denied_total";
    const AUTHORIZATION_ERROR_METRIC: &str = "carbide_bmc_proxy_authorization_errors_total";

    #[derive(Clone, Copy)]
    enum ForwardedHeaderCase {
        Missing,
        InvalidUtf8ThenHost,
        HostAmongParameters,
        HostInLaterElement,
        QuotedIpv4Host,
        Mac,
        Serial,
        InvalidHost,
        InvalidMac,
    }

    #[derive(Debug, PartialEq)]
    enum ForwardedTargetSummary {
        None,
        Ip(String),
        Mac(String),
        Serial(String),
        Error(&'static str),
    }

    #[derive(Clone, Copy)]
    enum HeaderCopyCase {
        ContentType,
        Custom,
        Host,
        Authorization,
        AuthToken,
        Forwarded,
        ContentLength,
        Connection,
        Upgrade,
        TraceParent,
        TraceState,
    }

    #[derive(Clone, Copy)]
    enum ProxyOverrideCase {
        Direct,
        HostOnly,
        PortOnly,
        HostAndPort,
    }

    #[derive(Debug, PartialEq)]
    enum CredentialSummary {
        UsernamePassword { username: String, password: String },
        SessionToken { token: String },
    }

    #[derive(Debug, PartialEq)]
    struct ClientSummary {
        base_upstream_uri: String,
        forwarded_header: Option<String>,
        credentials: CredentialSummary,
    }

    fn test_state_with_config(config: &str) -> BmcProxyState {
        let client_config = ForgeClientConfig::default();
        let api_config = ApiConfig::new("https://example.com", &client_config);

        BmcProxyState {
            config: Arc::new(crate::Config::parse(config).expect("test config should parse")),
            api_client: ForgeApiClient::new(&api_config),
            credential_cache: idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL),
            http_client: build_http_client().expect("test HTTP client builds"),
            ip_cache: bounded_cache(IP_CACHE_TTL),
            response_cache: Arc::new(ResponseCache::new(RESPONSE_CACHE_MAX_BYTES)),
        }
    }

    async fn test_state_with_ip_cache(seeded_ips: HashMap<LookupBy, IpAddr>) -> BmcProxyState {
        let state = test_state_with_config(TEST_CONFIG);
        for (lookup_by, ip) in seeded_ips {
            state.ip_cache.insert(lookup_by, ip).await;
        }
        state
    }

    struct AuthorizationRequestCase {
        method: Method,
        path: &'static str,
        principals: Option<Vec<Principal>>,
    }

    #[derive(Debug, PartialEq)]
    struct AuthorizationObservation<T> {
        result: T,
        denial_delta: f64,
        error_delta: f64,
        event_names: Vec<String>,
    }

    fn authorization_request(input: AuthorizationRequestCase) -> Request<Body> {
        let mut request = Request::builder()
            .method(input.method)
            .uri(input.path)
            .body(Body::empty())
            .expect("authorization test request should build");
        if let Some(principals) = input.principals {
            request.extensions_mut().insert(AuthContext::<()> {
                principals,
                authorization: None,
            });
        }
        request
    }

    fn authorization_event_names(logs: &[carbide_instrument::testing::CapturedLog]) -> Vec<String> {
        logs.iter()
            .filter_map(|log| log.field("event_name").map(str::to_owned))
            .collect()
    }

    fn observe_request_acl(
        state: &BmcProxyState,
        input: AuthorizationRequestCase,
    ) -> AuthorizationObservation<bool> {
        let method_label = MethodLabel::from(&input.method).label_value().to_string();
        let request = authorization_request(input);
        let labels = [
            ("authorization_layer", "request_acl"),
            ("method", method_label.as_str()),
        ];
        let metrics = MetricsCapture::start();
        let mut result = false;
        let logs = capture_logs(|| result = state.authorized_principals(&request).is_some());

        AuthorizationObservation {
            result,
            denial_delta: metrics.counter_delta(AUTHORIZATION_DENIED_METRIC, &labels),
            error_delta: metrics.counter_delta(AUTHORIZATION_ERROR_METRIC, &labels),
            event_names: authorization_event_names(&logs),
        }
    }

    fn observe_principal_allow_list(
        state: &BmcProxyState,
        input: AuthorizationRequestCase,
    ) -> AuthorizationObservation<Result<(), StatusCode>> {
        let method_label = MethodLabel::from(&input.method).label_value().to_string();
        let request = authorization_request(input);
        let labels = [
            ("authorization_layer", "principal_allow_list"),
            ("method", method_label.as_str()),
        ];
        let metrics = MetricsCapture::start();
        let mut result = Ok(());
        let logs = capture_logs(|| result = authorize_principal_allow_list(state, &request));

        AuthorizationObservation {
            result,
            denial_delta: metrics.counter_delta(AUTHORIZATION_DENIED_METRIC, &labels),
            error_delta: metrics.counter_delta(AUTHORIZATION_ERROR_METRIC, &labels),
            event_names: authorization_event_names(&logs),
        }
    }

    fn forwarded_headers(case: ForwardedHeaderCase) -> HeaderMap {
        let mut headers = HeaderMap::new();
        match case {
            ForwardedHeaderCase::Missing => {}
            ForwardedHeaderCase::InvalidUtf8ThenHost => {
                headers.append(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_bytes(&[0xff]).expect("non-UTF8 header value"),
                );
                headers.append(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https;host=10.1.2.3"),
                );
            }
            ForwardedHeaderCase::HostAmongParameters => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https;host=10.1.2.3;for=10.0.0.1"),
                );
            }
            ForwardedHeaderCase::HostInLaterElement => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("for=10.0.0.1, proto=https; host=10.2.3.4"),
                );
            }
            ForwardedHeaderCase::QuotedIpv4Host => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static(r#"host="10.3.4.5""#),
                );
            }
            ForwardedHeaderCase::Mac => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https;mac=00:11:22:33:44:55;for=10.0.0.1"),
                );
            }
            ForwardedHeaderCase::Serial => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https; serial = DGX-A100-0001 ; for=10.0.0.1"),
                );
            }
            ForwardedHeaderCase::InvalidHost => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("host=not-an-ip"),
                );
            }
            ForwardedHeaderCase::InvalidMac => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("mac=not-a-mac-address"),
                );
            }
        }
        headers
    }

    fn summarize_forwarded_header(case: ForwardedHeaderCase) -> ForwardedTargetSummary {
        match forwarded_header_value(&forwarded_headers(case)) {
            Ok(Some(ForwardedTarget::Ip(ip))) => ForwardedTargetSummary::Ip(ip.to_string()),
            Ok(Some(ForwardedTarget::Mac(mac))) => ForwardedTargetSummary::Mac(mac.to_string()),
            Ok(Some(ForwardedTarget::Serial(serial))) => {
                ForwardedTargetSummary::Serial(serial.to_string())
            }
            Ok(None) => ForwardedTargetSummary::None,
            Err(super::ForwardedHeaderParseError::Ip(_)) => ForwardedTargetSummary::Error("ip"),
            Err(super::ForwardedHeaderParseError::Mac(_)) => ForwardedTargetSummary::Error("mac"),
        }
    }

    fn header_for_copy_case(case: HeaderCopyCase) -> (HeaderName, HeaderValue) {
        match case {
            HeaderCopyCase::ContentType => (
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            HeaderCopyCase::Custom => (
                HeaderName::from_static("x-request-id"),
                HeaderValue::from_static("request-1"),
            ),
            HeaderCopyCase::Host => (
                axum::http::header::HOST,
                HeaderValue::from_static("bmc.example.com"),
            ),
            HeaderCopyCase::Authorization => (
                axum::http::header::AUTHORIZATION,
                HeaderValue::from_static("Bearer secret"),
            ),
            // One spelling suffices: `HeaderName` lowercases on construction,
            // so a caller's "X-Auth-Token" and "x-auth-token" are the same
            // key by the time the filter sees them.
            HeaderCopyCase::AuthToken => (
                HeaderName::from_static("x-auth-token"),
                HeaderValue::from_static("caller-session"),
            ),
            HeaderCopyCase::Forwarded => (
                HeaderName::from_static("forwarded"),
                HeaderValue::from_static("host=10.0.0.1"),
            ),
            HeaderCopyCase::ContentLength => (
                axum::http::header::CONTENT_LENGTH,
                HeaderValue::from_static("42"),
            ),
            HeaderCopyCase::Connection => (
                axum::http::header::CONNECTION,
                HeaderValue::from_static("keep-alive"),
            ),
            HeaderCopyCase::Upgrade => (
                axum::http::header::UPGRADE,
                HeaderValue::from_static("websocket"),
            ),
            HeaderCopyCase::TraceParent => (
                HeaderName::from_static("traceparent"),
                HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
            ),
            HeaderCopyCase::TraceState => (
                HeaderName::from_static("tracestate"),
                HeaderValue::from_static("vendor=value"),
            ),
        }
    }

    fn copied_header_names(case: HeaderCopyCase) -> Vec<String> {
        // Trace-header filtering asks the global propagator which headers are its own, so the
        // propagator `setup_logging` installs at startup has to be in place for the trace cases to
        // mean anything. Installing it here rather than relying on another test having run keeps
        // this independent of test ordering.
        opentelemetry::global::set_text_map_propagator(
            opentelemetry_sdk::propagation::TraceContextPropagator::new(),
        );

        let (name, value) = header_for_copy_case(case);
        let mut source = HeaderMap::new();
        source.insert(name, value);
        let mut dest = HeaderMap::new();

        copy_request_headers(&source, &mut dest);

        dest.keys().map(|name| name.to_string()).collect()
    }

    fn summarize_credentials(credentials: BmcCredentials) -> CredentialSummary {
        match credentials {
            BmcCredentials::UsernamePassword { username, password } => {
                CredentialSummary::UsernamePassword { username, password }
            }
            BmcCredentials::SessionToken { token } => CredentialSummary::SessionToken { token },
        }
    }

    fn proxy_override(case: ProxyOverrideCase) -> Option<HostPortPair> {
        match case {
            ProxyOverrideCase::Direct => None,
            ProxyOverrideCase::HostOnly => Some(HostPortPair::HostOnly("proxy.local".to_string())),
            ProxyOverrideCase::PortOnly => Some(HostPortPair::PortOnly(8443)),
            ProxyOverrideCase::HostAndPort => {
                Some(HostPortPair::HostAndPort("proxy.local".to_string(), 8443))
            }
        }
    }

    async fn summarize_created_client(case: ProxyOverrideCase) -> Result<ClientSummary, String> {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        // Prepopulate the cache so this test never falls through to the real
        // ForgeApiClient path.
        let credential_cache: CredentialCache = idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL);
        credential_cache
            .insert(
                ip,
                BmcCredentials::UsernamePassword {
                    username: "admin".to_string(),
                    password: "secret".to_string(),
                },
            )
            .await;
        let client_config = ForgeClientConfig::default();
        let api_config = ApiConfig::new("https://example.com", &client_config);
        let api_client = ForgeApiClient::new(&api_config);

        create_client(
            ip,
            &api_client,
            &credential_cache,
            build_http_client().expect("test HTTP client builds"),
            &proxy_override(case),
        )
        .await
        .map(|client| ClientSummary {
            base_upstream_uri: client.base_upstream_uri.to_string(),
            forwarded_header: client.header_map.get("forwarded").map(|value| {
                value
                    .to_str()
                    .expect("forwarded header is UTF-8")
                    .to_string()
            }),
            credentials: summarize_credentials(client.credentials),
        })
        .map_err(|error| error.to_string())
    }

    #[test]
    fn build_authority_brackets_ipv6() {
        value_scenarios!(
            run = |(host, port): (&str, Option<u16>)| {
                let authority = build_authority(Cow::Borrowed(host), port).into_owned();
                // The result is fed into `Uri::builder().authority(..)`, which
                // rejects a bare IPv6 literal — guard that it always parses.
                assert!(
                    authority.parse::<http::uri::Authority>().is_ok(),
                    "produced an invalid authority: {authority}"
                );
                authority
            };
            "IPv4 without port" {
                ("192.0.2.5", None) => "192.0.2.5".to_string(),
            }

            "IPv4 with port" {
                ("192.0.2.5", Some(443)) => "192.0.2.5:443".to_string(),
            }

            "bare IPv6 is bracketed" {
                ("2001:db8::1", None) => "[2001:db8::1]".to_string(),
            }

            "bare IPv6 with port is bracketed" {
                ("2001:db8::1", Some(443)) => "[2001:db8::1]:443".to_string(),
            }

            "already bracketed IPv6 is left unchanged" {
                ("[2001:db8::1]", Some(443)) => "[2001:db8::1]:443".to_string(),
            }

            "hostname is left unchanged" {
                ("bmc.example.com", Some(443)) => "bmc.example.com:443".to_string(),
            }
        );
    }

    #[test]
    fn forwarded_host_value_parsing() {
        value_scenarios!(
            run = |value| {
                parse_forwarded_host_value(value)
                    .ok()
                    .map(|ip| ip.to_string())
            };
            "IPv4" {
                "10.0.0.5" => Some("10.0.0.5".to_string()),
            }

            "raw IPv6" {
                "2001:db8::1" => Some("2001:db8::1".to_string()),
            }

            "quoted bracketed IPv6 with port" {
                "\"[2001:db8::1]:443\"" => Some("2001:db8::1".to_string()),
            }

            "bracketed IPv6 without port" {
                "[2001:db8::2]" => Some("2001:db8::2".to_string()),
            }

            "hostname rejected" {
                "bmc.example.com" => None,
            }

            "IPv4 with port rejected" {
                "10.0.0.5:443" => None,
            }
        );
    }

    #[test]
    fn forwarded_header_targets() {
        value_scenarios!(
            run = summarize_forwarded_header;
            "missing forwarded header" {
                ForwardedHeaderCase::Missing => ForwardedTargetSummary::None,
            }

            "invalid UTF-8 value skipped" {
                ForwardedHeaderCase::InvalidUtf8ThenHost => ForwardedTargetSummary::Ip("10.1.2.3".to_string()),
            }

            "host among parameters" {
                ForwardedHeaderCase::HostAmongParameters => ForwardedTargetSummary::Ip("10.1.2.3".to_string()),
            }

            "host in later element" {
                ForwardedHeaderCase::HostInLaterElement => ForwardedTargetSummary::Ip("10.2.3.4".to_string()),
            }

            "quoted IPv4 host" {
                ForwardedHeaderCase::QuotedIpv4Host => ForwardedTargetSummary::Ip("10.3.4.5".to_string()),
            }

            "MAC target" {
                ForwardedHeaderCase::Mac => ForwardedTargetSummary::Mac("00:11:22:33:44:55".to_string()),
            }

            "serial target" {
                ForwardedHeaderCase::Serial => ForwardedTargetSummary::Serial("DGX-A100-0001".to_string()),
            }

            "invalid host" {
                ForwardedHeaderCase::InvalidHost => ForwardedTargetSummary::Error("ip"),
            }

            "invalid MAC" {
                ForwardedHeaderCase::InvalidMac => ForwardedTargetSummary::Error("mac"),
            }
        );
    }

    /// A request that never built or never connected did not reach the BMC.
    #[tokio::test]
    async fn send_failures_classify_whether_the_bmc_was_reached() {
        let builder_error = reqwest_middleware::Error::from(
            reqwest::Client::new()
                .get("http://")
                .build()
                .expect_err("an empty host cannot build a request"),
        );
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let closed = listener.local_addr().expect("address");
        drop(listener);
        let connect_error = reqwest_middleware::Error::from(
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .build()
                .expect("client builds")
                .get(format!("http://{closed}/"))
                .send()
                .await
                .expect_err("a closed port refuses the connection"),
        );

        // A server that accepts the connection and never answers: the
        // request left the proxy, so its effect on the BMC is unknown.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let silent_addr = silent.local_addr().expect("address");
        let hold_connections = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (socket, _) = silent.accept().await.expect("accept");
                held.push(socket);
            }
        });
        let timeout_error = reqwest_middleware::Error::from(
            reqwest::Client::builder()
                .timeout(Duration::from_millis(200))
                .build()
                .expect("client builds")
                .post(format!("http://{silent_addr}/redfish/v1/UpdateService"))
                .send()
                .await
                .expect_err("a silent server times the request out"),
        );
        hold_connections.abort();

        assert!(!request_may_have_reached_bmc(&builder_error));
        assert!(!request_may_have_reached_bmc(&connect_error));
        assert!(request_may_have_reached_bmc(&timeout_error));
    }

    #[test]
    fn body_method_support() {
        value_scenarios!(
            run = |method| method_supports_body(&method);
            "GET has no upstream body" {
                Method::GET => false,
            }

            "HEAD has no upstream body" {
                Method::HEAD => false,
            }

            "POST supports body" {
                Method::POST => true,
            }

            "PUT supports body" {
                Method::PUT => true,
            }

            "PATCH supports body" {
                Method::PATCH => true,
            }

            "DELETE supports body for Redfish compatibility" {
                Method::DELETE => true,
            }
        );
    }

    #[test]
    fn hop_by_hop_header_detection() {
        value_scenarios!(
            run = is_hop_by_hop_header;
            "connection" {
                "connection" => true,
            }

            "case-insensitive keep-alive" {
                "Keep-Alive" => true,
            }

            "proxy authenticate" {
                "proxy-authenticate" => true,
            }

            "proxy authorization" {
                "proxy-authorization" => true,
            }

            "te" {
                "te" => true,
            }

            "trailer" {
                "trailer" => true,
            }

            "transfer encoding" {
                "transfer-encoding" => true,
            }

            "upgrade" {
                "upgrade" => true,
            }

            "content type is safe" {
                "content-type" => false,
            }
        );
    }

    #[test]
    fn request_header_copying_filters_proxy_owned_headers() {
        value_scenarios!(
            run = copied_header_names;
            "content type copied" {
                HeaderCopyCase::ContentType => vec!["content-type".to_string()],
            }

            "custom header copied" {
                HeaderCopyCase::Custom => vec!["x-request-id".to_string()],
            }

            "host filtered" {
                HeaderCopyCase::Host => vec![],
            }

            "authorization filtered" {
                HeaderCopyCase::Authorization => vec![],
            }

            // The proxy authenticates upstream itself; forwarding a caller's
            // own session token would reach the BMC alongside ours.
            "redfish auth token filtered" {
                HeaderCopyCase::AuthToken => vec![],
            }

            "forwarded filtered" {
                HeaderCopyCase::Forwarded => vec![],
            }

            "content length filtered" {
                HeaderCopyCase::ContentLength => vec![],
            }

            "connection filtered" {
                HeaderCopyCase::Connection => vec![],
            }

            "upgrade filtered" {
                HeaderCopyCase::Upgrade => vec![],
            }

            "traceparent filtered" {
                HeaderCopyCase::TraceParent => vec![],
            }

            "tracestate filtered" {
                HeaderCopyCase::TraceState => vec![],
            }
        );
    }

    #[test]
    fn proxy_request_span_continues_inbound_trace_on_upstream_inject() {
        use opentelemetry::trace::{SpanId, TraceContextExt, TraceId, TracerProvider};
        use opentelemetry_sdk::propagation::TraceContextPropagator;
        use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};
        use trace_propagation::{extract_context, inject_current_context};
        use tracing_subscriber::layer::SubscriberExt;

        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("nico-bmc-proxy-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));

        let inbound_trace = 0x42u128;
        let inbound_span = 0x55u64;
        let mut inbound_headers = http::HeaderMap::new();
        inbound_headers.insert(
            "traceparent",
            format!("00-{:032x}-{:016x}-01", inbound_trace, inbound_span)
                .parse()
                .unwrap(),
        );

        let mut egress_headers = http::HeaderMap::new();
        tracing::subscriber::with_default(subscriber, || {
            let request = Request::builder()
                .uri("/redfish/v1/Systems")
                .header("traceparent", inbound_headers["traceparent"].clone())
                .body(())
                .unwrap();
            let request_span = bmc_proxy_request_span(&request);
            let _entered = request_span.enter();
            inject_current_context(&mut egress_headers);
        });

        let egress_context = extract_context(&egress_headers);
        assert_eq!(
            egress_context.span().span_context().trace_id(),
            TraceId::from(inbound_trace),
        );
        assert_ne!(
            egress_context.span().span_context().span_id(),
            SpanId::from(inbound_span),
        );

        let spans = exporter.get_finished_spans().expect("finished spans");
        let request = spans
            .iter()
            .find(|span| span.name == "bmc_proxy_request")
            .expect("request span exported");
        assert_eq!(
            request.span_context.trace_id(),
            TraceId::from(inbound_trace)
        );
        assert_eq!(request.parent_span_id, SpanId::from(inbound_span));
    }

    #[test]
    fn proxy_request_span_reports_only_server_errors_as_failed() {
        value_scenarios!(
            run = span_status;
            "success" {
                StatusCode::OK => "ok",
            }

            "redirect" {
                StatusCode::TEMPORARY_REDIRECT => "ok",
            }

            "rejected by the allow list" {
                StatusCode::FORBIDDEN => "ok",
            }

            "malformed request" {
                StatusCode::BAD_REQUEST => "ok",
            }

            "upstream unreachable" {
                StatusCode::BAD_GATEWAY => "error",
            }

            "proxy failure" {
                StatusCode::INTERNAL_SERVER_ERROR => "error",
            }
        );
    }

    #[test]
    fn request_principal_identifiers_include_anonymous_fallback() {
        value_scenarios!(
            run = |principals| {
                request_principal_ids(&AuthContext {
                    principals,
                    authorization: None,
                })
            };
            "no authenticated principals" {
                vec![] => vec!["anonymous".to_string()],
            }

            "service principal" {
                vec![Principal::SpiffeServiceIdentifier(
                    "forge-system/carbide-api".to_string(),
                )] => vec![
                    "spiffe-service-id/forge-system/carbide-api".to_string(),
                    "anonymous".to_string(),
                ],
            }

            "machine and external user principals" {
                vec![
                    // Machine identities currently authorize by type token;
                    // the concrete machine id is intentionally not included.
                    Principal::SpiffeMachineIdentifier("machine-1".to_string()),
                    Principal::ExternalUser(ExternalUserInfo::new(
                        Some("nvidia".to_string()),
                        "admin".to_string(),
                        Some("chet".to_string()),
                    )),
                ] => vec![
                    "spiffe-machine-id".to_string(),
                    "external-role/admin".to_string(),
                    "anonymous".to_string(),
                ],
            }
        );
    }

    /// `BmcProxyState::allows` owns the per-principal ACL boundary. An ordinary
    /// policy rejection moves the denial counter, while a missing `AuthContext`
    /// still rejects the request but moves only the middleware-error counter.
    #[test]
    fn request_acl_authorization_emits_the_matching_event() {
        let state = test_state_with_config(AUTHORIZATION_TEST_CONFIG);
        let service_principal =
            || Principal::SpiffeServiceIdentifier("forge-system/carbide-api".to_string());

        check_values(
            [
                Check {
                    scenario: "configured principal and path are allowed",
                    input: AuthorizationRequestCase {
                        method: Method::GET,
                        path: "/redfish/v1/Systems/1",
                        principals: Some(vec![service_principal()]),
                    },
                    expect: AuthorizationObservation {
                        result: true,
                        denial_delta: 0.0,
                        error_delta: 0.0,
                        event_names: vec![],
                    },
                },
                Check {
                    scenario: "configured principal with denied method",
                    input: AuthorizationRequestCase {
                        method: Method::POST,
                        path: "/redfish/v1/Systems/1",
                        principals: Some(vec![service_principal()]),
                    },
                    expect: AuthorizationObservation {
                        result: false,
                        denial_delta: 1.0,
                        error_delta: 0.0,
                        event_names: vec!["bmc_proxy_request_acl_denied".to_string()],
                    },
                },
                Check {
                    scenario: "authentication context is missing",
                    input: AuthorizationRequestCase {
                        method: Method::DELETE,
                        path: "/redfish/v1/Systems/1",
                        principals: None,
                    },
                    expect: AuthorizationObservation {
                        result: false,
                        denial_delta: 0.0,
                        error_delta: 1.0,
                        event_names: vec!["bmc_proxy_auth_context_missing".to_string()],
                    },
                },
            ],
            |input| observe_request_acl(&state, input),
        );
    }

    /// The outer allow-list returns 403 only for a real policy rejection. A
    /// request that never passed through authentication keeps its existing 500
    /// response and is counted as an authorization wiring error instead.
    #[test]
    fn principal_allow_list_authorization_emits_the_matching_event() {
        let state = test_state_with_config(AUTHORIZATION_TEST_CONFIG);
        let service_principal =
            || Principal::SpiffeServiceIdentifier("forge-system/carbide-api".to_string());

        check_values(
            [
                Check {
                    scenario: "configured principal is allowed",
                    input: AuthorizationRequestCase {
                        method: Method::GET,
                        path: "/redfish/v1",
                        principals: Some(vec![service_principal()]),
                    },
                    expect: AuthorizationObservation {
                        result: Ok(()),
                        denial_delta: 0.0,
                        error_delta: 0.0,
                        event_names: vec![],
                    },
                },
                Check {
                    scenario: "principal is not on the allow-list",
                    input: AuthorizationRequestCase {
                        method: Method::PATCH,
                        path: "/redfish/v1",
                        principals: Some(vec![Principal::TrustedCertificate]),
                    },
                    expect: AuthorizationObservation {
                        result: Err(StatusCode::FORBIDDEN),
                        denial_delta: 1.0,
                        error_delta: 0.0,
                        event_names: vec!["bmc_proxy_principal_allow_list_denied".to_string()],
                    },
                },
                Check {
                    scenario: "authentication context is missing",
                    input: AuthorizationRequestCase {
                        method: Method::OPTIONS,
                        path: "/redfish/v1",
                        principals: None,
                    },
                    expect: AuthorizationObservation {
                        result: Err(StatusCode::INTERNAL_SERVER_ERROR),
                        denial_delta: 0.0,
                        error_delta: 1.0,
                        event_names: vec!["bmc_proxy_auth_context_missing".to_string()],
                    },
                },
            ],
            |input| observe_principal_allow_list(&state, input),
        );
    }

    #[tokio::test]
    async fn forwarded_ip_target_resolves_without_lookup() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let state = test_state_with_ip_cache(HashMap::new()).await;

        assert_eq!(
            ip_for_forwarded_target(&ForwardedTarget::Ip(ip), &state)
                .await
                .unwrap(),
            Some(ip)
        );
    }

    #[tokio::test]
    async fn forwarded_mac_target_resolves_from_ip_cache() {
        let mac = MacAddress::from_str("00:11:22:33:44:55").unwrap();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let state =
            test_state_with_ip_cache(HashMap::from([(LookupBy::MacAddress(mac.to_string()), ip)]))
                .await;

        assert_eq!(
            ip_for_forwarded_target(&ForwardedTarget::Mac(mac), &state)
                .await
                .unwrap(),
            Some(ip)
        );
    }

    #[tokio::test]
    async fn forwarded_serial_target_resolves_from_ip_cache() {
        let serial = "DGX-A100-0001";
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let state =
            test_state_with_ip_cache(HashMap::from([(LookupBy::Serial(serial.to_string()), ip)]))
                .await;

        assert_eq!(
            ip_for_forwarded_target(&ForwardedTarget::Serial(serial), &state)
                .await
                .unwrap(),
            Some(ip)
        );
    }

    #[test]
    fn bmc_credentials_convert_from_api_response() {
        scenarios!(
            run = |credentials| {
                BmcCredentials::try_from(credentials)
                    .map(summarize_credentials)
                    .map_err(|error| error.to_string())
            };
            "username and password" {
                forge::BmcCredentials {
                    r#type: Some(forge::bmc_credentials::Type::UsernamePassword(
                        forge::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    )),
                } => Yields(CredentialSummary::UsernamePassword {
                    username: "admin".to_string(),
                    password: "secret".to_string(),
                }),
            }

            "session token" {
                forge::BmcCredentials {
                    r#type: Some(forge::bmc_credentials::Type::SessionToken(
                        forge::SessionToken {
                            token: "token-123".to_string(),
                        },
                    )),
                } => Yields(CredentialSummary::SessionToken {
                    token: "token-123".to_string(),
                }),
            }

            "missing credential type" {
                forge::BmcCredentials { r#type: None } => Fails,
            }
        );
    }

    #[test]
    fn bmc_username_password_credentials_use_basic_auth() {
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
        let request = client.get("https://example.com/redfish/v1");
        let request = BmcCredentials::UsernamePassword {
            username: "admin".to_string(),
            password: "secret".to_string(),
        }
        .apply_to_request(request)
        .expect("credentials should apply")
        .build()
        .expect("request should build");

        let auth = request
            .headers()
            .get(http::header::AUTHORIZATION)
            .expect("authorization header should be present");
        assert!(auth.to_str().unwrap().starts_with("Basic "));
    }

    #[test]
    fn bmc_session_token_credentials_use_redfish_token_header() {
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
        let request = client.get("https://example.com/redfish/v1");
        let request = BmcCredentials::SessionToken {
            token: "token-123".to_string(),
        }
        .apply_to_request(request)
        .expect("credentials should apply")
        .build()
        .expect("request should build");

        assert_eq!(request.headers().get("X-Auth-Token").unwrap(), "token-123");
    }

    #[tokio::test]
    async fn client_creation_applies_proxy_overrides() {
        check_cases_async(
            [
                Case {
                    scenario: "direct BMC IP",
                    input: ProxyOverrideCase::Direct,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://10.0.0.5/".to_string(),
                        forwarded_header: None,
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
                Case {
                    scenario: "proxy host only",
                    input: ProxyOverrideCase::HostOnly,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://proxy.local/".to_string(),
                        forwarded_header: Some("host=10.0.0.5".to_string()),
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
                Case {
                    scenario: "proxy port only",
                    input: ProxyOverrideCase::PortOnly,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://10.0.0.5:8443/".to_string(),
                        forwarded_header: None,
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
                Case {
                    scenario: "proxy host and port",
                    input: ProxyOverrideCase::HostAndPort,
                    expect: Yields(ClientSummary {
                        base_upstream_uri: "https://proxy.local:8443/".to_string(),
                        forwarded_header: Some("host=10.0.0.5".to_string()),
                        credentials: CredentialSummary::UsernamePassword {
                            username: "admin".to_string(),
                            password: "secret".to_string(),
                        },
                    }),
                },
            ],
            summarize_created_client,
        )
        .await;
    }

    /// One observation of [`UpstreamBody::attach`]: whether the built request
    /// carries a buffered body (`as_bytes()` is `Some`), an explicit
    /// `Content-Length`, and a per-request timeout override.
    #[derive(Debug, PartialEq)]
    struct AttachedBodySummary {
        buffered: bool,
        explicit_content_length: Option<String>,
        timeout_secs: Option<u64>,
    }

    async fn observe_attached_body(
        declared_length: Option<u64>,
    ) -> Result<AttachedBodySummary, String> {
        let client = build_http_client().map_err(|e| e.to_string())?;
        let mut headers = HeaderMap::new();
        if let Some(length) = declared_length {
            headers.insert(
                axum::http::header::CONTENT_LENGTH,
                HeaderValue::from_str(&length.to_string()).expect("valid header value"),
            );
        }

        let request = UpstreamBody::prepare(&headers, Body::from("payload"))
            .await
            .map_err(|e| e.to_string())?
            .attach(client.post("https://bmc.invalid/redfish/v1/UpdateService"))
            .map_err(|e| e.message)?
            .build()
            .map_err(|e| e.to_string())?;

        Ok(AttachedBodySummary {
            buffered: request.body().is_some_and(|b| b.as_bytes().is_some()),
            explicit_content_length: request
                .headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .map(|v| v.to_str().expect("UTF-8 header").to_string()),
            timeout_secs: request.timeout().map(Duration::as_secs),
        })
    }

    // The framing contract: small and unsized bodies are buffered so BMCs see
    // exactly what they always did; only a body declared larger than the
    // buffer bound streams, with its length forwarded (hyper would otherwise
    // switch to chunked transfer, which BMC firmwares commonly reject) and a
    // timeout scaled to the transfer instead of the 60-second default.
    #[tokio::test]
    async fn request_bodies_buffer_small_and_stream_large() {
        let large = (MAX_BUFFERED_BODY_SIZE as u64) + 1;
        check_cases_async(
            [
                Case {
                    scenario: "no declared length stays buffered",
                    input: None,
                    expect: Yields(AttachedBodySummary {
                        buffered: true,
                        explicit_content_length: None,
                        timeout_secs: None,
                    }),
                },
                Case {
                    scenario: "small declared length stays buffered",
                    input: Some(1024),
                    expect: Yields(AttachedBodySummary {
                        buffered: true,
                        explicit_content_length: None,
                        timeout_secs: None,
                    }),
                },
                Case {
                    scenario: "length at the bound stays buffered",
                    input: Some(MAX_BUFFERED_BODY_SIZE as u64),
                    expect: Yields(AttachedBodySummary {
                        buffered: true,
                        explicit_content_length: None,
                        timeout_secs: None,
                    }),
                },
                Case {
                    scenario: "length past the bound streams",
                    input: Some(large),
                    expect: Yields(AttachedBodySummary {
                        buffered: false,
                        explicit_content_length: Some(large.to_string()),
                        timeout_secs: Some(super::sized_upload_timeout(large).as_secs()),
                    }),
                },
            ],
            observe_attached_body,
        )
        .await;
    }

    #[test]
    fn sized_upload_timeout_scales_with_declared_length() {
        check_values(
            [
                Check {
                    scenario: "tiny transfer keeps the base budget",
                    input: 1024,
                    expect: 60,
                },
                Check {
                    scenario: "a 50 MB image gets its transfer time",
                    input: 50_000_000,
                    expect: 60 + 5_000,
                },
                // The declared length is caller-supplied; a lie must not buy
                // an unbounded hold on a proxy task and a BMC connection.
                Check {
                    scenario: "a lying length is capped",
                    input: u64::MAX,
                    expect: 4 * 60 * 60,
                },
            ],
            |length| super::sized_upload_timeout(length).as_secs(),
        );
    }

    #[tokio::test]
    async fn evict_cached_credentials_removes_entry_for_ip() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let credential_cache: CredentialCache = idle_bounded_cache(CREDENTIAL_CACHE_IDLE_TTL);
        credential_cache
            .insert(
                ip,
                BmcCredentials::UsernamePassword {
                    username: "admin".to_string(),
                    password: "secret".to_string(),
                },
            )
            .await;

        evict_cached_credentials(ip, &credential_cache).await;

        assert!(credential_cache.get(&ip).await.is_none());
    }

    #[tokio::test]
    async fn build_response_keeps_safe_headers_and_streams_body() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            HeaderValue::from_static("999"),
        );
        headers.insert(
            reqwest::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );

        let body = Body::from_stream(iter([
            Result::<Bytes, Infallible>::Ok(Bytes::from_static(br#"{"value":"#)),
            Result::<Bytes, Infallible>::Ok(Bytes::from_static(br#""ok"}"#)),
        ]));

        let response = build_response(reqwest::StatusCode::OK, &headers, body);

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap(),
            "application/json"
        );
        assert!(
            !response
                .headers()
                .contains_key(reqwest::header::CONTENT_LENGTH)
        );
        assert!(!response.headers().contains_key(reqwest::header::CONNECTION));

        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, Bytes::from_static(br#"{"value":"ok"}"#));
    }

    const TLS_FAILURE_METRIC: &str = "carbide_bmc_proxy_tls_connection_fail_total";

    struct TlsFailureInput {
        reason: &'static str,
        emit: fn(),
    }

    #[derive(Debug, PartialEq)]
    struct TlsFailureObservation {
        counter_delta: f64,
        logs: Vec<TlsFailureLog>,
    }

    #[derive(Debug, PartialEq)]
    struct TlsFailureLog {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        reason: Option<String>,
        error: Option<String>,
        peer_address: Option<String>,
    }

    fn emit_tcp_accept_failure() {
        carbide_instrument::emit(TcpAcceptFailed {
            reason: ConnectionFailReason::TcpConnectionFailure,
            error: "accept failed".to_string(),
        });
    }

    fn emit_tls_certificate_reload_failure() {
        carbide_instrument::emit(TlsCertificateReloadFailed {
            reason: ConnectionFailReason::TlsCertificateInvalid,
            error: "certificate reload failed".to_string(),
        });
    }

    fn emit_tls_connection_failure() {
        carbide_instrument::emit(TlsConnectionFailed {
            reason: ConnectionFailReason::TlsConnectionFailure,
            error: "handshake failed".to_string(),
            peer_address: "192.0.2.20:443"
                .parse::<SocketAddr>()
                .expect("test peer address is valid"),
        });
    }

    fn observe_tls_failure(input: TlsFailureInput) -> TlsFailureObservation {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(input.emit)
            .into_iter()
            .map(|log| {
                let event_name = log.field("event_name").map(str::to_owned);
                let metric_name = log.field("metric_name").map(str::to_owned);
                let reason = log.field("reason").map(str::to_owned);
                let error = log.field("error").map(str::to_owned);
                let peer_address = log.field("peer_address").map(str::to_owned);
                TlsFailureLog {
                    level: log.level,
                    metadata_name: log.metadata_name,
                    message: log.message,
                    event_name,
                    metric_name,
                    reason,
                    error,
                    peer_address,
                }
            })
            .collect();

        TlsFailureObservation {
            counter_delta: metrics.counter_delta(TLS_FAILURE_METRIC, &[("reason", input.reason)]),
            logs,
        }
    }

    fn expected_tls_failure(
        event_name: &str,
        message: &str,
        reason: &str,
        error: &str,
        peer_address: Option<&str>,
    ) -> TlsFailureObservation {
        TlsFailureObservation {
            counter_delta: 1.0,
            logs: vec![TlsFailureLog {
                level: tracing::Level::ERROR,
                metadata_name: event_name.to_string(),
                message: message.to_string(),
                event_name: Some(event_name.to_string()),
                metric_name: Some(TLS_FAILURE_METRIC.to_string()),
                reason: Some(reason.to_string()),
                error: Some(error.to_string()),
                peer_address: peer_address.map(str::to_owned),
            }],
        }
    }

    /// Each accept, certificate reload, or handshake failure writes one ERROR
    /// record and increments exactly one existing `reason` series.
    #[test]
    fn tls_connection_failures_emit_their_metric_and_historical_log() {
        check_values(
            [
                Check {
                    scenario: "tcp accept failure",
                    input: TlsFailureInput {
                        reason: "tcp_connection_failure",
                        emit: emit_tcp_accept_failure,
                    },
                    expect: expected_tls_failure(
                        "bmc_proxy_tcp_accept_failed",
                        "Error accepting connection",
                        "tcp_connection_failure",
                        "accept failed",
                        None,
                    ),
                },
                Check {
                    scenario: "tls certificate reload failure",
                    input: TlsFailureInput {
                        reason: "tls_certificate_invalid",
                        emit: emit_tls_certificate_reload_failure,
                    },
                    expect: expected_tls_failure(
                        "bmc_proxy_tls_certificate_reload_failed",
                        "Error reloading TLS certificate, will retry",
                        "tls_certificate_invalid",
                        "certificate reload failed",
                        None,
                    ),
                },
                Check {
                    scenario: "tls handshake failure",
                    input: TlsFailureInput {
                        reason: "tls_connection_failure",
                        emit: emit_tls_connection_failure,
                    },
                    expect: expected_tls_failure(
                        "bmc_proxy_tls_connection_failed",
                        "error accepting tls connection",
                        "tls_connection_failure",
                        "handshake failed",
                        Some("192.0.2.20:443"),
                    ),
                },
            ],
            observe_tls_failure,
        );
    }

    // The retry contract: a buffered body can be attached again for the one
    // credential-refresh replay, a streamed body cannot (its bytes were
    // consumed by the first attempt), and a bodyless request is trivially
    // replayable.
    #[tokio::test]
    async fn only_buffered_or_absent_bodies_are_replayable() {
        let client = build_http_client().expect("http client");
        let post = || client.post("https://bmc.invalid/redfish/v1/Systems");

        let mut none = UpstreamBody::None;
        assert!(none.is_replayable());
        let _ = none.attach(post()).expect("first attach");
        let _ = none
            .attach(post())
            .expect("bodyless requests replay freely");

        let mut buffered = UpstreamBody::prepare(&HeaderMap::new(), Body::from("payload"))
            .await
            .expect("small bodies buffer");
        assert!(buffered.is_replayable());
        let _ = buffered.attach(post()).expect("first attach");
        let _ = buffered
            .attach(post())
            .expect("a buffered body replays for the credential-refresh retry");

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            HeaderValue::from_str(&(MAX_BUFFERED_BODY_SIZE as u64 + 1).to_string()).unwrap(),
        );
        let mut streamed = UpstreamBody::prepare(&headers, Body::from("payload"))
            .await
            .expect("large declared bodies stream");
        assert!(
            !streamed.is_replayable(),
            "a streamed body must never be replayed"
        );
        let _ = streamed
            .attach(post())
            .expect("first attach consumes the stream");
        let Err(err) = streamed.attach(post()) else {
            panic!("a consumed stream cannot be attached again");
        };
        assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    // --- end to end through a TLS upstream ---------------------------------

    const INVENTORY_PATH: &str = "/redfish/v1/UpdateService/FirmwareInventory";
    const INVENTORY_BODY: &str =
        r#"{"Members":[{"@odata.id":"/redfish/v1/UpdateService/FirmwareInventory/FW_BMC_0"}]}"#;
    const INVENTORY_ETAG: &str = "\"inv-1\"";
    const SYSTEM_PATH: &str = "/redfish/v1/Systems/System_0";
    const HUGE_PATH: &str = "/redfish/v1/UpdateService/FirmwareInventory/Huge";
    /// One byte past what the store accepts.
    const HUGE_LEN: usize = MAX_BUFFERED_BODY_SIZE + 1;
    const FAKE_BMC_IP: &str = "10.0.0.8";
    /// A class name only this test emits, so parallel cache tests emitting
    /// for their own classes cannot move the counters asserted here.
    const E2E_CLASS: &str = "e2e_inventory";

    /// A TLS listener standing in for one BMC. It counts every request by
    /// method and path, serves the firmware inventory with an `ETag`,
    /// accepts writes, and can be switched to answer everything with a 503.
    #[derive(Clone)]
    struct FakeBmc {
        hits: Arc<Mutex<HashMap<String, usize>>>,
        failing: Arc<AtomicBool>,
    }

    impl FakeBmc {
        fn hits(&self, method: Method, path: &str) -> usize {
            self.hits
                .lock()
                .unwrap()
                .get(&format!("{method} {path}"))
                .copied()
                .unwrap_or(0)
        }
    }

    async fn fake_bmc_handler(
        State(bmc): State<FakeBmc>,
        request: Request<Body>,
    ) -> axum::response::Response {
        let key = format!("{} {}", request.method(), request.uri().path());
        *bmc.hits.lock().unwrap().entry(key).or_insert(0) += 1;
        if bmc.failing.load(Ordering::SeqCst) {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        match (request.method().clone(), request.uri().path()) {
            (Method::GET, INVENTORY_PATH) => (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::ETAG, INVENTORY_ETAG),
                ],
                INVENTORY_BODY,
            )
                .into_response(),
            (Method::GET, SYSTEM_PATH) => (
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"PowerState":"On"}"#,
            )
                .into_response(),
            (Method::GET, HUGE_PATH) => (
                [(header::CONTENT_TYPE, "application/json")],
                vec![b'x'; HUGE_LEN],
            )
                .into_response(),
            (Method::OPTIONS, _) => StatusCode::NO_CONTENT.into_response(),
            (Method::PATCH, _) => StatusCode::NO_CONTENT.into_response(),
            (Method::POST, _) => StatusCode::ACCEPTED.into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        }
    }

    fn spawn_fake_bmc() -> (SocketAddr, FakeBmc) {
        // The test binary links two rustls crypto providers (rcgen brings its
        // own), so pick the one the proxy itself uses before any TLS config
        // is built. A second call reports it is already installed.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let params =
            rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()]).expect("cert params");
        let key = rcgen::KeyPair::generate().expect("server key");
        let cert = params.self_signed(&key).expect("self-signed server cert");
        let server_cert: CertificateDer<'static> = cert.der().clone();
        let server_key = PrivateKeyDer::Pkcs8(key.serialize_der().into());
        // The proxy's upstream client accepts any certificate, as it does for
        // real BMCs, so a self-signed one suffices and no client auth is asked.
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![server_cert], server_key)
            .expect("server TLS config");

        let bmc = FakeBmc {
            hits: Arc::new(Mutex::new(HashMap::new())),
            failing: Arc::new(AtomicBool::new(false)),
        };
        let app = Router::new()
            .route("/{*path}", any(fake_bmc_handler))
            .with_state(bmc.clone());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind fake BMC");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("fake BMC address");
        tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, RustlsConfig::from_config(Arc::new(config)))
                .expect("fake BMC listener")
                .serve(app.into_make_service())
                .await
                .expect("fake BMC serves");
        });
        (addr, bmc)
    }

    /// A proxy whose upstream override points at `upstream` and whose
    /// credential cache is seeded for [`FAKE_BMC_IP`], so no nico-api call is
    /// needed. Every request is anonymous and the ACL allows anonymous
    /// everything; `classes` supplies the `[[class]]` tables.
    async fn proxy_with_classes(upstream: SocketAddr, classes: &str) -> BmcProxyState {
        let state = test_state_with_config(&format!(
            r#"
            allowed_principals = ["anonymous"]
            bmc_proxy = "{upstream}"

            [tls]
            identity_pemfile_path = ""
            identity_keyfile_path = ""
            root_cafile_path = ""
            admin_root_cafile_path = ""

            [auth]

            [auth.acls]
            anonymous = ["/**"]

            {classes}
            "#
        ));
        state
            .credential_cache
            .insert(
                FAKE_BMC_IP.parse().unwrap(),
                BmcCredentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "secret".to_string(),
                },
            )
            .await;
        state
    }

    /// The fake BMC's proxy: one cached class covers the firmware inventory
    /// and is held off the cache for an hour after a firmware update post.
    async fn proxy_in_front_of(upstream: SocketAddr) -> BmcProxyState {
        proxy_with_classes(
            upstream,
            &format!(
                r#"
            [[class]]
            name = "{E2E_CLASS}"
            match = ["GET /redfish/v1/UpdateService/FirmwareInventory/**"]
            upstream_timeout = "30s"
            cache = {{ ttl = "1h", stale_if_error = "1h", invalidated_by = ["POST /redfish/v1/UpdateService/**"], hold_after_write = "1h" }}
            "#
            ),
        )
        .await
    }

    fn proxied_request(method: Method, path: &str, extra: &[(HeaderName, &str)]) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("forwarded", format!("host={FAKE_BMC_IP}"));
        for (name, value) in extra {
            builder = builder.header(name.clone(), *value);
        }
        let mut request = builder.body(Body::empty()).expect("request builds");
        request.extensions_mut().insert(AuthContext::<()> {
            principals: vec![],
            authorization: None,
        });
        request
    }

    /// What a caller observes from one proxied request.
    #[derive(Debug, PartialEq)]
    struct Observed {
        status: u16,
        cache: Option<String>,
        has_age: bool,
        body: String,
    }

    async fn send(state: &BmcProxyState, request: Request<Body>) -> Observed {
        let response = match proxy_request_inner(state.clone(), request).await {
            Ok(response) | Err(response) => response,
        };
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let status = response.status().as_u16();
        let cache = header("x-nico-cache");
        let has_age = header("age").is_some();
        // Oversized bodies are part of what this test observes, so the read
        // limit here is not the store's.
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body reads");
        Observed {
            status,
            cache,
            has_age,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    fn observed(status: u16, cache: Option<&str>, has_age: bool, body: &str) -> Observed {
        Observed {
            status,
            cache: cache.map(str::to_owned),
            has_age,
            body: body.to_string(),
        }
    }

    /// The cache as a caller and the BMC see it: a miss goes upstream once,
    /// hits and conditional hits do not, a bypass does but still refreshes
    /// the store, an unrelated write leaves the entry alone, a stored entry
    /// stands in for a failing BMC, a matching write drops the entry and
    /// holds the class off the cache, and an uncached class is forwarded
    /// every time.
    #[tokio::test]
    async fn cached_get_round_trip_through_a_tls_upstream() {
        let (upstream, bmc) = spawn_fake_bmc();
        let state = proxy_in_front_of(upstream).await;
        let metrics = MetricsCapture::start();
        let lookups = |outcome: &str| {
            metrics.counter_delta(
                "carbide_bmc_proxy_cache_lookups_total",
                &[("class", E2E_CLASS), ("outcome", outcome)],
            )
        };

        // A miss fetches once and stores.
        assert_eq!(
            send(&state, proxied_request(Method::GET, INVENTORY_PATH, &[])).await,
            observed(200, Some("miss"), true, INVENTORY_BODY),
        );
        assert_eq!(bmc.hits(Method::GET, INVENTORY_PATH), 1);

        // A hit and a conditional hit never reach the BMC.
        assert_eq!(
            send(&state, proxied_request(Method::GET, INVENTORY_PATH, &[])).await,
            observed(200, Some("hit"), true, INVENTORY_BODY),
        );
        assert_eq!(
            send(
                &state,
                proxied_request(
                    Method::GET,
                    INVENTORY_PATH,
                    &[(header::IF_NONE_MATCH, INVENTORY_ETAG)],
                ),
            )
            .await,
            observed(304, Some("hit"), true, ""),
        );
        assert_eq!(bmc.hits(Method::GET, INVENTORY_PATH), 1);

        // A bypass goes upstream and is not a hit.
        assert_eq!(
            send(
                &state,
                proxied_request(
                    Method::GET,
                    INVENTORY_PATH,
                    &[(header::CACHE_CONTROL, "no-cache")],
                ),
            )
            .await,
            observed(200, Some("bypass"), true, INVENTORY_BODY),
        );
        assert_eq!(bmc.hits(Method::GET, INVENTORY_PATH), 2);

        // A write the policy does not name keeps the entry.
        assert_eq!(
            send(
                &state,
                proxied_request(
                    Method::PATCH,
                    "/redfish/v1/Managers/BMC/NodeManager/Domains/D0",
                    &[],
                ),
            )
            .await
            .status,
            204,
        );
        assert_eq!(
            send(&state, proxied_request(Method::GET, INVENTORY_PATH, &[]))
                .await
                .cache
                .as_deref(),
            Some("hit"),
        );
        assert_eq!(bmc.hits(Method::GET, INVENTORY_PATH), 2);

        // A method that is not a write leaves the entry alone.
        assert_eq!(
            send(
                &state,
                proxied_request(Method::OPTIONS, INVENTORY_PATH, &[])
            )
            .await
            .status,
            204,
        );
        assert_eq!(
            send(&state, proxied_request(Method::GET, INVENTORY_PATH, &[]))
                .await
                .cache
                .as_deref(),
            Some("hit"),
        );

        // A query the cache does not key is forwarded and never stored.
        assert_eq!(
            send(
                &state,
                proxied_request(Method::GET, &format!("{INVENTORY_PATH}?vendor=1"), &[]),
            )
            .await,
            observed(200, Some("uncacheable"), false, INVENTORY_BODY),
        );

        // A miss the BMC answers with 404 passes through, named but unaged.
        let missing = format!("{INVENTORY_PATH}/Missing");
        assert_eq!(
            send(&state, proxied_request(Method::GET, &missing, &[])).await,
            observed(404, Some("miss"), false, ""),
        );

        // A body too large to store is forwarded unstored, at the cost of a
        // second upstream request for this caller.
        let huge = send(&state, proxied_request(Method::GET, HUGE_PATH, &[])).await;
        assert_eq!(
            (
                huge.status,
                huge.cache.as_deref(),
                huge.has_age,
                huge.body.len()
            ),
            (200, Some("miss"), false, HUGE_LEN),
        );
        assert_eq!(bmc.hits(Method::GET, HUGE_PATH), 2);

        // With the BMC failing, a bypass that must fetch falls back to the
        // stored entry.
        bmc.failing.store(true, Ordering::SeqCst);
        assert_eq!(
            send(
                &state,
                proxied_request(
                    Method::GET,
                    INVENTORY_PATH,
                    &[(header::CACHE_CONTROL, "no-cache")],
                ),
            )
            .await,
            observed(200, Some("stale_if_error"), true, INVENTORY_BODY),
        );
        assert_eq!(bmc.hits(Method::GET, INVENTORY_PATH), 4);
        bmc.failing.store(false, Ordering::SeqCst);

        // A write the policy names drops the entry and holds the class off
        // the cache: the next reads are forwarded, named `held`, and store
        // nothing.
        assert_eq!(
            send(
                &state,
                proxied_request(
                    Method::POST,
                    "/redfish/v1/UpdateService/update-multipart",
                    &[]
                ),
            )
            .await
            .status,
            202,
        );
        for _ in 0..2 {
            assert_eq!(
                send(&state, proxied_request(Method::GET, INVENTORY_PATH, &[])).await,
                observed(200, Some("held"), false, INVENTORY_BODY),
            );
        }
        assert_eq!(bmc.hits(Method::GET, INVENTORY_PATH), 6);

        // An uncached class is forwarded every time and carries no cache header.
        for _ in 0..2 {
            assert_eq!(
                send(&state, proxied_request(Method::GET, SYSTEM_PATH, &[])).await,
                observed(200, None, false, r#"{"PowerState":"On"}"#),
            );
        }
        assert_eq!(bmc.hits(Method::GET, SYSTEM_PATH), 2);

        assert_eq!(lookups("miss"), 3.0);
        assert_eq!(lookups("hit"), 4.0);
        assert_eq!(lookups("bypass"), 1.0);
        assert_eq!(lookups("stale_if_error"), 1.0);
        assert_eq!(lookups("held"), 2.0);
        assert_eq!(lookups("uncacheable"), 1.0);
        assert_eq!(lookups("coalesced"), 0.0);
        assert_eq!(
            metrics.counter_delta(
                "carbide_bmc_proxy_cache_invalidations_total",
                &[("class", E2E_CLASS)],
            ),
            1.0,
        );
    }
}
