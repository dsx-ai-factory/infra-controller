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

//! The credentials the proxy applies for a proxied address, fetched from
//! nico-api and cached by the address: a BMC's, or the NVOS admin's for a
//! switch's NVOS management interface.

use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use carbide_instrument::emit;
use carbide_utils::redfish::redfish_basic_authorization_context;
use carbide_uuid::switch::SwitchId;
use mac_address::MacAddress;
use moka::future::Cache as MokaCache;
use rpc::forge;
use rpc::forge_api_client::ForgeApiClient;

use crate::metrics::SwitchProbeDenied;
use crate::proxy::BmcProxyError;

/// Redfish's session token header, per DMTF. Applied on egress and stripped
/// on ingress by `copy_request_headers` in [`upstream`](super::upstream).
pub(super) const REDFISH_AUTH_TOKEN_HEADER: &str = "X-Auth-Token";

/// Cached credentials by IP. Correctness comes from the 401/403 eviction
/// in [`proxy_request_inner`](super::proxy_request_inner). Expiry is idle-based, not lifetime-based: an entry
/// a caller keeps using stays served even through a long nico-api outage
/// (the availability the pre-cache-bound proxy provided), while an entry for
/// a machine that stopped existing falls out once nothing asks for it. The
/// capacity bounds memory.
pub(super) type CredentialCache = MokaCache<IpAddr, TargetCredentials>;

/// How long unused cached credentials linger before falling out.
pub(super) const CREDENTIAL_CACHE_IDLE_TTL: Duration = Duration::from_secs(60 * 60);

/// What a proxied address is, which decides where its credentials come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TargetKind {
    /// The Redfish BMC of a machine, switch, or power shelf.
    Bmc,
    /// A switch's NVOS management interface, serving its NVUE REST API.
    SwitchNvos,
}

impl TargetKind {
    /// The kind's name on the request's trace span, as `bmc_proxy.target`.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Bmc => "bmc",
            Self::SwitchNvos => "switch_nvos",
        }
    }
}

/// The credentials resolved for a proxied address, and what the address is.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct TargetCredentials {
    pub(super) target: TargetKind,
    pub(super) credentials: BmcCredentials,
}

/// How long the proxy takes every address for a BMC's after nico-api denies
/// the switch probe, before it probes again.
pub(super) const SWITCH_PROBE_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// Whether nico-api lets the proxy probe for the switch at an address. A
/// nico-api that denies the probe predates the proxy's switch support, or
/// lost the proxy's grant for `FindSwitchIds`; either way it can serve only
/// BMCs, so the proxy takes every address for a BMC's and probes again after
/// [`SWITCH_PROBE_RETRY_AFTER`], rather than asking once per cache miss.
#[derive(Default)]
pub(super) struct SwitchProbe {
    denied_until: Mutex<Option<tokio::time::Instant>>,
}

impl SwitchProbe {
    /// Whether a denial still stands at `now`.
    fn is_denied_at(&self, now: tokio::time::Instant) -> bool {
        lock(&self.denied_until).is_some_and(|until| now < until)
    }

    /// Remembers a denial at `now`.
    fn denied_at(&self, now: tokio::time::Instant) {
        *lock(&self.denied_until) = Some(now + SWITCH_PROBE_RETRY_AFTER);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("the switch probe mutex must not be poisoned")
}

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

/// The credentials for the address `ip`, cached or resolved through nico-api:
/// the MAC at the address, then the switch whose NVOS management interface
/// has that MAC, if any, whose NVOS admin credentials the address takes;
/// otherwise the BMC credentials for the MAC.
pub(super) async fn resolve_credentials(
    ip: IpAddr,
    api_client: &ForgeApiClient,
    credential_cache: &CredentialCache,
    switch_probe: &SwitchProbe,
) -> Result<TargetCredentials, BmcProxyError> {
    if let Some(resolved) = credential_cache.get(&ip).await {
        tracing::debug!(
            bmc_ip_address = %ip,
            target = resolved.target.as_str(),
            "Using cached credentials",
        );
        return Ok(resolved);
    }

    tracing::debug!(bmc_ip_address = %ip, "Fetching credentials from Carbide API");
    // Parsed, so the probe's filter is a MAC nico-api can parse too: an
    // unparsable filter would match every switch, not none.
    let mac_address: MacAddress = api_client
        .find_mac_address_by_bmc_ip(forge::BmcIp {
            bmc_ip: ip.to_string(),
        })
        .await
        .map_err(|e| BmcProxyError::Api(e.to_string()))?
        .mac_address
        .parse()
        .map_err(|e| BmcProxyError::Api(format!("invalid MAC address for {ip}: {e}")))?;

    let resolved = match switch_with_nvos_mac(ip, mac_address, api_client, switch_probe).await? {
        Some(switch_id) => TargetCredentials {
            target: TargetKind::SwitchNvos,
            credentials: nvos_credentials(ip, switch_id, api_client).await?,
        },
        None => TargetCredentials {
            target: TargetKind::Bmc,
            credentials: bmc_credentials(ip, mac_address, api_client).await?,
        },
    };

    credential_cache.insert(ip, resolved.clone()).await;
    Ok(resolved)
}

/// The switch whose NVOS management interface has `mac_address`, if one
/// does: the probe that tells an NVOS address from a BMC's. While nico-api
/// denies the probe, as `switch_probe` remembers, the address is taken for
/// a BMC's.
async fn switch_with_nvos_mac(
    ip: IpAddr,
    mac_address: MacAddress,
    api_client: &ForgeApiClient,
    switch_probe: &SwitchProbe,
) -> Result<Option<SwitchId>, BmcProxyError> {
    if switch_probe.is_denied_at(tokio::time::Instant::now()) {
        tracing::debug!(
            bmc_ip_address = %ip,
            "Switch lookup denied earlier; taking the address for a BMC's",
        );
        return Ok(None);
    }
    let ids = match api_client
        .find_switch_ids(forge::SwitchSearchFilter {
            nvos_mac: Some(mac_address.to_string()),
            ..Default::default()
        })
        .await
    {
        Ok(list) => list.ids,
        Err(status) if status.code() == tonic::Code::PermissionDenied => {
            switch_probe.denied_at(tokio::time::Instant::now());
            emit(SwitchProbeDenied {
                bmc_ip_address: ip.to_string(),
                error: status.to_string(),
            });
            return Ok(None);
        }
        Err(status) => return Err(BmcProxyError::Api(status.to_string())),
    };
    match ids.as_slice() {
        [] => Ok(None),
        [switch_id] => Ok(Some(*switch_id)),
        _ => Err(BmcProxyError::AmbiguousNvosAddress(ip)),
    }
}

async fn nvos_credentials(
    ip: IpAddr,
    switch_id: SwitchId,
    api_client: &ForgeApiClient,
) -> Result<BmcCredentials, BmcProxyError> {
    api_client
        .get_switch_nvos_credentials(forge::GetSwitchNvosCredentialsRequest {
            switch_id: Some(switch_id),
        })
        .await
        .map_err(|e| BmcProxyError::Api(e.to_string()))?
        .credentials
        .ok_or(BmcProxyError::NoCredentials(ip))?
        .try_into()
}

async fn bmc_credentials(
    ip: IpAddr,
    mac_address: MacAddress,
    api_client: &ForgeApiClient,
) -> Result<BmcCredentials, BmcProxyError> {
    api_client
        .get_bmc_credentials(forge::GetBmcCredentialsRequest {
            mac_addr: mac_address.to_string(),
        })
        .await
        .map_err(|e| BmcProxyError::Api(e.to_string()))?
        .credentials
        .ok_or(BmcProxyError::NoCredentials(ip))?
        .try_into()
}

pub(super) async fn evict_cached_credentials(ip: IpAddr, credential_cache: &CredentialCache) {
    if credential_cache.remove(&ip).await.is_some() {
        tracing::info!(bmc_ip_address = %ip, "Evicted cached credentials after upstream auth failure");
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Check, check_values, scenarios};
    use rpc::forge;

    use super::{
        BmcCredentials, CREDENTIAL_CACHE_IDLE_TTL, CredentialCache, SWITCH_PROBE_RETRY_AFTER,
        SwitchProbe, evict_cached_credentials,
    };
    use crate::proxy::idle_bounded_cache;
    use crate::proxy::test_support::*;

    /// A denied probe stands for [`SWITCH_PROBE_RETRY_AFTER`] and no longer.
    #[test]
    fn a_denied_probe_stands_until_its_retry_time() {
        let probe = SwitchProbe::default();
        let denied = tokio::time::Instant::now();
        assert!(!probe.is_denied_at(denied));

        probe.denied_at(denied);
        assert!(probe.is_denied_at(denied));
        assert!(probe.is_denied_at(denied + SWITCH_PROBE_RETRY_AFTER / 2));
        assert!(!probe.is_denied_at(denied + SWITCH_PROBE_RETRY_AFTER));
    }

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
                for_bmc(BmcCredentials::UsernamePassword {
                    username: "admin".to_string(),
                    password: "secret".to_string(),
                }),
            )
            .await;

        evict_cached_credentials(ip, &credential_cache).await;

        assert!(credential_cache.get(&ip).await.is_none());
    }
}
