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

//! The credentials the proxy applies for a BMC, fetched from nico-api and
//! cached by the BMC's IP.

use std::net::IpAddr;
use std::time::Duration;

use carbide_utils::redfish::redfish_basic_authorization_context;
use moka::future::Cache as MokaCache;
use rpc::forge;
use rpc::forge_api_client::ForgeApiClient;

use crate::proxy::BmcProxyError;

/// Redfish's session token header, per DMTF. Applied on egress and stripped
/// on ingress by `copy_request_headers` in [`upstream`](super::upstream).
pub(super) const REDFISH_AUTH_TOKEN_HEADER: &str = "X-Auth-Token";

/// Cached BMC credentials by IP. Correctness comes from the 401/403 eviction
/// in [`proxy_request_inner`](super::proxy_request_inner). Expiry is idle-based, not lifetime-based: an entry
/// a caller keeps using stays served even through a long nico-api outage
/// (the availability the pre-cache-bound proxy provided), while an entry for
/// a machine that stopped existing falls out once nothing asks for it. The
/// capacity bounds memory.
pub(super) type CredentialCache = MokaCache<IpAddr, BmcCredentials>;

/// How long unused cached credentials linger before falling out.
pub(super) const CREDENTIAL_CACHE_IDLE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, PartialEq, Eq)]
pub(super) enum BmcCredentials {
    UsernamePassword { username: String, password: String },
    SessionToken { token: String },
}

impl BmcCredentials {
    /// Applies this credential and returns every secret representation sent on
    /// the wire so the matching response can be sanitized with exact context.
    pub(super) fn apply_to_request(
        self,
        request: reqwest_middleware::RequestBuilder,
    ) -> Result<(reqwest_middleware::RequestBuilder, Vec<String>), http::header::InvalidHeaderValue>
    {
        match self {
            Self::UsernamePassword { username, password } => {
                // Generate the header once so the transmitted and retained
                // representations cannot drift apart.
                let (authorization, sensitive_values) =
                    redfish_basic_authorization_context(&username, Some(&password));
                let mut header = http::HeaderValue::from_str(&authorization)?;
                header.set_sensitive(true);

                // The shared context also covers the bare Base64 payload if a
                // peer normalizes or omits the authentication scheme.
                Ok((
                    request.header(http::header::AUTHORIZATION, header),
                    sensitive_values,
                ))
            }
            Self::SessionToken { token } => {
                // Keep the exact token header value with the request attempt.
                let mut header = http::HeaderValue::from_str(&token)?;
                header.set_sensitive(true);
                let sensitive_values = if token.is_empty() {
                    Vec::new()
                } else {
                    vec![token]
                };
                Ok((
                    request.header(REDFISH_AUTH_TOKEN_HEADER, header),
                    sensitive_values,
                ))
            }
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

pub(super) async fn get_bmc_credentials(
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

pub(super) async fn evict_cached_credentials(ip: IpAddr, credential_cache: &CredentialCache) {
    if credential_cache.remove(&ip).await.is_some() {
        tracing::info!(bmc_ip_address = %ip, "Evicted cached BMC credentials after upstream auth failure");
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Check, check_values, scenarios};
    use rpc::forge;

    use super::{
        BmcCredentials, CREDENTIAL_CACHE_IDLE_TTL, CredentialCache, evict_cached_credentials,
    };
    use crate::proxy::idle_bounded_cache;
    use crate::proxy::test_support::*;

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

    /// Verifies every credential variant retains the exact reusable values put
    /// on the wire while leaving a non-secret username out of the redaction set.
    #[test]
    fn bmc_credentials_retain_wire_secrets_for_response_redaction() {
        check_values(
            [
                // Basic auth retains plaintext, payload, and complete-header forms.
                Check {
                    scenario: "username and password",
                    input: BmcCredentials::UsernamePassword {
                        username: "admin".to_string(),
                        password: "secret".to_string(),
                    },
                    expect: vec![
                        "secret".to_string(),
                        "YWRtaW46c2VjcmV0".to_string(),
                        "Basic YWRtaW46c2VjcmV0".to_string(),
                    ],
                },
                // A session token is already the exact value sent on the wire.
                Check {
                    scenario: "session token",
                    input: BmcCredentials::SessionToken {
                        token: "token-123".to_string(),
                    },
                    expect: vec!["token-123".to_string()],
                },
                // Empty passwords still produce reusable payload and header forms.
                Check {
                    scenario: "empty password",
                    input: BmcCredentials::UsernamePassword {
                        username: "admin".to_string(),
                        password: String::new(),
                    },
                    expect: vec!["YWRtaW46".to_string(), "Basic YWRtaW46".to_string()],
                },
                // An empty token sends no credential material worth retaining.
                Check {
                    scenario: "empty session token",
                    input: BmcCredentials::SessionToken {
                        token: String::new(),
                    },
                    expect: Vec::new(),
                },
            ],
            |credentials| {
                // Apply credentials to a real builder so the returned context
                // is exercised at the same boundary used by production requests.
                let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
                let request = client.get("https://example.com/redfish/v1");
                let (_, sensitive_values) = credentials
                    .apply_to_request(request)
                    .expect("credentials should apply");
                sensitive_values
            },
        );
    }

    #[test]
    fn bmc_username_password_credentials_use_basic_auth() {
        // Apply Basic credentials through the production request boundary.
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
        let request = client.get("https://example.com/redfish/v1");
        let (request, sensitive_values) = BmcCredentials::UsernamePassword {
            username: "admin".to_string(),
            password: "secret".to_string(),
        }
        .apply_to_request(request)
        .expect("credentials should apply");
        let request = request.build().expect("request should build");

        // The emitted header and retained value must be byte-for-byte identical.
        let auth = request
            .headers()
            .get(http::header::AUTHORIZATION)
            .expect("authorization header should be present");
        assert_eq!(auth, "Basic YWRtaW46c2VjcmV0");
        assert_eq!(sensitive_values[1], "YWRtaW46c2VjcmV0");
        assert_eq!(sensitive_values[2], auth.to_str().unwrap());
    }

    #[test]
    fn bmc_session_token_credentials_use_redfish_token_header() {
        // Apply a session token through the production request boundary.
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
        let request = client.get("https://example.com/redfish/v1");
        let (request, sensitive_values) = BmcCredentials::SessionToken {
            token: "token-123".to_string(),
        }
        .apply_to_request(request)
        .expect("credentials should apply");
        let request = request.build().expect("request should build");

        // The emitted token and retained redaction context must stay identical.
        assert_eq!(request.headers().get("X-Auth-Token").unwrap(), "token-123");
        assert_eq!(sensitive_values, ["token-123"]);
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
}
