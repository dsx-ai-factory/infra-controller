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

//! TLS helpers for outbound health connections.
//!
//! Switch collectors use `[tls.switch]`; OTLP targets use their own `tls`
//! tables. This module owns HTTP and gRPC TLS construction inside the health
//! crate so outbound transport security does not depend on NICo API certificate
//! settings. Periodic HTTP collectors share one cached mTLS client per profile;
//! the cache refreshes on the configured reload cadence so certificate changes
//! are adopted without rebuilding a client for every switch target. Streaming
//! collectors build a new TLS config when they reconnect. OTLP drains reload
//! their target profile periodically and adopt it only after a replacement
//! connection succeeds.

use std::fmt;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http::{HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::{BodyExt, Empty};
use hyper_rustls::{FixedServerNameResolver, HttpsConnector};
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::RootCertStore;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use thiserror::Error;
use tokio::fs;
use tokio::sync::RwLock;
use tonic::transport::{
    Certificate as TonicCertificate, ClientTlsConfig, Identity as TonicIdentity,
};
use url::Url;
use x509_parser::prelude::*;

use crate::config::{MtlsProfileConfig, OtlpTlsConfig};

type HyperMtlsClient = HyperClient<HttpsConnector<HttpConnector>, Empty<Bytes>>;

/// Role for one TLS profile material file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TlsMaterialKind {
    CaBundle,
    ClientCertificate,
    ClientKey,
}

impl fmt::Display for TlsMaterialKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CaBundle => f.write_str("CA bundle"),
            Self::ClientCertificate => f.write_str("client certificate"),
            Self::ClientKey => f.write_str("client key"),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum TlsError {
    #[error("failed to read TLS profile {kind} at {path}: {source}")]
    Read {
        kind: TlsMaterialKind,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("TLS profile {kind} at {path} is empty")]
    Empty {
        kind: TlsMaterialKind,
        path: PathBuf,
    },

    #[error("failed to parse TLS profile {kind} at {path}: {message}")]
    Parse {
        kind: TlsMaterialKind,
        path: PathBuf,
        message: String,
    },

    #[error(
        "TLS profile {kind} certificate at {path} is not valid before unix timestamp {not_before}"
    )]
    NotYetValid {
        kind: TlsMaterialKind,
        path: PathBuf,
        not_before: i64,
    },

    #[error("TLS profile {kind} certificate at {path} expired at unix timestamp {not_after}")]
    Expired {
        kind: TlsMaterialKind,
        path: PathBuf,
        not_after: i64,
    },

    #[error("TLS profile CA bundle at {path} contains no usable trust anchors")]
    NoTrustedCa { path: PathBuf },

    #[error("TLS profile client certificate and key are invalid or do not match: {source}")]
    InvalidIdentity {
        #[source]
        source: rustls::Error,
    },

    /// Expected client identity material was absent while constructing mTLS.
    #[error("TLS profile requires a client certificate and key")]
    MissingClientIdentity,

    /// An OTLP TLS profile configured only one client identity path.
    #[error("TLS profile client certificate and key must be configured together")]
    IncompleteClientIdentity,
}

struct TlsMaterial {
    ca_pem: Vec<u8>,
    client_identity: Option<ClientIdentityMaterial>,
}

struct ClientIdentityMaterial {
    client_cert_pem: Vec<u8>,
    client_key_pem: Vec<u8>,
}

pub(crate) async fn preflight(config: &MtlsProfileConfig) -> Result<(), TlsError> {
    read_validated_material(
        &config.ca_cert_path,
        Some((&config.client_cert_path, &config.client_key_path)),
    )
    .await
    .map(|_| ())
}

/// Reads and validates the CA and optional client identity for one OTLP target.
///
/// # Errors
///
/// Returns an error when identity paths are incomplete or configured material
/// cannot be read or validated.
pub(crate) async fn otlp_preflight(config: &OtlpTlsConfig) -> Result<(), TlsError> {
    read_validated_material(&config.ca_cert_path, otlp_client_identity_paths(config)?)
        .await
        .map(|_| ())
}

/// Cloneable HTTP client built from one validated mTLS profile.
///
/// The client owns its connection pool and TLS configuration. Clones are cheap
/// handles to the same pool; TLS material changes require building a new
/// `MtlsHttpClient`.
#[derive(Clone)]
pub(crate) struct MtlsHttpClient {
    inner: HyperMtlsClient,
}

/// Reloading provider for the shared switch mTLS HTTP client.
///
/// One provider is shared by all periodic HTTP switch collectors using the same
/// `[tls.switch]` profile. The provider rebuilds the underlying HTTP client
/// only after the reload interval expires, so cert file reads, parsing, and
/// connection-pool construction are not repeated per switch target.
#[derive(Clone)]
pub(crate) struct MtlsHttpClientProvider {
    inner: Arc<MtlsHttpClientProviderInner>,
}

struct MtlsHttpClientProviderInner {
    config: MtlsProfileConfig,
    reload_interval: Duration,
    state: RwLock<MtlsHttpClientProviderState>,
}

struct MtlsHttpClientProviderState {
    /// Cached client for the most recently accepted TLS material.
    client: Option<MtlsHttpClient>,

    /// Instant at which callers must attempt to rebuild from disk again.
    next_reload: Option<Instant>,
}

/// Fully buffered response from the small mTLS HTTP client wrapper.
pub(crate) struct MtlsHttpResponse {
    pub(crate) status: StatusCode,
    pub(crate) body: Bytes,
}

#[derive(Debug, Error)]
pub(crate) enum MtlsHttpError {
    #[error("failed to create mTLS HTTP request: {source}")]
    Request {
        #[source]
        source: http::Error,
    },

    #[error("failed to execute mTLS HTTP request: {source}")]
    Execute {
        #[source]
        source: hyper_util::client::legacy::Error,
    },

    #[error("failed to read mTLS HTTP response body: {source}")]
    Body {
        #[source]
        source: hyper::Error,
    },

    #[error("mTLS HTTP request timed out after {timeout:?}")]
    Timeout {
        timeout: Duration,
        #[source]
        source: tokio::time::error::Elapsed,
    },
}

impl MtlsHttpClient {
    /// Sends one GET request and buffers the whole response body.
    ///
    /// `request_timeout` covers both request execution and response body
    /// collection so callers keep the same timeout shape as the previous
    /// `reqwest` implementation.
    pub(crate) async fn get(
        &self,
        url: &Url,
        headers: impl IntoIterator<Item = (HeaderName, HeaderValue)>,
        request_timeout: Duration,
    ) -> Result<MtlsHttpResponse, MtlsHttpError> {
        let mut builder = Request::builder().method(Method::GET).uri(url.as_str());

        for (name, value) in headers {
            builder = builder.header(name, value);
        }

        let request = builder
            .body(Empty::<Bytes>::new())
            .map_err(|source| MtlsHttpError::Request { source })?;

        let response = tokio::time::timeout(request_timeout, async {
            let response = self
                .inner
                .request(request)
                .await
                .map_err(|source| MtlsHttpError::Execute { source })?;

            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|source| MtlsHttpError::Body { source })?
                .to_bytes();

            Ok(MtlsHttpResponse { status, body })
        })
        .await
        .map_err(|source| MtlsHttpError::Timeout {
            timeout: request_timeout,
            source,
        })??;

        Ok(response)
    }
}

impl MtlsHttpClientProvider {
    /// Creates a provider for one mTLS profile and reload cadence.
    ///
    /// The cadence is chosen by the discovery loop from the shortest enabled
    /// periodic HTTP switch collector interval.
    pub(crate) fn new(config: MtlsProfileConfig, reload_interval: Duration) -> Self {
        Self {
            inner: Arc::new(MtlsHttpClientProviderInner {
                config,
                reload_interval,
                state: RwLock::new(MtlsHttpClientProviderState {
                    client: None,
                    next_reload: None,
                }),
            }),
        }
    }

    /// Returns the cached client or rebuilds it from current certificate files.
    ///
    /// Once the reload window expires, callers must see the new material or the
    /// reload error. A failed reload does not advance `next_reload`, so the
    /// provider does not silently keep returning stale cert material after the
    /// configured reload point.
    pub(crate) async fn client(&self) -> Result<MtlsHttpClient, TlsError> {
        let now = Instant::now();

        {
            let state = self.inner.state.read().await;
            if let (Some(client), Some(next_reload)) = (&state.client, state.next_reload)
                && now < next_reload
            {
                return Ok(client.clone());
            }
        }

        let mut state = self.inner.state.write().await;
        let now = Instant::now();

        if let (Some(client), Some(next_reload)) = (&state.client, state.next_reload)
            && now < next_reload
        {
            return Ok(client.clone());
        }

        // Hold the Tokio write lock while rebuilding so concurrent switch
        // collectors collapse into one file read, cert validation, and client
        // construction. This is the expensive operation this cache exists to
        // serialize.
        let client = http_client(&self.inner.config).await?;

        state.next_reload = Some(now + self.inner.reload_interval);
        state.client = Some(client.clone());

        Ok(client)
    }
}

/// Builds an HTTP client from the current mTLS profile material.
///
/// The URL host remains the discovered switch IP. When
/// `[tls.switch].tls_server_name` is set, hyper-rustls uses that value only for
/// SNI and certificate verification.
pub(crate) async fn http_client(config: &MtlsProfileConfig) -> Result<MtlsHttpClient, TlsError> {
    let material = read_validated_material(
        &config.ca_cert_path,
        Some((&config.client_cert_path, &config.client_key_path)),
    )
    .await?;

    let tls_config = http_tls_config(config, &material)?;
    let mut http_connector = HttpConnector::new();

    http_connector.enforce_http(false);

    let connector = match &config.tls_server_name {
        Some(tls_server_name) => {
            let server_name = ServerName::try_from(tls_server_name.clone()).map_err(|source| {
                TlsError::Parse {
                    kind: TlsMaterialKind::CaBundle,
                    path: config.ca_cert_path.clone(),
                    message: format!("invalid TLS server name: {source}"),
                }
            })?;

            hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(tls_config)
                .https_only()
                .with_server_name_resolver(FixedServerNameResolver::new(server_name))
                .enable_http1()
                .enable_http2()
                .wrap_connector(http_connector)
        }
        None => hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_only()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http_connector),
    };

    Ok(MtlsHttpClient {
        inner: HyperClient::builder(TokioExecutor::new()).build(connector),
    })
}

fn http_tls_config(
    config: &MtlsProfileConfig,
    material: &TlsMaterial,
) -> Result<rustls::ClientConfig, TlsError> {
    let ca_certs = parse_certificates(
        TlsMaterialKind::CaBundle,
        &config.ca_cert_path,
        &material.ca_pem,
    )?;

    let mut root_store = RootCertStore::empty();
    let (accepted, ignored) = root_store.add_parsable_certificates(ca_certs);

    if accepted == 0 {
        return Err(TlsError::NoTrustedCa {
            path: config.ca_cert_path.clone(),
        });
    }

    if ignored != 0 {
        return Err(TlsError::Parse {
            kind: TlsMaterialKind::CaBundle,
            path: config.ca_cert_path.clone(),
            message: format!("{ignored} certificate(s) could not be used as trust anchors"),
        });
    }

    let client_identity = material
        .client_identity
        .as_ref()
        .ok_or(TlsError::MissingClientIdentity)?;

    let client_certs = parse_certificates(
        TlsMaterialKind::ClientCertificate,
        &config.client_cert_path,
        &client_identity.client_cert_pem,
    )?;

    let client_key = parse_private_key(&config.client_key_path, &client_identity.client_key_pem)?;

    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|source| TlsError::InvalidIdentity { source })?
    .with_root_certificates(root_store)
    .with_client_auth_cert(client_certs, client_key)
    .map_err(|source| TlsError::InvalidIdentity { source })
}

/// Builds a tonic client TLS configuration from the current mTLS profile material.
///
/// Existing gRPC channels keep their current TLS session. Streaming collectors
/// use refreshed files when they reconnect and build a new channel.
pub(crate) async fn tonic_tls_config(
    config: &MtlsProfileConfig,
) -> Result<ClientTlsConfig, TlsError> {
    let material = read_validated_material(
        &config.ca_cert_path,
        Some((&config.client_cert_path, &config.client_key_path)),
    )
    .await?;

    Ok(tonic_tls_config_from_material(
        &material,
        &config.tls_server_name,
    ))
}

/// Builds a Tonic TLS configuration for one OTLP target.
///
/// # Errors
///
/// Returns an error when identity paths are incomplete or configured material
/// cannot be read or validated.
pub(crate) async fn otlp_tonic_tls_config(
    config: &OtlpTlsConfig,
) -> Result<ClientTlsConfig, TlsError> {
    let material =
        read_validated_material(&config.ca_cert_path, otlp_client_identity_paths(config)?).await?;

    Ok(tonic_tls_config_from_material(
        &material,
        &config.tls_server_name,
    ))
}

fn otlp_client_identity_paths(config: &OtlpTlsConfig) -> Result<Option<(&Path, &Path)>, TlsError> {
    match (&config.client_cert_path, &config.client_key_path) {
        (Some(client_cert_path), Some(client_key_path)) => {
            Ok(Some((client_cert_path, client_key_path)))
        }
        (None, None) => Ok(None),
        _ => Err(TlsError::IncompleteClientIdentity),
    }
}

fn tonic_tls_config_from_material(
    material: &TlsMaterial,
    tls_server_name: &Option<String>,
) -> ClientTlsConfig {
    let mut tls_config =
        ClientTlsConfig::new().ca_certificate(TonicCertificate::from_pem(&material.ca_pem));

    if let Some(client_identity) = &material.client_identity {
        tls_config = tls_config.identity(TonicIdentity::from_pem(
            &client_identity.client_cert_pem,
            &client_identity.client_key_pem,
        ));
    }

    if let Some(tls_server_name) = tls_server_name {
        // This override changes only TLS SNI and certificate verification. The
        // connection still uses the configured endpoint URI.
        tls_config = tls_config.domain_name(tls_server_name.clone());
    }

    tls_config
}

async fn read_validated_material(
    ca_cert_path: &Path,
    client_identity_paths: Option<(&Path, &Path)>,
) -> Result<TlsMaterial, TlsError> {
    ensure_rustls_provider();

    let ca_pem = read_material(TlsMaterialKind::CaBundle, ca_cert_path).await?;

    let client_identity = match client_identity_paths {
        Some((client_cert_path, client_key_path)) => {
            let (client_cert_pem, client_key_pem) = tokio::try_join!(
                read_material(TlsMaterialKind::ClientCertificate, client_cert_path),
                read_material(TlsMaterialKind::ClientKey, client_key_path),
            )?;

            Some((
                client_cert_path,
                client_key_path,
                client_cert_pem,
                client_key_pem,
            ))
        }
        None => None,
    };

    let ca_certs = parse_certificates(TlsMaterialKind::CaBundle, ca_cert_path, &ca_pem)?;
    let now = unix_timestamp_now();

    validate_certificate_times(TlsMaterialKind::CaBundle, ca_cert_path, &ca_certs, now)?;
    validate_root_store(ca_cert_path, ca_certs)?;

    let client_identity = match client_identity {
        Some((client_cert_path, client_key_path, client_cert_pem, client_key_pem)) => {
            let client_certs = parse_certificates(
                TlsMaterialKind::ClientCertificate,
                client_cert_path,
                &client_cert_pem,
            )?;

            let client_key = parse_private_key(client_key_path, &client_key_pem)?;

            validate_certificate_times(
                TlsMaterialKind::ClientCertificate,
                client_cert_path,
                &client_certs,
                now,
            )?;

            validate_client_identity(client_certs, client_key)?;

            Some(ClientIdentityMaterial {
                client_cert_pem,
                client_key_pem,
            })
        }
        None => None,
    };

    Ok(TlsMaterial {
        ca_pem,
        client_identity,
    })
}

async fn read_material(kind: TlsMaterialKind, path: &Path) -> Result<Vec<u8>, TlsError> {
    let data = fs::read(path).await.map_err(|source| TlsError::Read {
        kind,
        path: path.to_path_buf(),
        source,
    })?;

    if data.iter().all(u8::is_ascii_whitespace) {
        return Err(TlsError::Empty {
            kind,
            path: path.to_path_buf(),
        });
    }

    Ok(data)
}

fn parse_certificates(
    kind: TlsMaterialKind,
    path: &Path,
    pem: &[u8],
) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let mut reader = BufReader::new(pem);
    let certificates = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| TlsError::Parse {
            kind,
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;

    if certificates.is_empty() {
        return Err(TlsError::Parse {
            kind,
            path: path.to_path_buf(),
            message: "no certificate PEM blocks found".to_string(),
        });
    }

    Ok(certificates)
}

fn parse_private_key(path: &Path, pem: &[u8]) -> Result<PrivateKeyDer<'static>, TlsError> {
    let mut reader = BufReader::new(pem);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|source| TlsError::Parse {
            kind: TlsMaterialKind::ClientKey,
            path: path.to_path_buf(),
            message: source.to_string(),
        })?
        .ok_or_else(|| TlsError::Parse {
            kind: TlsMaterialKind::ClientKey,
            path: path.to_path_buf(),
            message: "no supported private key PEM block found".to_string(),
        })
}

fn validate_certificate_times(
    kind: TlsMaterialKind,
    path: &Path,
    certificates: &[CertificateDer<'static>],
    now: i64,
) -> Result<(), TlsError> {
    for certificate in certificates {
        let (_, x509) =
            X509Certificate::from_der(certificate.as_ref()).map_err(|source| TlsError::Parse {
                kind,
                path: path.to_path_buf(),
                message: format!("invalid X.509 certificate: {source}"),
            })?;

        let validity = x509.validity();
        let not_before = validity.not_before.timestamp();
        let not_after = validity.not_after.timestamp();

        if now < not_before {
            return Err(TlsError::NotYetValid {
                kind,
                path: path.to_path_buf(),
                not_before,
            });
        }

        if now > not_after {
            return Err(TlsError::Expired {
                kind,
                path: path.to_path_buf(),
                not_after,
            });
        }
    }

    Ok(())
}

fn validate_root_store(
    path: &Path,
    certificates: Vec<CertificateDer<'static>>,
) -> Result<(), TlsError> {
    let mut root_store = RootCertStore::empty();
    let (accepted, ignored) = root_store.add_parsable_certificates(certificates);

    if accepted == 0 {
        return Err(TlsError::NoTrustedCa {
            path: path.to_path_buf(),
        });
    }

    if ignored != 0 {
        return Err(TlsError::Parse {
            kind: TlsMaterialKind::CaBundle,
            path: path.to_path_buf(),
            message: format!("{ignored} certificate(s) could not be used as trust anchors"),
        });
    }

    Ok(())
}

fn validate_client_identity(
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<(), TlsError> {
    let root_store = RootCertStore::empty();

    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|source| TlsError::InvalidIdentity { source })?
    .with_root_certificates(root_store);

    builder
        .with_client_auth_cert(certificates, key)
        .map(|_| ())
        .map_err(|source| TlsError::InvalidIdentity { source })
}

fn ensure_rustls_provider() {
    static INSTALL_PROVIDER: Once = Once::new();

    INSTALL_PROVIDER.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};
    use rcgen::{
        BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
        Issuer, KeyPair, KeyUsagePurpose, date_time_ymd,
    };
    use tempfile::TempDir;

    use super::*;

    struct GeneratedMaterial {
        ca_pem: Vec<u8>,
        client_cert_pem: Vec<u8>,
        client_key_pem: Vec<u8>,
        alternate_client_key_pem: Vec<u8>,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum ExpectedTlsError {
        Empty(TlsMaterialKind),
        Parse(TlsMaterialKind),
        InvalidIdentity,
        Expired(TlsMaterialKind),
        NotYetValid(TlsMaterialKind),
    }

    fn valid_material() -> GeneratedMaterial {
        material_with_validity(
            date_time_ymd(1975, 1, 1),
            date_time_ymd(4096, 1, 1),
            date_time_ymd(1975, 1, 1),
            date_time_ymd(4096, 1, 1),
        )
    }

    fn material_with_validity(
        ca_not_before: ::time::OffsetDateTime,
        ca_not_after: ::time::OffsetDateTime,
        client_not_before: ::time::OffsetDateTime,
        client_not_after: ::time::OffsetDateTime,
    ) -> GeneratedMaterial {
        let (ca, issuer) = ca_with_validity(ca_not_before, ca_not_after);
        let (client_cert, client_key) =
            client_cert_with_validity(&issuer, client_not_before, client_not_after);

        let alternate_key = KeyPair::generate().expect("alternate client key should generate");

        GeneratedMaterial {
            ca_pem: ca.pem().into_bytes(),
            client_cert_pem: client_cert.pem().into_bytes(),
            client_key_pem: client_key.serialize_pem().into_bytes(),
            alternate_client_key_pem: alternate_key.serialize_pem().into_bytes(),
        }
    }

    fn ca_with_validity(
        not_before: ::time::OffsetDateTime,
        not_after: ::time::OffsetDateTime,
    ) -> (Certificate, Issuer<'static, KeyPair>) {
        let mut params =
            CertificateParams::new(Vec::new()).expect("empty subject alt names should be valid");

        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "switch test ca");

        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        params.key_usages.push(KeyUsagePurpose::CrlSign);

        params.not_before = not_before;
        params.not_after = not_after;

        let key_pair = KeyPair::generate().expect("CA key should generate");
        let cert = params
            .self_signed(&key_pair)
            .expect("CA certificate should sign");

        (cert, Issuer::new(params, key_pair))
    }

    fn client_cert_with_validity(
        issuer: &Issuer<'static, KeyPair>,
        not_before: ::time::OffsetDateTime,
        not_after: ::time::OffsetDateTime,
    ) -> (Certificate, KeyPair) {
        let mut params =
            CertificateParams::new(Vec::new()).expect("empty subject alt names should be valid");

        params
            .distinguished_name
            .push(DnType::CommonName, "switch test client");

        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params
            .extended_key_usages
            .push(ExtendedKeyUsagePurpose::ClientAuth);

        params.not_before = not_before;
        params.not_after = not_after;

        let key_pair = KeyPair::generate().expect("client key should generate");
        let cert = params
            .signed_by(&key_pair, issuer)
            .expect("client certificate should sign");

        (cert, key_pair)
    }

    async fn write_material(
        dir: &TempDir,
        material: &GeneratedMaterial,
    ) -> Result<MtlsProfileConfig, std::io::Error> {
        let ca_cert_path = dir.path().join("ca.crt");
        let client_cert_path = dir.path().join("tls.crt");
        let client_key_path = dir.path().join("tls.key");

        tokio::fs::write(&ca_cert_path, &material.ca_pem).await?;
        tokio::fs::write(&client_cert_path, &material.client_cert_pem).await?;
        tokio::fs::write(&client_key_path, &material.client_key_pem).await?;

        Ok(MtlsProfileConfig {
            ca_cert_path,
            client_cert_path,
            client_key_path,
            tls_server_name: None,
        })
    }

    fn expected_tls_error(error: TlsError) -> ExpectedTlsError {
        match error {
            TlsError::Empty { kind, .. } => ExpectedTlsError::Empty(kind),
            TlsError::Parse { kind, .. } => ExpectedTlsError::Parse(kind),
            TlsError::InvalidIdentity { .. } => ExpectedTlsError::InvalidIdentity,
            TlsError::Expired { kind, .. } => ExpectedTlsError::Expired(kind),
            TlsError::NotYetValid { kind, .. } => ExpectedTlsError::NotYetValid(kind),
            error => panic!("unexpected mTLS profile error: {error}"),
        }
    }

    #[tokio::test]
    async fn valid_tls_material_builds_http_and_grpc_clients()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let material = valid_material();
        let config = write_material(&dir, &material).await?;

        preflight(&config).await?;
        let _http_client = http_client(&config).await?;

        let _grpc_tls = tonic_tls_config(&config).await?;

        Ok(())
    }

    #[tokio::test]
    async fn otlp_tls_material_builds_tls_and_mtls_grpc_clients()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let material = valid_material();
        let switch_config = write_material(&dir, &material).await?;

        let tls_config = OtlpTlsConfig {
            ca_cert_path: switch_config.ca_cert_path.clone(),
            client_cert_path: None,
            client_key_path: None,
            tls_server_name: Some("telemetry.example.com".to_string()),
            reload_interval: OtlpTlsConfig::DEFAULT_RELOAD_INTERVAL,
        };

        otlp_preflight(&tls_config).await?;
        let _tls = otlp_tonic_tls_config(&tls_config).await?;

        let mtls_config = OtlpTlsConfig {
            ca_cert_path: switch_config.ca_cert_path,
            client_cert_path: Some(switch_config.client_cert_path),
            client_key_path: Some(switch_config.client_key_path),
            tls_server_name: None,
            reload_interval: OtlpTlsConfig::DEFAULT_RELOAD_INTERVAL,
        };

        otlp_preflight(&mtls_config).await?;
        let _mtls = otlp_tonic_tls_config(&mtls_config).await?;

        Ok(())
    }

    #[tokio::test]
    async fn otlp_tls_config_detects_invalid_rotated_material()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let material = valid_material();
        let switch_config = write_material(&dir, &material).await?;

        let config = OtlpTlsConfig {
            ca_cert_path: switch_config.ca_cert_path,
            client_cert_path: Some(switch_config.client_cert_path),
            client_key_path: Some(switch_config.client_key_path),
            tls_server_name: None,
            reload_interval: OtlpTlsConfig::DEFAULT_RELOAD_INTERVAL,
        };

        otlp_preflight(&config).await?;
        let _tls = otlp_tonic_tls_config(&config).await?;

        let client_key_path = config
            .client_key_path
            .as_ref()
            .expect("test config must contain a client key path");

        tokio::fs::write(client_key_path, &material.alternate_client_key_pem).await?;

        let result = otlp_tonic_tls_config(&config).await;

        assert!(matches!(result, Err(TlsError::InvalidIdentity { .. })));

        Ok(())
    }

    #[tokio::test]
    async fn http_client_build_reads_changed_tls_material() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let material = valid_material();
        let config = write_material(&dir, &material).await?;

        preflight(&config).await?;

        let rotated = valid_material();
        tokio::fs::write(&config.ca_cert_path, &rotated.ca_pem).await?;
        tokio::fs::write(&config.client_cert_path, &rotated.client_cert_pem).await?;
        tokio::fs::write(&config.client_key_path, &rotated.client_key_pem).await?;

        let _http_client = http_client(&config).await?;

        Ok(())
    }

    #[tokio::test]
    async fn http_client_build_reports_invalid_changed_tls_material()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let material = valid_material();
        let config = write_material(&dir, &material).await?;

        preflight(&config).await?;

        tokio::fs::write(&config.client_key_path, &material.alternate_client_key_pem).await?;

        let result = http_client(&config).await;

        assert!(matches!(result, Err(TlsError::InvalidIdentity { .. })));

        Ok(())
    }

    #[tokio::test]
    async fn http_client_provider_reuses_cached_client_until_reload_interval()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let material = valid_material();
        let config = write_material(&dir, &material).await?;
        let provider = MtlsHttpClientProvider::new(config.clone(), Duration::from_secs(3600));

        let _http_client = provider.client().await?;

        tokio::fs::write(&config.client_key_path, &material.alternate_client_key_pem).await?;

        let _cached_http_client = provider.client().await?;

        Ok(())
    }

    #[tokio::test]
    async fn http_client_provider_reloads_after_reload_interval()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let material = valid_material();
        let config = write_material(&dir, &material).await?;
        let provider = MtlsHttpClientProvider::new(config.clone(), Duration::ZERO);

        let _http_client = provider.client().await?;

        tokio::fs::write(&config.client_key_path, &material.alternate_client_key_pem).await?;

        let result = provider.client().await;

        assert!(matches!(result, Err(TlsError::InvalidIdentity { .. })));

        Ok(())
    }

    #[tokio::test]
    async fn missing_tls_material_returns_path_and_role() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = TempDir::new()?;
        let material = valid_material();
        let config = write_material(&dir, &material).await?;

        tokio::fs::remove_file(&config.client_key_path).await?;

        let result = preflight(&config).await;

        let Err(TlsError::Read { kind, path, .. }) = result else {
            panic!("expected read error for missing key");
        };

        assert_eq!(kind, TlsMaterialKind::ClientKey);
        assert_eq!(path, config.client_key_path);

        Ok(())
    }

    #[tokio::test]
    async fn invalid_tls_material_returns_clear_errors() -> Result<(), Box<dyn std::error::Error>> {
        check_cases_async(
            [
                Case {
                    scenario: "empty CA bundle",
                    input: {
                        let mut material = valid_material();
                        material.ca_pem = Vec::new();
                        material
                    },
                    expect: FailsWith(ExpectedTlsError::Empty(TlsMaterialKind::CaBundle)),
                },
                Case {
                    scenario: "malformed client certificate",
                    input: {
                        let mut material = valid_material();
                        material.client_cert_pem = b"not pem".to_vec();
                        material
                    },
                    expect: FailsWith(ExpectedTlsError::Parse(TlsMaterialKind::ClientCertificate)),
                },
                Case {
                    scenario: "malformed client key",
                    input: {
                        let mut material = valid_material();
                        material.client_key_pem = b"not pem".to_vec();
                        material
                    },
                    expect: FailsWith(ExpectedTlsError::Parse(TlsMaterialKind::ClientKey)),
                },
                Case {
                    scenario: "mismatched client key",
                    input: {
                        let mut material = valid_material();
                        material.client_key_pem = material.alternate_client_key_pem.clone();
                        material
                    },
                    expect: FailsWith(ExpectedTlsError::InvalidIdentity),
                },
            ],
            |material| async move {
                let dir = TempDir::new().expect("temp dir should create");

                let config = write_material(&dir, &material)
                    .await
                    .expect("mTLS profile material should write");

                preflight(&config).await.map_err(expected_tls_error)
            },
        )
        .await;

        Ok(())
    }

    #[tokio::test]
    async fn tls_material_rejects_expired_and_future_certificates()
    -> Result<(), Box<dyn std::error::Error>> {
        check_cases_async(
            [
                Case {
                    scenario: "expired client certificate",
                    input: material_with_validity(
                        date_time_ymd(1975, 1, 1),
                        date_time_ymd(4096, 1, 1),
                        date_time_ymd(2020, 1, 1),
                        date_time_ymd(2021, 1, 1),
                    ),
                    expect: FailsWith(ExpectedTlsError::Expired(
                        TlsMaterialKind::ClientCertificate,
                    )),
                },
                Case {
                    scenario: "future CA certificate",
                    input: material_with_validity(
                        date_time_ymd(4097, 1, 1),
                        date_time_ymd(4098, 1, 1),
                        date_time_ymd(1975, 1, 1),
                        date_time_ymd(4096, 1, 1),
                    ),
                    expect: FailsWith(ExpectedTlsError::NotYetValid(TlsMaterialKind::CaBundle)),
                },
            ],
            |material| async move {
                let dir = TempDir::new().expect("temp dir should create");

                let config = write_material(&dir, &material)
                    .await
                    .expect("mTLS profile material should write");

                preflight(&config).await.map_err(expected_tls_error)
            },
        )
        .await;

        Ok(())
    }
}
