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

//! Accepting connections over TLS, and the connection metrics. The proxy's
//! own identity and trusted CAs are reloaded from disk on the first
//! connection after five minutes, and kept when a reload fails. A client
//! certificate is optional here; `guard` decides what each caller may do.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use carbide_authn::middleware::ConnectionAttributes;
use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_rustls::server::TlsStream;
use tokio_rustls::{TlsAcceptor, rustls};
use tokio_util::sync::CancellationToken;
use tower_http::add_extension::AddExtensionLayer;

use crate::config::TlsConfig;
use crate::proxy::{BmcProxyError, BmcProxyState};

/// Successful loads schedule the next TLS configuration reload after this
/// interval. A new TCP connection triggers the reload once it is due.
const TLS_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// After a failed reload, wait at least this long before retrying on a new TCP
/// connection. Keep using the previous identity and client trust roots.
const TLS_RELOAD_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Deadline for completing TLS, measured from TCP acceptance and including any
/// reload wait. Expiry closes the connection. This timer does not limit the
/// lifetime of established HTTP connections.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Pause between failed TCP accept attempts to prevent a tight retry loop.
/// Proxy shutdown interrupts the pause.
const ACCEPT_RETRY_DELAY: Duration = Duration::from_secs(1);

pub(super) struct RefreshableTlsAcceptor {
    acceptor: TlsAcceptor,
    /// When the next reload from disk is due.
    reload_at: Instant,
}

impl RefreshableTlsAcceptor {
    /// Return the acceptor for a new connection, reloading it if due.
    /// On failure, retain the previous configuration and defer further attempts
    /// by [`TLS_RELOAD_RETRY_INTERVAL`].
    async fn current<F, Fut>(&mut self, reload: F) -> TlsAcceptor
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Self, BmcProxyError>>,
    {
        if Instant::now() >= self.reload_at {
            match reload().await {
                Ok(reloaded) => *self = reloaded,
                Err(e) => {
                    emit(TlsCertificateReloadFailed {
                        error: e.to_string(),
                    });
                    self.reload_at = Instant::now() + TLS_RELOAD_RETRY_INTERVAL;
                }
            }
        }
        self.acceptor.clone()
    }

    pub(super) async fn new(config: TlsConfig) -> Result<Self, BmcProxyError> {
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
#[allow(clippy::enum_variant_names)] // Variant names preserve published metric label values.
enum ConnectionFailReason {
    /// The TCP accept itself errored.
    TcpConnectionFailure,
    /// The TLS handshake errored.
    TlsConnectionFailure,
}

/// Shared counter for TCP accept and TLS handshake failures.
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
/// the on-disk TLS configuration. The connection still uses the last usable
/// acceptor, so this has its own counter rather than counting a failed connection.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_tls_certificate_reload_failed",
    metric_name = "carbide_bmc_proxy_tls_reload_failures_total",
    component = "nico-bmc-proxy",
    log = error,
    metric = counter,
    message = "Error reloading TLS certificate, will retry",
    describe = "Number of failed inbound TLS identity and trust-root reloads"
)]
struct TlsCertificateReloadFailed {
    #[context]
    error: String,
}

/// `TlsConnectionFailed` records a handshake error after the listener knows
/// the peer. It shares the failure counter with TCP accept failures,
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

pub(super) struct BmcProxy {
    pub(super) app: Router,
    pub(super) listener: TcpListener,
    pub(super) state: BmcProxyState,
    pub(super) tls_acceptor: RefreshableTlsAcceptor,
}

impl BmcProxy {
    pub(super) async fn run(mut self, cancel_token: CancellationToken) {
        let http = auto::Builder::new(TokioExecutor::new());
        // Own the connection tasks and observe their panics. Shutdown closes
        // active connections immediately, then waits for their tasks to stop.
        let mut connections = JoinSet::new();
        let mut tls_reloads = JoinSet::new();

        'accepting: loop {
            let (conn, addr) = {
                let incoming = accept_with_backoff(|| self.listener.accept());
                tokio::pin!(incoming);
                loop {
                    tokio::select! {
                        biased;
                        () = cancel_token.cancelled() => break 'accepting,
                        Some(served) = connections.join_next(), if !connections.is_empty() => {
                            report_connection_result(served);
                        }
                        incoming = &mut incoming => break incoming,
                    }
                }
            };
            let handshake_deadline = Instant::now() + TLS_HANDSHAKE_TIMEOUT;

            let tls_config = self.state.config.tls.clone();
            let Some(tls_acceptor) = cancel_token
                .run_until_cancelled(self.tls_acceptor.current(|| {
                    reload_tls(&mut tls_reloads, handshake_deadline, move || {
                        get_tls_acceptor(&tls_config)
                    })
                }))
                .await
            else {
                break;
            };

            connections
                .build_task()
                .name("http conn handler")
                .spawn(serve(
                    conn,
                    addr,
                    tls_acceptor,
                    http.clone(),
                    self.app.clone(),
                    handshake_deadline,
                ))
                // Safety: This only fails if run outside the tokio runtime
                .expect("could not spawn task to handle HTTP connection");
        }

        tracing::info!("nico-bmc-proxy shutting down");
        connections.abort_all();
        while let Some(served) = connections.join_next().await {
            report_connection_result(served);
        }
        stop_tls_reloads(&mut tls_reloads).await;
    }
}

/// Start a reload only when no previous task remains. Timing out or cancelling
/// the wait leaves the blocking task owned by the listener for reuse or shutdown.
async fn reload_tls<F>(
    tasks: &mut JoinSet<Result<RefreshableTlsAcceptor, BmcProxyError>>,
    deadline: Instant,
    load: F,
) -> Result<RefreshableTlsAcceptor, BmcProxyError>
where
    F: FnOnce() -> Result<RefreshableTlsAcceptor, BmcProxyError> + Send + 'static,
{
    if tasks.is_empty() {
        tasks
            .build_task()
            .name("get_tls_acceptor refresh")
            .spawn_blocking(load)
            .expect("TLS reload must spawn inside the Tokio runtime");
    }
    tokio::time::timeout_at(deadline, tasks.join_next())
        .await
        .map_err(|_| {
            BmcProxyError::TlsConfig(
                "tls reload exceeded the inbound handshake deadline".to_string(),
            )
        })?
        .expect("the reload set contains a task until it is joined")
        // Preserve the startup loader's fail-fast policy for a worker panic.
        .expect("TLS reload task panicked")
}

/// Abort queued reloads and join any running one. A blocking file read that has
/// already started cannot be aborted, so shutdown waits for it to finish.
async fn stop_tls_reloads(tasks: &mut JoinSet<Result<RefreshableTlsAcceptor, BmcProxyError>>) {
    tasks.abort_all();
    while let Some(reloaded) = tasks.join_next().await {
        match reloaded {
            Ok(Err(error)) => emit(TlsCertificateReloadFailed {
                error: error.to_string(),
            }),
            Err(error) if !error.is_cancelled() => {
                tracing::error!(%error, "TLS reload task failed");
            }
            _ => {}
        }
    }
}

fn report_connection_result(served: Result<(), JoinError>) {
    if let Err(error) = served
        && !error.is_cancelled()
    {
        tracing::error!(%error, "http connection task failed");
    }
}

/// Listener errors are retried after a fixed pause for the listener's lifetime.
/// The owner polls this future alongside cancellation and completed tasks.
async fn accept_with_backoff<F, Fut, T>(mut accept: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = std::io::Result<T>>,
{
    loop {
        let incoming = accept().await;
        emit(TlsConnectionAttempted);
        match incoming {
            Ok(incoming) => return incoming,
            Err(error) => {
                emit(TcpAcceptFailed {
                    reason: ConnectionFailReason::TcpConnectionFailure,
                    error: error.to_string(),
                });
                tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
            }
        }
    }
}

/// Complete TLS within the deadline set at TCP acceptance, then serve HTTP.
/// The owner stops this task when the proxy shuts down.
async fn serve<IO>(
    conn: IO,
    addr: SocketAddr,
    tls_acceptor: TlsAcceptor,
    http: auto::Builder<TokioExecutor>,
    app: Router,
    handshake_deadline: Instant,
) where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    match handshake(&tls_acceptor, conn, handshake_deadline).await {
        Ok(conn) => {
            let conn = TokioIo::new(conn);
            emit(TlsConnectionSucceeded);

            let (_, session) = conn.inner().get_ref();
            let connection_attributes = {
                let peer_address = addr;
                let peer_certificates = session.peer_certificates().unwrap_or_default().to_vec();
                Arc::new(ConnectionAttributes {
                    peer_address,
                    peer_certificates,
                })
            };
            let conn_attrs_extension_layer = AddExtensionLayer::new(connection_attributes);

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
}

/// Complete TLS using the remaining time before the connection's deadline.
async fn handshake<IO>(
    acceptor: &TlsAcceptor,
    conn: IO,
    deadline: Instant,
) -> std::io::Result<TlsStream<IO>>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout_at(deadline, acceptor.accept(conn))
        .await
        .unwrap_or_else(|_elapsed| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("tls handshake did not complete within {TLS_HANDSHAKE_TIMEOUT:?}"),
            ))
        })
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
        reload_at: Instant::now() + TLS_REFRESH_INTERVAL,
    })
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use axum::Router;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};
    use hyper_util::rt::TokioExecutor;
    use hyper_util::server::conn::auto;
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinSet;
    use tokio::time::Instant;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
    use tokio_rustls::{TlsAcceptor, TlsConnector, rustls};
    use tokio_util::sync::CancellationToken;

    use super::{
        ACCEPT_RETRY_DELAY, BmcProxy, ConnectionFailReason, RefreshableTlsAcceptor,
        TLS_HANDSHAKE_TIMEOUT, TLS_REFRESH_INTERVAL, TLS_RELOAD_RETRY_INTERVAL, TcpAcceptFailed,
        TlsCertificateReloadFailed, TlsConnectionFailed, accept_with_backoff, reload_tls,
        report_connection_result, serve, stop_tls_reloads,
    };
    use crate::proxy::test_support::test_state_with_config;

    const TLS_FAILURE_METRIC: &str = "carbide_bmc_proxy_tls_connection_fail_total";
    const TLS_RELOAD_FAILURE_METRIC: &str = "carbide_bmc_proxy_tls_reload_failures_total";

    struct TlsFailureInput {
        metric: &'static str,
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

        let labels = [("reason", input.reason)];
        TlsFailureObservation {
            counter_delta: metrics.counter_delta(
                input.metric,
                if input.reason.is_empty() {
                    &[]
                } else {
                    &labels
                },
            ),
            logs,
        }
    }

    fn expected_tls_failure(
        metric: &str,
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
                metric_name: Some(metric.to_string()),
                reason: (!reason.is_empty()).then(|| reason.to_string()),
                error: Some(error.to_string()),
                peer_address: peer_address.map(str::to_owned),
            }],
        }
    }

    /// Accept and handshake failures retain their counter and reason labels;
    /// reload failures have a separate counter and retain their ERROR event.
    #[test]
    fn tls_connection_failures_emit_their_metric_and_historical_log() {
        check_values(
            [
                Check {
                    scenario: "tcp accept failure",
                    input: TlsFailureInput {
                        metric: TLS_FAILURE_METRIC,
                        reason: "tcp_connection_failure",
                        emit: emit_tcp_accept_failure,
                    },
                    expect: expected_tls_failure(
                        TLS_FAILURE_METRIC,
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
                        metric: TLS_RELOAD_FAILURE_METRIC,
                        reason: "",
                        emit: emit_tls_certificate_reload_failure,
                    },
                    expect: expected_tls_failure(
                        TLS_RELOAD_FAILURE_METRIC,
                        "bmc_proxy_tls_certificate_reload_failed",
                        "Error reloading TLS certificate, will retry",
                        "",
                        "certificate reload failed",
                        None,
                    ),
                },
                Check {
                    scenario: "tls handshake failure",
                    input: TlsFailureInput {
                        metric: TLS_FAILURE_METRIC,
                        reason: "tls_connection_failure",
                        emit: emit_tls_connection_failure,
                    },
                    expect: expected_tls_failure(
                        TLS_FAILURE_METRIC,
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

    fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    }

    /// An acceptor for a fresh identity for `localhost`, and the CA that
    /// issued it.
    fn test_identity() -> (TlsAcceptor, CertificateDer<'static>) {
        let ca_key = rcgen::KeyPair::generate().expect("CA key");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("CA params");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("CA certificate");
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let key = rcgen::KeyPair::generate().expect("server key");
        let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("server params")
            .signed_by(&key, &issuer)
            .expect("server certificate");
        let config = rustls::ServerConfig::builder_with_provider(crypto_provider())
            .with_safe_default_protocol_versions()
            .expect("TLS protocol versions")
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .expect("server TLS config");
        (TlsAcceptor::from(Arc::new(config)), ca_cert.der().clone())
    }

    /// A partial key update keeps the complete previous TLS configuration.
    /// Restoring the key does not bypass backoff; the due retry loads it.
    #[tokio::test(start_paused = true)]
    async fn failed_reload_keeps_the_acceptor_and_recovers_after_backoff() {
        let metrics = MetricsCapture::start();
        let dir = tempfile::tempdir().expect("TLS files directory");
        let key = include_str!("../../../../dev/certs/localhost/localhost.key");
        let key_path = dir.path().join("identity.key");
        std::fs::write(&key_path, key).expect("write complete key");
        let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../dev/certs/localhost/");
        let config = crate::config::TlsConfig {
            identity_pemfile_path: format!("{fixtures}localhost.crt"),
            identity_keyfile_path: key_path.to_str().expect("key path").to_string(),
            root_cafile_path: format!("{fixtures}ca.crt"),
            admin_root_cafile_path: format!("{fixtures}ca.crt"),
        };
        let mut tls = RefreshableTlsAcceptor::new(config.clone())
            .await
            .expect("initial TLS config");
        let original = tls.acceptor.clone();
        let not_due = tls
            .current(|| async { panic!("fresh configuration must not reload") })
            .await;
        assert!(Arc::ptr_eq(not_due.config(), original.config()));

        std::fs::write(&key_path, "incomplete key").expect("simulate partial key update");
        tls.reload_at = Instant::now();
        let retained = tls
            .current(|| RefreshableTlsAcceptor::new(config.clone()))
            .await;
        assert!(Arc::ptr_eq(retained.config(), original.config()));
        assert_eq!(tls.reload_at, Instant::now() + TLS_RELOAD_RETRY_INTERVAL);
        assert_eq!(metrics.counter_delta(TLS_RELOAD_FAILURE_METRIC, &[]), 1.0);
        assert_eq!(
            metrics.counter_delta(TLS_FAILURE_METRIC, &[("reason", "tls_certificate_invalid")]),
            0.0
        );

        std::fs::write(&key_path, key).expect("finish key update");
        tokio::time::advance(TLS_RELOAD_RETRY_INTERVAL - Duration::from_nanos(1)).await;
        let before_retry = tls
            .current(|| RefreshableTlsAcceptor::new(config.clone()))
            .await;
        assert!(Arc::ptr_eq(before_retry.config(), original.config()));
        tokio::time::advance(Duration::from_nanos(1)).await;
        let recovered = tls.current(|| RefreshableTlsAcceptor::new(config)).await;
        assert!(!Arc::ptr_eq(recovered.config(), original.config()));
        assert_eq!(tls.reload_at, Instant::now() + TLS_REFRESH_INTERVAL);
        assert_eq!(metrics.counter_delta(TLS_RELOAD_FAILURE_METRIC, &[]), 1.0);
    }

    /// A timed-out reload remains owned across retries and cancellation. Even
    /// shutdown must join a blocking task that has already started.
    #[tokio::test(start_paused = true)]
    async fn stalled_reload_is_reused_and_joined_after_timeout_or_cancellation() {
        for cancel in [false, true] {
            let metrics = MetricsCapture::start();
            let (acceptor, _) = test_identity();
            let mut tls = RefreshableTlsAcceptor {
                acceptor: acceptor.clone(),
                reload_at: Instant::now(),
            };
            let original = tls.acceptor.clone();
            let mut tasks = JoinSet::new();
            let (started, wait_started) = tokio::sync::oneshot::channel();
            let (release, wait_release) = std::sync::mpsc::channel();
            let retained = tls
                .current(|| {
                    reload_tls(&mut tasks, Instant::now(), move || {
                        started.send(()).expect("announce the blocking read");
                        wait_release.recv().expect("finish the blocking read");
                        Ok(RefreshableTlsAcceptor {
                            acceptor,
                            reload_at: Instant::now() + TLS_REFRESH_INTERVAL,
                        })
                    })
                })
                .await;
            wait_started.await.expect("the blocking read has started");
            assert!(Arc::ptr_eq(retained.config(), original.config()));
            assert_eq!(tasks.len(), 1);
            assert_eq!(metrics.counter_delta(TLS_RELOAD_FAILURE_METRIC, &[]), 1.0);

            tokio::time::advance(TLS_RELOAD_RETRY_INTERVAL).await;
            let shutdown = CancellationToken::new();
            let deadline = Instant::now()
                + if cancel {
                    TLS_HANDSHAKE_TIMEOUT
                } else {
                    Duration::ZERO
                };
            let waiting = shutdown.run_until_cancelled(tls.current(|| {
                reload_tls(&mut tasks, deadline, || {
                    panic!("a retry must reuse the existing blocking read")
                })
            }));
            let cancel_wait = async {
                if cancel {
                    tokio::task::yield_now().await;
                    shutdown.cancel();
                }
            };
            let (retained, ()) = tokio::join!(waiting, cancel_wait);
            assert_eq!(retained.is_none(), cancel);
            if let Some(retained) = retained {
                assert!(Arc::ptr_eq(retained.config(), original.config()));
            }
            assert_eq!(tasks.len(), 1, "a retry must not spawn another reload");
            assert_eq!(
                metrics.counter_delta(TLS_RELOAD_FAILURE_METRIC, &[]),
                if cancel { 1.0 } else { 2.0 }
            );

            {
                let stopping = stop_tls_reloads(&mut tasks);
                tokio::pin!(stopping);
                tokio::select! {
                    biased;
                    () = &mut stopping => panic!("shutdown must wait for the running read"),
                    () = std::future::ready(()) => {}
                }
                release.send(()).expect("release the stalled read");
                stopping.await;
            }
            assert!(tasks.is_empty(), "shutdown must join the reload task");
        }
    }

    fn localhost() -> ServerName<'static> {
        ServerName::try_from("localhost").expect("server name")
    }

    fn tls_client(ca: CertificateDer<'static>) -> TlsConnector {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).expect("the CA is trusted");
        TlsConnector::from(Arc::new(
            rustls::ClientConfig::builder_with_provider(crypto_provider())
                .with_safe_default_protocol_versions()
                .expect("TLS protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ))
    }

    fn answering_app() -> Router {
        Router::new().route("/", axum::routing::get(|| async { "served" }))
    }

    /// The status line of the answer to a request sent on `conn`, or the
    /// error that ended the exchange.
    async fn status_line(conn: &mut (impl AsyncRead + AsyncWrite + Unpin)) -> String {
        let mut status = [0; 12];
        let exchanged = async {
            conn.write_all(b"GET / HTTP/1.1\r\nhost: localhost\r\n\r\n")
                .await?;
            conn.read_exact(&mut status).await
        };
        match exchanged.await {
            Ok(_) => String::from_utf8_lossy(&status).into_owned(),
            Err(error) => format!("error: {:?}", error.kind()),
        }
    }

    /// How a connection the caller leaves silent ends within `within`.
    async fn ending(conn: &mut (impl AsyncRead + Unpin), within: Duration) -> String {
        let mut byte = [0; 1];
        match tokio::time::timeout(within, conn.read(&mut byte)).await {
            Ok(Ok(0) | Err(_)) => "closed".to_string(),
            other => format!("{other:?}"),
        }
    }

    #[derive(Clone, Copy)]
    enum Caller {
        RequestsAtOnce,
        RequestsAfterIdling,
        NeverSpeaks,
        HandshakeStartsLate,
    }

    /// What `caller` gets from a connection the proxy serves: (the status
    /// line of its answer, or how its connection ended; when, on the paused
    /// clock).
    async fn served(caller: Caller) -> (String, Duration) {
        // Serving emits the connection metrics other tests measure.
        let _metrics = MetricsCapture::start();
        let (acceptor, ca) = test_identity();
        let (mut client_io, server_io) = tokio::io::duplex(64 * 1024);
        let started = tokio::time::Instant::now();
        let mut serving = JoinSet::new();
        serving.spawn(async move {
            if matches!(caller, Caller::HandshakeStartsLate) {
                tokio::time::sleep(2 * TLS_HANDSHAKE_TIMEOUT).await;
            }
            serve(
                server_io,
                "192.0.2.20:443".parse().expect("peer address"),
                acceptor,
                auto::Builder::new(TokioExecutor::new()),
                answering_app(),
                started + TLS_HANDSHAKE_TIMEOUT,
            )
            .await;
        });
        let got = match caller {
            Caller::NeverSpeaks | Caller::HandshakeStartsLate => {
                ending(&mut client_io, 3 * TLS_HANDSHAKE_TIMEOUT).await
            }
            Caller::RequestsAtOnce | Caller::RequestsAfterIdling => {
                let mut tls = tls_client(ca)
                    .connect(localhost(), client_io)
                    .await
                    .expect("the client completes the handshake");
                if let Caller::RequestsAfterIdling = caller {
                    tokio::time::sleep(2 * TLS_HANDSHAKE_TIMEOUT).await;
                }
                status_line(&mut tls).await
            }
        };
        serving
            .join_next()
            .await
            .expect("server task")
            .expect("server does not panic");
        (got, started.elapsed())
    }

    /// A connection that never completes the TLS handshake is dropped at the
    /// deadline instead of holding its task and socket. One that completes
    /// it is served, however long it then idles.
    #[tokio::test(start_paused = true)]
    async fn only_the_tls_handshake_is_bounded() {
        check_cases_async(
            [
                Case {
                    scenario: "a caller that requests at once",
                    input: Caller::RequestsAtOnce,
                    expect: Yields(("HTTP/1.1 200".to_string(), Duration::ZERO)),
                },
                Case {
                    scenario: "a caller that idles past the deadline before its request",
                    input: Caller::RequestsAfterIdling,
                    expect: Yields(("HTTP/1.1 200".to_string(), 2 * TLS_HANDSHAKE_TIMEOUT)),
                },
                Case {
                    scenario: "a caller that never speaks",
                    input: Caller::NeverSpeaks,
                    expect: Yields(("closed".to_string(), TLS_HANDSHAKE_TIMEOUT)),
                },
                Case {
                    scenario: "starting the handshake late does not reset its deadline",
                    input: Caller::HandshakeStartsLate,
                    expect: Yields(("closed".to_string(), 2 * TLS_HANDSHAKE_TIMEOUT)),
                },
            ],
            |caller| async move { Ok::<_, Infallible>(served(caller).await) },
        )
        .await;
    }

    /// No TLS files exist at these paths, so a reload from them fails.
    const CONFIG_WITHOUT_TLS_FILES: &str = r#"
        [tls]
        identity_pemfile_path = ""
        identity_keyfile_path = ""
        root_cafile_path = ""
        admin_root_cafile_path = ""

        [auth]
    "#;

    #[derive(Clone, Copy)]
    enum AtTheProxy {
        AReloadFails,
        Shutdown,
        ShutdownDuringHandshake,
    }

    /// What a caller connected to a running proxy sees when `event` happens
    /// there: (what the caller got, TLS reloads that failed).
    async fn caller_sees(event: AtTheProxy) -> (String, f64) {
        let metrics = MetricsCapture::start();
        let (acceptor, ca) = test_identity();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the proxy");
        let addr = listener.local_addr().expect("the proxy's address");
        let proxy = BmcProxy {
            app: answering_app(),
            listener,
            state: test_state_with_config(CONFIG_WITHOUT_TLS_FILES),
            tls_acceptor: RefreshableTlsAcceptor {
                acceptor,
                reload_at: match event {
                    AtTheProxy::AReloadFails => Instant::now(),
                    AtTheProxy::Shutdown | AtTheProxy::ShutdownDuringHandshake => {
                        Instant::now() + TLS_REFRESH_INTERVAL
                    }
                },
            },
        };
        let shutdown = CancellationToken::new();
        let mut running = JoinSet::new();
        running.spawn(proxy.run(shutdown.clone()));

        let tcp = TcpStream::connect(addr)
            .await
            .expect("connect to the proxy");
        let got = if matches!(event, AtTheProxy::ShutdownDuringHandshake) {
            let mut tcp = tcp;
            tokio::time::timeout(Duration::from_secs(5), async {
                while metrics.counter_delta("carbide_bmc_proxy_tls_connection_attempted_total", &[])
                    == 0.0
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("proxy accepts TCP before shutdown");
            shutdown.cancel();
            ending(&mut tcp, Duration::from_secs(5)).await
        } else {
            let mut tls = tls_client(ca)
                .connect(localhost(), tcp)
                .await
                .expect("the client completes the handshake");
            match event {
                AtTheProxy::AReloadFails => status_line(&mut tls).await,
                AtTheProxy::Shutdown => {
                    shutdown.cancel();
                    ending(&mut tls, Duration::from_secs(5)).await
                }
                AtTheProxy::ShutdownDuringHandshake => unreachable!(),
            }
        };
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), running.join_next())
            .await
            .expect("proxy shutdown finishes")
            .expect("proxy task")
            .expect("the proxy stops");
        let failures = metrics.counter_delta(TLS_RELOAD_FAILURE_METRIC, &[]);
        assert_eq!(
            metrics.counter_delta(TLS_FAILURE_METRIC, &[("reason", "tls_connection_failure")]),
            0.0
        );
        let rendered = metrics.render();
        assert!(rendered.contains(&format!(
            "# HELP {TLS_RELOAD_FAILURE_METRIC} {}",
            <TlsCertificateReloadFailed as carbide_instrument::Event>::DESCRIBE
        )));
        assert!(rendered.contains(&format!("# TYPE {TLS_RELOAD_FAILURE_METRIC} counter")));
        (got, failures)
    }

    /// A running proxy keeps serving through a failed reload of its TLS
    /// identity, and its shutdown ends the connections it is serving.
    #[tokio::test]
    async fn connections_survive_a_failed_reload_but_not_shutdown() {
        check_cases_async(
            [
                Case {
                    scenario: "a due reload fails",
                    input: AtTheProxy::AReloadFails,
                    expect: Yields(("HTTP/1.1 200".to_string(), 1.0)),
                },
                Case {
                    scenario: "the proxy shuts down",
                    input: AtTheProxy::Shutdown,
                    expect: Yields(("closed".to_string(), 0.0)),
                },
                Case {
                    scenario: "shutdown interrupts an incomplete handshake",
                    input: AtTheProxy::ShutdownDuringHandshake,
                    expect: Yields(("closed".to_string(), 0.0)),
                },
            ],
            |event| async move { Ok::<_, Infallible>(caller_sees(event).await) },
        )
        .await;
    }

    /// The listener waits between errors, then retries successfully. Cancelling
    /// during that same wait prevents another attempt.
    #[tokio::test(start_paused = true)]
    async fn accept_backoff_retries_and_observes_cancellation() {
        for cancel in [false, true] {
            let metrics = MetricsCapture::start();
            let mut attempts = 0;
            let started = Instant::now();
            let shutdown = CancellationToken::new();
            let accepted = shutdown.run_until_cancelled(accept_with_backoff(|| {
                attempts += 1;
                std::future::ready(if attempts == 1 {
                    Err(std::io::Error::other("injected accept failure"))
                } else {
                    Ok(())
                })
            }));
            let cancel_after_failure = async {
                if cancel {
                    tokio::time::sleep(ACCEPT_RETRY_DELAY / 2).await;
                    shutdown.cancel();
                }
            };
            let (accepted, ()) = tokio::join!(accepted, cancel_after_failure);
            assert_eq!(accepted, (!cancel).then_some(()));
            assert_eq!(attempts, if cancel { 1 } else { 2 });
            assert_eq!(
                started.elapsed(),
                if cancel {
                    ACCEPT_RETRY_DELAY / 2
                } else {
                    ACCEPT_RETRY_DELAY
                }
            );
            assert_eq!(
                metrics.counter_delta(TLS_FAILURE_METRIC, &[("reason", "tcp_connection_failure")]),
                1.0
            );
        }
    }

    /// Connection panics are reported when joined; intentional shutdown
    /// cancellation is not reported as a task failure.
    #[tokio::test]
    async fn connection_task_panics_are_reported_but_shutdown_is_expected() {
        for panic in [true, false] {
            let mut tasks = JoinSet::new();
            let task = tasks.spawn(async move {
                if panic {
                    panic!("injected connection task panic");
                }
                std::future::pending::<()>().await;
            });
            if !panic {
                task.abort();
            }
            let result = tasks.join_next().await.expect("connection task");
            let logs = capture_logs(|| report_connection_result(result));
            assert_eq!(logs.len(), usize::from(panic));
            if panic {
                assert_eq!(logs[0].message, "http connection task failed");
                assert!(
                    logs[0]
                        .field("error")
                        .expect("join error")
                        .contains("injected connection task panic")
                );
            }
        }
    }
}
