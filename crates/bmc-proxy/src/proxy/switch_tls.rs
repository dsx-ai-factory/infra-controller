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

//! The client the proxy reaches switches' NVOS interfaces on when
//! `[tls.switch]` is set. A switch's NVUE REST API can require a client
//! certificate, so this client presents the proxy's own, and holds the
//! switch's certificate to the switch CA. As switches are reached, the files
//! are checked again every five minutes, in the background: changed files
//! are reloaded, a failed reload keeps the material loaded before and is
//! retried after thirty seconds, and each check reports any certificate in
//! the files outside its validity period.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use carbide_instrument::{Event, LabelValue, emit};
use carbide_utils::reloading::{ReloadError, ReloadSchedule, Reloading};
use tokio_rustls::rustls;
use tokio_rustls::rustls::client::WebPkiServerVerifier;
use tokio_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use tokio_rustls::rustls::pki_types::{
    CertificateDer, DnsName, PrivateKeyDer, ServerName, UnixTime,
};
use tokio_rustls::rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::config::SwitchTlsConfig;
use crate::proxy::{BmcProxyError, TLS_REFRESH_INTERVAL, TLS_RELOAD_RETRY_INTERVAL};

/// The client for switches' NVOS interfaces, built from `[tls.switch]`'s
/// files.
pub(super) struct SwitchClient {
    loaded: Reloading<Loaded>,
}

/// A client built from `[tls.switch]`'s files, and when the certificates in
/// them are valid.
#[derive(Clone)]
struct Loaded {
    client: reqwest_middleware::ClientWithMiddleware,
    certificates: Arc<[Validity]>,
}

impl SwitchClient {
    /// Loads `config`'s files, failing when they do not load, and makes the
    /// client with `client` from the TLS configuration they describe.
    pub(super) fn new(
        config: &SwitchTlsConfig,
        client: impl Fn(
            rustls::ClientConfig,
        ) -> Result<reqwest_middleware::ClientWithMiddleware, BmcProxyError>
        + Send
        + Sync
        + 'static,
    ) -> Result<Self, BmcProxyError> {
        let files = [
            ("ca_cert_path", &config.ca_cert_path),
            ("client_cert_path", &config.client_cert_path),
            ("client_key_path", &config.client_key_path),
        ]
        .map(|(field, path)| (format!("tls.switch.{field}"), PathBuf::from(path)));
        let config = config.clone();
        let loaded = Reloading::new(
            files,
            ReloadSchedule {
                every: TLS_REFRESH_INTERVAL,
                retry: TLS_RELOAD_RETRY_INTERVAL,
            },
            move |[ca, certificate, key]: &[Vec<u8>; 3]| -> Result<Loaded, String> {
                let (tls, certificates) = client_config(&config, ca, certificate, key)?;
                Ok(Loaded {
                    client: client(tls).map_err(|error| error.to_string())?,
                    certificates: certificates.into(),
                })
            },
            |loaded: &Loaded, failure: Option<&ReloadError>| {
                if let Some(error) = failure {
                    emit(SwitchTlsReloadFailed {
                        error: error.to_string(),
                    });
                }
                loaded.report_out_of_validity(unix_now());
            },
        )
        .map_err(|error| BmcProxyError::TlsConfig(error.to_string()))?;
        Ok(Self { loaded })
    }

    /// The client, as last built from the files. A check of the files, when
    /// one is due, runs in the background.
    pub(super) fn current(&self) -> reqwest_middleware::ClientWithMiddleware {
        self.loaded.current().client
    }
}

/// Which of `[tls.switch]`'s files a certificate is in, as a bounded label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum SwitchTlsFile {
    /// `ca_cert_path`.
    CaBundle,
    /// `client_cert_path`.
    ClientCertificate,
}

/// When a certificate in `[tls.switch]`'s files is valid, in Unix seconds.
#[derive(Clone, Copy, Debug)]
struct Validity {
    file: SwitchTlsFile,
    not_before: i64,
    not_after: i64,
}

impl Loaded {
    /// Reports each certificate in the files that is not valid at `now`, in
    /// Unix seconds.
    fn report_out_of_validity(&self, now: i64) {
        for certificate in self
            .certificates
            .iter()
            .filter(|certificate| now < certificate.not_before || now > certificate.not_after)
        {
            emit(SwitchTlsCertificateOutOfValidity {
                file: certificate.file,
                not_before: certificate.not_before,
                not_after: certificate.not_after,
            });
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// A check of `[tls.switch]`'s files could not reload them, and the material
/// loaded before stays in use: once its certificate expires, switches that
/// require one refuse the proxy.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_switch_tls_reload_failed",
    metric_name = "carbide_bmc_proxy_switch_tls_reload_failures_total",
    component = "nico-bmc-proxy",
    log = error,
    metric = counter,
    message = "Error reloading the switch client certificate, keeping the previous one",
    describe = "Number of failed reloads of the tls.switch client identity and CA bundle the proxy uses for switches' NVOS interfaces; the previously loaded ones stay in use"
)]
struct SwitchTlsReloadFailed {
    #[context]
    error: String,
}

/// A certificate in `[tls.switch]`'s files is expired or not yet valid, as
/// the files' load or a check of them found: a switch refuses an expired
/// client certificate. For a CA, this is only a report: rustls-webpki keeps
/// no validity for a trust anchor, so a switch whose certificate an expired
/// CA in the bundle anchors is still accepted.
#[derive(Event)]
#[event(
    event_name = "bmc_proxy_switch_tls_certificate_out_of_validity",
    metric_name = "carbide_bmc_proxy_switch_tls_certificate_out_of_validity_total",
    component = "nico-bmc-proxy",
    log = error,
    metric = counter,
    message = "A switch TLS certificate is expired or not yet valid",
    describe = "Number of times a load or check of the tls.switch files found a certificate in them expired or not yet valid, by the file holding it"
)]
struct SwitchTlsCertificateOutOfValidity {
    #[label]
    file: SwitchTlsFile,
    #[context(value)]
    not_before: i64,
    #[context(value)]
    not_after: i64,
}

/// The TLS configuration `config`'s files describe, from their contents
/// `ca`, `certificate`, and `key`: the proxy's certificate, presented to
/// every switch, and a verifier holding a switch's certificate to the CA
/// bundle, and to `tls_server_name`, or else the switch's IP. Also when each
/// certificate in the files is valid.
fn client_config(
    config: &SwitchTlsConfig,
    ca: &[u8],
    certificate: &[u8],
    key: &[u8],
) -> Result<(rustls::ClientConfig, Vec<Validity>), String> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let anchors = certificates("ca_cert_path", &config.ca_cert_path, ca)?;
    let chain = certificates("client_cert_path", &config.client_cert_path, certificate)?;
    let validity = validity(SwitchTlsFile::CaBundle, &anchors)
        .chain(validity(SwitchTlsFile::ClientCertificate, &chain))
        .collect();
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(anchors);
    let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| invalid("ca_cert_path", &config.ca_cert_path, e))?;
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls.switch: {e}"))?;
    let builder = match &config.tls_server_name {
        None => builder.with_webpki_verifier(verifier),
        Some(name) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SharedNameVerifier {
                name: ServerName::DnsName(
                    DnsName::try_from(name.clone())
                        .map_err(|e| invalid("tls_server_name", name, e))?,
                ),
                verifier,
            })),
    };
    let mut tls = builder
        .with_client_auth_cert(chain, private_key(&config.client_key_path, key)?)
        .map_err(|e| {
            format!(
                "tls.switch.client_cert_path {:?} and client_key_path {:?}: {e}",
                config.client_cert_path, config.client_key_path,
            )
        })?;
    // reqwest takes a preconfigured TLS configuration as is, ALPN included.
    // NVUE serves HTTP/1.1, which every reqwest build speaks; offering h2
    // would need reqwest's http2 feature, which the proxy does not enable.
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok((tls, validity))
}

/// The certificates in `pem`, the file at `path`, `[tls.switch]`'s `field`.
fn certificates(
    field: &str,
    path: &str,
    pem: &[u8],
) -> Result<Vec<CertificateDer<'static>>, String> {
    let certificates = rustls_pemfile::certs(&mut &pem[..])
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| invalid(field, path, e))?;
    if certificates.is_empty() {
        return Err(invalid(field, path, "no certificate found"));
    }
    Ok(certificates)
}

/// The private key in `pem`, the file at `path`, `[tls.switch]`'s
/// `client_key_path`.
fn private_key(path: &str, pem: &[u8]) -> Result<PrivateKeyDer<'static>, String> {
    let field = "client_key_path";
    rustls_pemfile::private_key(&mut &pem[..])
        .map_err(|e| invalid(field, path, e))?
        .ok_or_else(|| invalid(field, path, "no private key found"))
}

/// When each of `certificates`, from `file`, is valid. A certificate that
/// does not parse is left out; rustls refuses it where it matters.
fn validity<'a>(
    file: SwitchTlsFile,
    certificates: &'a [CertificateDer<'_>],
) -> impl Iterator<Item = Validity> + 'a {
    certificates.iter().filter_map(move |certificate| {
        let (_, certificate) = X509Certificate::from_der(certificate.as_ref()).ok()?;
        let validity = certificate.validity();
        Some(Validity {
            file,
            not_before: validity.not_before.timestamp(),
            not_after: validity.not_after.timestamp(),
        })
    })
}

fn invalid(field: &str, value: &str, error: impl std::fmt::Display) -> String {
    format!("tls.switch.{field} {value:?}: {error}")
}

/// Holds every switch's certificate to the one name they share, as
/// `tls_server_name` gives it, instead of to the IP the proxy connected to.
#[derive(Debug)]
struct SharedNameVerifier {
    name: ServerName<'static>,
    verifier: Arc<WebPkiServerVerifier>,
}

impl ServerCertVerifier for SharedNameVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _connected_to: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.verifier
            .verify_server_cert(end_entity, intermediates, &self.name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verifier.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.verifier.supported_verify_schemes()
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::Outcome::{FailsWith, Yields};
    use carbide_test_support::scenarios;
    use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, date_time_ymd};

    use super::SwitchClient;
    use crate::config::{RedirectMode, SwitchTlsConfig};
    use crate::proxy::TLS_REFRESH_INTERVAL;
    use crate::proxy::test_support::SwitchCa;
    use crate::proxy::upstream::build_switch_http_client;

    fn switch_client(config: &SwitchTlsConfig) -> Result<SwitchClient, String> {
        SwitchClient::new(config, |tls| {
            build_switch_http_client(RedirectMode::FollowSameOrigin, tls)
        })
        .map_err(|error| error.to_string())
    }

    /// What is wrong with a `[tls.switch]` profile in a test.
    #[derive(Debug)]
    enum Flaw {
        None,
        MissingCaBundle,
        CaBundleWithoutCertificate,
        KeyOfAnotherCertificate,
    }

    /// Whether a profile with `flaw` makes a client, or else the
    /// `[tls.switch]` field its error names.
    fn load_with(flaw: Flaw) -> Result<(), Option<&'static str>> {
        let dir = tempfile::tempdir().expect("TLS files directory");
        let mut config = SwitchCa::new().switch_tls(dir.path(), Some("switches.test"));
        match flaw {
            Flaw::None => {}
            Flaw::MissingCaBundle => {
                config.ca_cert_path = format!("{}/missing.crt", dir.path().display());
            }
            Flaw::CaBundleWithoutCertificate => {
                std::fs::write(&config.ca_cert_path, "").expect("CA bundle empties");
            }
            Flaw::KeyOfAnotherCertificate => {
                let key = KeyPair::generate().expect("another key");
                std::fs::write(&config.client_key_path, key.serialize_pem()).expect("key writes");
            }
        }
        switch_client(&config).map(drop).map_err(|error| {
            ["ca_cert_path", "client_cert_path"]
                .into_iter()
                .find(|field| error.contains(&format!("tls.switch.{field}")))
        })
    }

    /// A profile that makes no client fails to load, naming the field at
    /// fault, rather than leave the proxy reaching switches without it.
    #[tokio::test]
    async fn a_profile_that_does_not_load_names_what_is_wrong() {
        scenarios!(
            run = load_with;
            "loaded" {
                Flaw::None => Yields(()),
            }

            "refused" {
                Flaw::MissingCaBundle => FailsWith(Some("ca_cert_path")),
                Flaw::CaBundleWithoutCertificate => FailsWith(Some("ca_cert_path")),
                Flaw::KeyOfAnotherCertificate => FailsWith(Some("client_cert_path")),
            }
        );
    }

    /// A check that cannot reload changed files is counted as this client's
    /// failed reload.
    #[tokio::test(start_paused = true)]
    async fn a_check_that_cannot_reload_the_files_is_counted() {
        let metrics = MetricsCapture::start();
        let dir = tempfile::tempdir().expect("TLS files directory");
        let config = SwitchCa::new().switch_tls(dir.path(), None);
        let client = switch_client(&config).expect("the profile loads");

        std::fs::write(&config.client_key_path, "not a key").expect("the key rewrites");
        tokio::time::advance(TLS_REFRESH_INTERVAL).await;
        client.current();
        client.loaded.settled().await;

        assert_eq!(
            metrics.counter_delta("carbide_bmc_proxy_switch_tls_reload_failures_total", &[]),
            1.0
        );
    }

    /// An expired client certificate still loads, so BMC traffic is not
    /// stopped with the proxy, but its load and every check after it report
    /// it.
    #[tokio::test(start_paused = true)]
    async fn an_expired_client_certificate_is_reported_at_each_check() {
        let metrics = MetricsCapture::start();
        let dir = tempfile::tempdir().expect("TLS files directory");
        let ca = SwitchCa::new();
        let config = ca.switch_tls(dir.path(), None);
        let mut expired = CertificateParams::new(Vec::new()).expect("client parameters");
        expired.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        expired.not_before = date_time_ymd(2020, 1, 1);
        expired.not_after = date_time_ymd(2021, 1, 1);
        let (certificate, key) = ca.issue_with(expired);
        std::fs::write(&config.client_cert_path, certificate.pem()).expect("certificate writes");
        std::fs::write(&config.client_key_path, key.serialize_pem()).expect("key writes");
        let reported = || {
            metrics.counter_delta(
                "carbide_bmc_proxy_switch_tls_certificate_out_of_validity_total",
                &[("file", "client_certificate")],
            )
        };

        let client = switch_client(&config).expect("an expired certificate loads");
        assert_eq!(reported(), 1.0, "reported when the files load");

        tokio::time::advance(TLS_REFRESH_INTERVAL).await;
        client.current();
        client.loaded.settled().await;
        assert_eq!(
            reported(),
            2.0,
            "reported again by a check of unchanged files"
        );
    }
}
