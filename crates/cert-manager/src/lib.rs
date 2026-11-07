// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Issues machine and UFM certificates without putting their private keys in Kubernetes.
//! The signing issuer must chain to the site's existing trust bundle.

mod crypto;
mod resources;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use carbide_secrets::certificates::{Certificate, CertificateProvider};
use carbide_secrets::{SecretsError, SpiffeIdentity};
use forge_tls::client_config::MAX_CERT_RENEWAL_TIME_SECS;
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Client};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::crypto::CertificateRequestMaterial;
use crate::resources::{CertificateRequest, IssuerRef, RequestSpec, RequestStatus, api_resource};

const MACHINE_ID_LABEL: &str = "nico.nvidia.com/machine-id";
const FABRIC_LABEL: &str = "nico.nvidia.com/fabric";
const POLL_INTERVAL: Duration = Duration::from_secs(1);
// Leave a day after the latest scheduled renewal for loop delays and retries.
const CERT_RENEWAL_GRACE_PERIOD_SECS: u64 = 24 * 60 * 60;
const MIN_CERTIFICATE_TTL: Duration =
    Duration::from_secs(MAX_CERT_RENEWAL_TIME_SECS + CERT_RENEWAL_GRACE_PERIOD_SECS);

/// Supported cert-manager issuer scopes, serialized with Kubernetes spelling.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub enum IssuerKind {
    /// An issuer in the same namespace as the CertificateRequest.
    Issuer,
    /// A cluster-scoped issuer; its CA Secret is managed by cert-manager.
    #[default]
    ClusterIssuer,
}

/// Settings for the certificate signer, defaulting to the site issuer in forge-system.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Namespace in which NICo creates CertificateRequests; defaults to forge-system.
    /// Must match its request RBAC.
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// Existing issuer backed by the site's compatible signing CA; defaults to site-issuer.
    #[serde(default = "default_issuer_name")]
    pub issuer_name: String,
    /// Issuer scope, defaulting to ClusterIssuer.
    #[serde(default)]
    pub issuer_kind: IssuerKind,
    /// Issuance deadline in seconds, including request creation; defaults to 120, range 1..=600.
    /// Cleanup has a separate wait bounded by the same duration.
    #[serde(default = "default_request_timeout_secs")]
    pub request_timeout_secs: u64,
    /// Maximum certificate lifetime; defaults to 720h, matching the bootstrap Vault role.
    /// Must be a whole-second duration of at least the maximum agent renewal interval
    /// plus one day of grace for polling delays and retries (8d with the current interval).
    /// Set to the existing Vault role limit when migrating.
    #[serde(default = "default_max_ttl", with = "humantime_serde")]
    pub max_ttl: Duration,
}

fn default_namespace() -> String {
    "forge-system".into()
}

fn default_issuer_name() -> String {
    "site-issuer".into()
}

fn default_request_timeout_secs() -> u64 {
    120
}

fn default_max_ttl() -> Duration {
    Duration::from_secs(720 * 3600)
}

impl Default for Config {
    fn default() -> Self {
        Self {
            namespace: default_namespace(),
            issuer_name: default_issuer_name(),
            issuer_kind: IssuerKind::default(),
            request_timeout_secs: default_request_timeout_secs(),
            max_ttl: default_max_ttl(),
        }
    }
}

impl Config {
    /// Reject empty namespace/issuer, deadlines outside 1..=600 seconds, and
    /// lifetimes that are fractional seconds or leave less than a day after the
    /// maximum agent renewal interval.
    pub fn validate(&self) -> Result<(), Error> {
        if self.namespace.trim().is_empty() || self.issuer_name.trim().is_empty() {
            return Err(Error::Configuration(
                "namespace and issuer_name must be non-empty".into(),
            ));
        }
        if !(1..=600).contains(&self.request_timeout_secs) {
            return Err(Error::Configuration(
                "request_timeout_secs must be in 1..=600".into(),
            ));
        }
        if self.max_ttl < MIN_CERTIFICATE_TTL || self.max_ttl.subsec_nanos() != 0 {
            return Err(Error::Configuration(format!(
                "max_ttl must be a whole-second duration of at least {}",
                humantime::format_duration(MIN_CERTIFICATE_TTL),
            )));
        }
        Ok(())
    }
}

/// Certificate issuance or validation failure, retaining Kubernetes and cryptographic sources.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid provider configuration.
    #[error("invalid cert-manager configuration: {0}")]
    Configuration(String),
    /// Invalid certificate request identifier, DNS SAN, or TTL.
    #[error("invalid cert-manager request argument: {0}")]
    InvalidArgument(String),
    /// Kubernetes request failure.
    #[error("cert-manager kubernetes request failed: {0}")]
    Kubernetes(#[from] kube::Error),
    /// Kubernetes client configuration could not be inferred.
    #[error("could not configure kubernetes client: {0}")]
    ClientConfiguration(#[from] kube::config::InferConfigError),
    /// Key generation or CSR encoding failed.
    #[error("certificate CSR failed: {0}")]
    Crypto(#[from] rcgen::Error),
    /// The request was denied, invalid, or terminally failed.
    #[error("cert-manager request rejected: {reason}: {message}")]
    Rejected {
        /// cert-manager's reason for rejecting the request.
        reason: String,
        /// Diagnostic message from the request condition.
        message: String,
    },
    /// Certificate issuance did not finish before the deadline.
    #[error("cert-manager issuance timed out")]
    Timeout,
    /// Issued material failed the local key, identity, usage, lifetime, or trust checks.
    #[error("invalid issued certificate: {0}")]
    Certificate(String),
    /// The configured validation bundle could not be read.
    #[error("could not read CA bundle: {0}")]
    TrustBundle(#[from] std::io::Error),
}

/// CertificateProvider with P-256 keys generated in process memory.
///
/// Calls submit a CSR, wait for issuance, validate against the local key and trusted CA,
/// and delete the request on success, failure, or timeout. Cancellation or process death
/// may leave a public CSR behind for operator cleanup.
/// UFM DNS SANs are included in the CSR. Explicit TTLs are capped by `Config::max_ttl`;
/// absent TTLs retain the randomized machine lifetime, subject to the same cap.
/// Requested and issued certificates must remain valid for at least the maximum
/// agent renewal interval plus one day of grace for polling delays and retries.
pub struct CertManagerCertificateProvider {
    requests: Api<CertificateRequest>,
    config: Config,
    spiffe: SpiffeIdentity,
    trust_bundle_path: PathBuf,
}

impl CertManagerCertificateProvider {
    /// Build with Kubernetes's configured client and validate the site's CA bundle at startup.
    pub async fn from_config(
        config: Config,
        spiffe: SpiffeIdentity,
        trust_bundle_path: PathBuf,
    ) -> Result<Self, Error> {
        config.validate()?;
        crypto::verifier(&tokio::fs::read(&trust_bundle_path).await?)?;
        let client = Client::try_default().await?;
        Self::new(client, config, spiffe, trust_bundle_path)
    }

    fn new(
        client: Client,
        config: Config,
        spiffe: SpiffeIdentity,
        trust_bundle_path: PathBuf,
    ) -> Result<Self, Error> {
        config.validate()?;
        let requests = Api::namespaced_with(client, &config.namespace, &api_resource());
        Ok(Self {
            requests,
            config,
            spiffe,
            trust_bundle_path,
        })
    }

    async fn delete_request(&self, name: &str, uid: Option<String>) -> Result<(), Error> {
        let params = DeleteParams {
            preconditions: uid.map(|uid| kube::api::Preconditions {
                uid: Some(uid),
                resource_version: None,
            }),
            ..Default::default()
        };
        match self.requests.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(kube::Error::Api(error)) if error.code == 404 => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn issue(
        &self,
        identifier: &str,
        alt_names: Option<&str>,
        ttl: Option<&str>,
    ) -> Result<Certificate, Error> {
        let request = CertificateRequestMaterial::new(
            &self.spiffe,
            identifier,
            alt_names,
            ttl,
            self.config.max_ttl,
        )?;
        let (prefix, label) = if alt_names.is_some() {
            ("nico-ufm", FABRIC_LABEL)
        } else {
            ("nico-machine", MACHINE_ID_LABEL)
        };
        let name = format!("{prefix}-{}", Uuid::new_v4());
        let mut resource = CertificateRequest::new(
            &name,
            &api_resource(),
            RequestSpec {
                request: k8s_openapi::ByteString(request.csr_pem()?),
                issuer_ref: IssuerRef {
                    name: self.config.issuer_name.clone(),
                    kind: self.config.issuer_kind,
                    group: "cert-manager.io".into(),
                },
                duration: format!("{}s", request.lifetime.as_secs()),
                is_ca: false,
                usages: vec![
                    "digital signature".into(),
                    "key agreement".into(),
                    "key encipherment".into(),
                    "client auth".into(),
                    "server auth".into(),
                ],
            },
        );
        resource.metadata.labels = Some(BTreeMap::from([(label.into(), identifier.into())]));
        let mut uid = None;
        let result = tokio::time::timeout(
            Duration::from_secs(self.config.request_timeout_secs),
            async {
                let created = self
                    .requests
                    .create(&PostParams::default(), &resource)
                    .await?;
                uid = created.metadata.uid;
                loop {
                    let resource = self.requests.get(&name).await?;
                    if let Some(status) = resource.status.as_ref() {
                        if let Some(rejected) = status.conditions.iter().find(|condition| {
                            (condition.status == "True"
                                && matches!(condition.type_.as_str(), "Denied" | "InvalidRequest"))
                                || (condition.type_ == "Ready"
                                    && condition.status == "False"
                                    && condition.reason == "Failed")
                        }) {
                            return Err(Error::Rejected {
                                reason: rejected.reason.clone(),
                                message: rejected.message.clone(),
                            });
                        }
                        if status.conditions.iter().any(|condition| {
                            condition.type_ == "Ready" && condition.status == "True"
                        }) {
                            return self.validate(&request, status).await;
                        }
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            },
        )
        .await
        .unwrap_or(Err(Error::Timeout));
        // The name is known before creation. Even an interrupted POST may have
        // created the CSR, so attempt deletion after every creation outcome.
        // DELETE acknowledges the deletion request; do not wait for finalizers.
        // Cleanup must still be polled after the issuance deadline expires.
        match tokio::time::timeout(
            Duration::from_secs(self.config.request_timeout_secs),
            self.delete_request(&name, uid),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(request_name = %name, %error, "Could not delete CertificateRequest");
            }
            Err(_) => {
                tracing::warn!(request_name = %name, "Timed out deleting CertificateRequest");
            }
        }
        result
    }

    async fn validate(
        &self,
        request: &CertificateRequestMaterial,
        status: &RequestStatus,
    ) -> Result<Certificate, Error> {
        // Re-read the configured trust bundle so CA updates take effect without a restart.
        let trusted_ca = tokio::fs::read(&self.trust_bundle_path).await?;
        request.validate(status, &trusted_ca)
    }
}

#[async_trait]
impl CertificateProvider for CertManagerCertificateProvider {
    async fn get_certificate(
        &self,
        unique_identifier: &str,
        alt_names: Option<String>,
        ttl: Option<String>,
    ) -> Result<Certificate, SecretsError> {
        self.issue(unique_identifier, alt_names.as_deref(), ttl.as_deref())
            .await
            .map_err(|error| SecretsError::GenericError(eyre::Report::new(error)))
    }
}

#[cfg(test)]
mod tests;
