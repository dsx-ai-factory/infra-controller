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
//! connection after five minutes. A client certificate is optional here;
//! `guard` decides what each caller may do.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use carbide_authn::middleware::ConnectionAttributes;
use carbide_instrument::{Event, LabelValue, MetricFamily, emit};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_rustls::rustls::server::WebPkiClientVerifier;
use tokio_rustls::rustls::{RootCertStore, ServerConfig};
use tokio_rustls::{TlsAcceptor, rustls};
use tokio_util::sync::CancellationToken;
use tower_http::add_extension::AddExtensionLayer;

use crate::config::TlsConfig;
use crate::proxy::{BmcProxyError, BmcProxyState};

const TLS_REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Clone)]
pub(super) struct RefreshableTlsAcceptor {
    acceptor: TlsAcceptor,
    refreshed_at: Instant,
}

impl RefreshableTlsAcceptor {
    fn is_fresh(&self) -> bool {
        self.refreshed_at.elapsed() < TLS_REFRESH_INTERVAL
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

pub(super) struct BmcProxy {
    pub(super) app: Router,
    pub(super) listener: TcpListener,
    pub(super) state: BmcProxyState,
    pub(super) tls_acceptor: RefreshableTlsAcceptor,
}

impl BmcProxy {
    pub(super) async fn run(mut self, cancel_token: CancellationToken) {
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

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};

    use super::{
        ConnectionFailReason, TcpAcceptFailed, TlsCertificateReloadFailed, TlsConnectionFailed,
    };

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
}
