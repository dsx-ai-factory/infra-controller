// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// `ResourcePoolTlsConfig` configures TLS trust and client identity for a
/// resource pool backend. Files are PEM encoded and at most 256 KiB each.
///
/// Each call reads the files before reusing a connection. Changing any file's bytes
/// causes the next call to connect with the new material; calls already in progress
/// can finish on their existing connection. A rotation between the certificate and
/// key reads can fail that call; the next call reads both files again. Invalid
/// replacements fail visibly until the files are corrected, without falling
/// back to old credentials.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePoolTlsConfig {
    /// Optional CA bundle used to validate the provider. When supplied, only
    /// this bundle is trusted. When omitted, TLS uses the host's native root store;
    /// connection setup can fail if that store has no usable root certificates.
    #[serde(default)]
    pub ca_cert_file: Option<PathBuf>,
    /// Optional PEM client certificate chain, paired with `client_key_file`.
    #[serde(default)]
    pub client_cert_file: Option<PathBuf>,
    /// Optional PEM client private key, paired with `client_cert_file`.
    #[serde(default)]
    pub client_key_file: Option<PathBuf>,
}

/// `GrpcResourcePoolConfigError` identifies settings that cannot be used together.
/// Messages omit the endpoint and paths because those may contain credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum GrpcResourcePoolConfigError {
    /// The endpoint needs an HTTP or HTTPS scheme and a host.
    #[error("endpoint must use http or https and include a host")]
    InvalidEndpoint,
    /// Authentication and URL parameters do not belong in the endpoint.
    #[error("endpoint must not contain credentials, a query, or a fragment")]
    EndpointContainsCredentialsOrParameters,
    /// The gRPC client supplies the method path for each call.
    #[error("endpoint path must be empty or /")]
    EndpointPath,
    /// A client certificate and private key must be supplied together.
    #[error("client_cert_file and client_key_file must be configured together")]
    IncompleteClientIdentity,
    /// Certificate settings require a TLS connection.
    #[error("TLS certificate and key files require an https endpoint")]
    TlsRequiresHttps,
}

/// `validate_endpoint` checks the URL and TLS field combinations without network
/// or file access. Startup validation and direct client construction share these
/// rules, including for a backend that no pool selects.
pub fn validate_endpoint(
    endpoint: &Url,
    tls: Option<&ResourcePoolTlsConfig>,
) -> Result<(), GrpcResourcePoolConfigError> {
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host().is_none() {
        return Err(GrpcResourcePoolConfigError::InvalidEndpoint);
    }
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(GrpcResourcePoolConfigError::EndpointContainsCredentialsOrParameters);
    }
    if !matches!(endpoint.path(), "" | "/") {
        return Err(GrpcResourcePoolConfigError::EndpointPath);
    }
    if let Some(tls) = tls {
        if tls.client_cert_file.is_some() != tls.client_key_file.is_some() {
            return Err(GrpcResourcePoolConfigError::IncompleteClientIdentity);
        }
        if endpoint.scheme() != "https"
            && (tls.ca_cert_file.is_some() || tls.client_cert_file.is_some())
        {
            return Err(GrpcResourcePoolConfigError::TlsRequiresHttps);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn endpoint_and_tls_validation() {
        scenarios!(
            run = |(endpoint, tls): (&str, Option<ResourcePoolTlsConfig>)| validate_endpoint(
                &endpoint.parse().expect("test endpoint URL"),
                tls.as_ref(),
            );
            "valid settings require no network or certificate reads" {
                ("http://allocator.example:50051", None) => Yields(()),
                ("http://allocator.example:50051", Some(ResourcePoolTlsConfig::default())) => Yields(()),
                ("https://allocator.example/", Some(ResourcePoolTlsConfig {
                    ca_cert_file: Some("absent-ca.pem".into()),
                    client_cert_file: Some("absent-cert.pem".into()),
                    client_key_file: Some("absent-key.pem".into()),
                })) => Yields(()),
            }
            "endpoint restrictions" {
                ("file:///allocator", None) => FailsWith(GrpcResourcePoolConfigError::InvalidEndpoint),
                ("https://user:secret@allocator.example", None) => FailsWith(GrpcResourcePoolConfigError::EndpointContainsCredentialsOrParameters),
                ("https://allocator.example?token=secret", None) => FailsWith(GrpcResourcePoolConfigError::EndpointContainsCredentialsOrParameters),
                ("https://allocator.example#secret", None) => FailsWith(GrpcResourcePoolConfigError::EndpointContainsCredentialsOrParameters),
                ("https://allocator.example/pool", None) => FailsWith(GrpcResourcePoolConfigError::EndpointPath),
            }
            "TLS fields must be usable together" {
                ("https://allocator.example", Some(ResourcePoolTlsConfig {
                    client_cert_file: Some("client.pem".into()),
                    ..Default::default()
                })) => FailsWith(GrpcResourcePoolConfigError::IncompleteClientIdentity),
                ("https://allocator.example", Some(ResourcePoolTlsConfig {
                    client_key_file: Some("client.key".into()),
                    ..Default::default()
                })) => FailsWith(GrpcResourcePoolConfigError::IncompleteClientIdentity),
                ("http://allocator.example", Some(ResourcePoolTlsConfig {
                    ca_cert_file: Some("ca.pem".into()),
                    ..Default::default()
                })) => FailsWith(GrpcResourcePoolConfigError::TlsRequiresHttps),
            }
        );
    }
}
