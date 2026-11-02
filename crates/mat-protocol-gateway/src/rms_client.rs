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

//! gRPC transport from the gateway to the RMS services of each machine-a-tron instance.
//!
//! machine-a-tron serves RMS on the same TLS listener as its simulated BMCs and status routes,
//! negotiating HTTP/2 over ALPN. The gateway reaches it at the source `base_url` and trusts it the
//! way the inventory poller does: the `[sources]` CA when one is configured, the system roots
//! otherwise, or nothing at all when `insecure_skip_verify` is set. Plain `http://` base URLs, as
//! used by tests, speak HTTP/2 with prior knowledge.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use eyre::{Context, ensure};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint};
use url::Url;

use crate::config::{RmsConfig, SourceClientConfig};

/// Builds the gRPC channels towards the machine-a-tron instances.
#[derive(Clone)]
pub(crate) struct BackendConnector {
    trust: Trust,
    request_timeout: Duration,
}

/// Which server certificates an `https` base URL is accepted with.
#[derive(Clone)]
enum Trust {
    SystemRoots,
    /// The `[sources]` CA is the only root.
    Ca(Certificate),
    SkipVerification(Arc<dyn ServerCertVerifier>),
}

impl BackendConnector {
    /// Builds the connector from the gateway's source trust settings and RMS timeout.
    ///
    /// The CA file is read here so that a missing or malformed file fails startup rather than
    /// the first forwarded request.
    pub(crate) fn new(sources: &SourceClientConfig, rms: &RmsConfig) -> eyre::Result<Self> {
        let trust = if sources.insecure_skip_verify {
            tracing::warn!(
                "sources.insecure_skip_verify is set; machine-a-tron RMS certificates are not verified"
            );
            Trust::SkipVerification(Arc::new(SkipServerVerification(crypto_provider())))
        } else if let Some(path) = sources.ca_cert_path.as_ref() {
            Trust::Ca(read_ca(path)?)
        } else {
            Trust::SystemRoots
        };
        Ok(Self {
            trust,
            request_timeout: rms.request_timeout,
        })
    }

    /// A channel to the instance at `base_url`; nothing is connected until the first call. The
    /// trust settings apply to `https` URLs and are ignored for `http` ones. The proxy bounds
    /// each call as a whole; the TCP connect and the TLS handshake are bounded here as well so
    /// that a stalled connection attempt does not hold the channel.
    pub(crate) fn channel(&self, base_url: &Url) -> eyre::Result<Channel> {
        let endpoint = Endpoint::from_shared(base_url.to_string())
            .wrap_err_with(|| format!("machine-a-tron base URL {base_url} is not a valid URI"))?;
        let tls = ClientTlsConfig::new().timeout(self.request_timeout);
        let endpoint = match &self.trust {
            Trust::SystemRoots => endpoint.tls_config(tls.with_native_roots()),
            Trust::Ca(certificate) => endpoint.tls_config(tls.ca_certificate(certificate.clone())),
            Trust::SkipVerification(verifier) => {
                endpoint.tls_config_with_verifier(tls, verifier.clone())
            }
        }
        .wrap_err_with(|| format!("configuring TLS towards machine-a-tron {base_url}"))?;
        Ok(endpoint
            .connect_timeout(self.request_timeout)
            .connect_lazy())
    }
}

/// The process-wide rustls provider, which `tonic` builds its client configuration from.
///
/// Other workspace crates link the `ring` provider next to `aws-lc-rs`, so rustls cannot pick a
/// default on its own; installing is a no-op once one is installed.
fn crypto_provider() -> Arc<CryptoProvider> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
}

/// The PEM at `sources.ca_cert_path`, checked to hold at least one certificate.
fn read_ca(path: &Path) -> eyre::Result<Certificate> {
    let pem = std::fs::read(path)
        .wrap_err_with(|| format!("reading sources.ca_cert_path {}", path.display()))?;
    let certificates = rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .wrap_err_with(|| format!("parsing PEM in {}", path.display()))?;
    ensure!(
        !certificates.is_empty(),
        "sources.ca_cert_path {} contains no certificates",
        path.display()
    );
    Ok(Certificate::from_pem(pem))
}

/// Accepts any server certificate. Signatures are still checked so that a broken handshake is
/// rejected; only the chain and the name are ignored, which is what `insecure_skip_verify` asks.
#[derive(Debug)]
struct SkipServerVerification(Arc<CryptoProvider>);

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use axum_server::tls_rustls::RustlsConfig;
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::{Case, check_cases_async};
    use librms::protos::rack_manager::GetVersionRequest;
    use librms::protos::rack_manager::rack_manager_client::RackManagerClient;
    use rms_mock::{RmsMock, RmsMockConfig, StaticInventory};
    use tonic::Code;

    use super::*;

    #[test]
    fn a_missing_or_empty_ca_file_fails_construction() {
        let missing = SourceClientConfig {
            ca_cert_path: Some("/nonexistent/ca.crt".into()),
            ..SourceClientConfig::default()
        };
        let error = BackendConnector::new(&missing, &RmsConfig::default())
            .map(drop)
            .unwrap_err();
        assert!(error.to_string().contains("ca_cert_path"), "{error}");

        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "not a certificate\n").unwrap();
        let empty = SourceClientConfig {
            ca_cert_path: Some(file.path().to_path_buf()),
            ..SourceClientConfig::default()
        };
        let error = BackendConnector::new(&empty, &RmsConfig::default())
            .map(drop)
            .unwrap_err();
        assert!(error.to_string().contains("no certificates"), "{error}");
    }

    /// The RMS mock behind a TLS listener presenting `certified`, a self-signed certificate for
    /// `localhost`.
    async fn serve_rms_over_tls(certified: &rcgen::CertifiedKey<rcgen::KeyPair>) -> Url {
        let _ = crypto_provider();
        let tls = RustlsConfig::from_pem(
            certified.cert.pem().into_bytes(),
            certified.signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let mock = Arc::new(RmsMock::new(
            Arc::new(StaticInventory::new(Vec::new().into())),
            RmsMockConfig::default(),
        ));
        tokio::spawn(
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(rms_mock::router(mock).into_make_service()),
        );
        Url::parse(&format!("https://localhost:{port}/")).unwrap()
    }

    #[tokio::test]
    async fn trust_settings_decide_whether_an_rms_connection_is_made() {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let ca = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca.path(), certified.cert.pem()).unwrap();
        let base_url = serve_rms_over_tls(&certified).await;

        check_cases_async(
            [
                Case {
                    scenario: "the sources CA trusts the instance",
                    input: SourceClientConfig {
                        ca_cert_path: Some(ca.path().to_path_buf()),
                        ..SourceClientConfig::default()
                    },
                    expect: Yields(()),
                },
                Case {
                    scenario: "the system roots do not",
                    input: SourceClientConfig::default(),
                    expect: FailsWith(Code::Unavailable),
                },
                Case {
                    scenario: "insecure_skip_verify accepts the certificate anyway",
                    input: SourceClientConfig {
                        insecure_skip_verify: true,
                        ..SourceClientConfig::default()
                    },
                    expect: Yields(()),
                },
            ],
            |sources| {
                let base_url = base_url.clone();
                async move {
                    let connector = BackendConnector::new(&sources, &RmsConfig::default()).unwrap();
                    RackManagerClient::new(connector.channel(&base_url).unwrap())
                        .get_version(GetVersionRequest {})
                        .await
                        .map(drop)
                        .map_err(|status| status.code())
                }
            },
        )
        .await;
    }
}
