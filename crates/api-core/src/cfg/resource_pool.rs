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

pub use carbide_resource_pool::grpc::ResourcePoolTlsConfig;
use carbide_resource_pool::grpc::{GrpcResourcePoolConfigError, validate_endpoint};
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

impl ResourcePoolBackendConfig {
    pub(super) fn validate(&self) -> Result<(), GrpcResourcePoolConfigError> {
        match self {
            Self::Integrated => Ok(()),
            Self::Grpc { endpoint, tls } => validate_endpoint(endpoint, tls.as_ref()),
        }
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
    fn backend_validation_delegates_grpc_settings() {
        let backend = ResourcePoolBackendConfig::Grpc {
            endpoint: "http://allocator.example"
                .parse()
                .expect("test endpoint URL"),
            tls: Some(ResourcePoolTlsConfig {
                ca_cert_file: Some("ca.pem".into()),
                ..Default::default()
            }),
        };
        assert_eq!(
            backend.validate(),
            Err(GrpcResourcePoolConfigError::TlsRequiresHttps)
        );
    }
}
