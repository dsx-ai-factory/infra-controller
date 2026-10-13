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

use std::path::PathBuf;

use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

/// `ResourcePoolBackendConfig` defines a named allocator available to pools.
/// Defining a gRPC backend does not connect to it or activate remote allocation.
/// Definitions may remain unused, regardless of their backend type.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResourcePoolBackendConfig {
    /// Allocate through the local database in the caller's transaction.
    Integrated,
    /// Configuration for a remote provider. v2.4 rejects pools selecting this
    /// backend before provider I/O or pool writes.
    Grpc {
        /// HTTP or HTTPS origin with a host, no credentials, query, fragment,
        /// or path other than `/`.
        endpoint: Url,
        /// Optional certificate paths. Any configured certificate or key
        /// requires HTTPS; client certificate and key must be set together.
        tls: Option<ResourcePoolTlsConfig>,
    },
}

/// `BackendKind` reads the `type` field of a named backend definition.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum BackendKind {
    Integrated,
    Grpc,
}

/// `BackendInput` reads backend fields before checking which ones its type
/// accepts. Reading TLS fields directly preserves the nested paths the
/// configuration loader needs to report unknown fields.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackendInput {
    #[serde(rename = "type")]
    kind: BackendKind,
    // `Url`'s deserializer echoes invalid input, including credentials.
    endpoint: Option<String>,
    tls: Option<ResourcePoolTlsConfig>,
}

impl<'de> Deserialize<'de> for ResourcePoolBackendConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let input = BackendInput::deserialize(deserializer)?;
        match input.kind {
            BackendKind::Integrated => {
                if input.endpoint.is_some() || input.tls.is_some() {
                    return Err(D::Error::custom(
                        "integrated backend does not accept endpoint or tls",
                    ));
                }
                Ok(Self::Integrated)
            }
            BackendKind::Grpc => {
                let endpoint = input
                    .endpoint
                    .ok_or_else(|| D::Error::missing_field("endpoint"))?;
                let endpoint = Url::parse(&endpoint)
                    .map_err(|error| D::Error::custom(format!("invalid endpoint URL: {error}")))?;
                Ok(Self::Grpc {
                    endpoint,
                    tls: input.tls,
                })
            }
        }
    }
}

/// `ResourcePoolTlsConfig` configures TLS trust and client identity for a
/// resource pool backend.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourcePoolTlsConfig {
    /// Optional PEM CA certificate file for validating the provider.
    #[serde(default)]
    pub ca_cert_file: Option<PathBuf>,
    /// Optional PEM client certificate file, paired with `client_key_file`.
    #[serde(default)]
    pub client_cert_file: Option<PathBuf>,
    /// Optional PEM client private key file, paired with `client_cert_file`.
    #[serde(default)]
    pub client_key_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum ResourcePoolBackendConfigError {
    #[error("endpoint must use http or https and include a host")]
    InvalidEndpoint,
    #[error("endpoint must not contain credentials, a query, or a fragment")]
    EndpointContainsCredentialsOrParameters,
    #[error("endpoint path must be empty or /")]
    EndpointPath,
    #[error("client_cert_file and client_key_file must be configured together")]
    IncompleteClientIdentity,
    #[error("TLS certificate and key files require an https endpoint")]
    TlsRequiresHttps,
}

impl ResourcePoolBackendConfig {
    pub(super) fn validate(&self) -> Result<(), ResourcePoolBackendConfigError> {
        let Self::Grpc { endpoint, tls } = self else {
            return Ok(());
        };

        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host().is_none() {
            return Err(ResourcePoolBackendConfigError::InvalidEndpoint);
        }
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ResourcePoolBackendConfigError::EndpointContainsCredentialsOrParameters);
        }
        if !matches!(endpoint.path(), "" | "/") {
            return Err(ResourcePoolBackendConfigError::EndpointPath);
        }

        let Some(tls) = tls else {
            return Ok(());
        };
        if tls.client_cert_file.is_some() != tls.client_key_file.is_some() {
            return Err(ResourcePoolBackendConfigError::IncompleteClientIdentity);
        }
        if endpoint.scheme() != "https"
            && (tls.ca_cert_file.is_some() || tls.client_cert_file.is_some())
        {
            return Err(ResourcePoolBackendConfigError::TlsRequiresHttps);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn backend_config_parser() {
        scenarios!(
            run = |input| toml::from_str::<ResourcePoolBackendConfig>(input).map_err(drop);
            "backend definitions" {
                "type = \"integrated\"" => Yields(ResourcePoolBackendConfig::Integrated),
                "type = \"grpc\"\nendpoint = \"https://allocator.example\"" => Yields(ResourcePoolBackendConfig::Grpc {
                    endpoint: "https://allocator.example".parse().unwrap(),
                    tls: None,
                }),
            }
            "invalid backend definitions" {
                "type = \"other\"" => Fails,
                "type = \"grpc\"" => Fails,
                "type = \"grpc\"\nendpoint = \"not a URL\"" => Fails,
                "type = \"integrated\"\nendpoint = \"https://allocator.example\"" => Fails,
            }
        );
    }

    #[test]
    fn backend_endpoint_and_tls_validation() {
        scenarios!(
            run = |(endpoint, tls): (&str, Option<ResourcePoolTlsConfig>)| ResourcePoolBackendConfig::Grpc {
                endpoint: endpoint.parse().expect("test endpoint URL"),
                tls,
            }.validate();
            "unused definitions are valid without network or certificate reads" {
                ("http://allocator.example:50051", None) => Yields(()),
                ("http://allocator.example:50051", Some(ResourcePoolTlsConfig::default())) => Yields(()),
                ("https://allocator.example/", Some(ResourcePoolTlsConfig {
                    ca_cert_file: Some("absent-ca.pem".into()),
                    client_cert_file: Some("absent-cert.pem".into()),
                    client_key_file: Some("absent-key.pem".into()),
                })) => Yields(()),
            }
            "endpoint restrictions" {
                ("file:///allocator", None) => FailsWith(ResourcePoolBackendConfigError::InvalidEndpoint),
                ("https://user:secret@allocator.example", None) => FailsWith(ResourcePoolBackendConfigError::EndpointContainsCredentialsOrParameters),
                ("https://allocator.example?token=secret", None) => FailsWith(ResourcePoolBackendConfigError::EndpointContainsCredentialsOrParameters),
                ("https://allocator.example#secret", None) => FailsWith(ResourcePoolBackendConfigError::EndpointContainsCredentialsOrParameters),
                ("https://allocator.example/pool", None) => FailsWith(ResourcePoolBackendConfigError::EndpointPath),
            }
            "TLS fields must be usable together" {
                ("https://allocator.example", Some(ResourcePoolTlsConfig {
                    client_cert_file: Some("client.pem".into()),
                    ..Default::default()
                })) => FailsWith(ResourcePoolBackendConfigError::IncompleteClientIdentity),
                ("https://allocator.example", Some(ResourcePoolTlsConfig {
                    client_key_file: Some("client.key".into()),
                    ..Default::default()
                })) => FailsWith(ResourcePoolBackendConfigError::IncompleteClientIdentity),
                ("http://allocator.example", Some(ResourcePoolTlsConfig {
                    ca_cert_file: Some("ca.pem".into()),
                    ..Default::default()
                })) => FailsWith(ResourcePoolBackendConfigError::TlsRequiresHttps),
            }
        );
    }
}
