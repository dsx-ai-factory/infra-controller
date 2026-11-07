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

//! SOCKS5 proxy address for the API client.

use std::net::SocketAddr;
use std::time::Duration;

use eyre::{WrapErr, eyre};

/// Bound on waiting for the proxy lookup, matching the API client's per-query
/// DNS timeout. The lookup runs before any API connection, so it is not part
/// of the connect timeout.
const PROXY_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Resolve a `host:port` proxy setting to the socket address the API client
/// requires.
///
/// The API client's connector accepts only an IP address and port, so a name
/// such as `localhost` is handled here, before the client is built. The
/// system resolver is used deliberately: the proxy is typically a local tunnel
/// with entry in `/etc/hosts` or the user's OS resolver settings, the same
/// lookup other tools use for the same proxy URL. When both IPv4 and IPv6 resolve,
/// IPv4 is chosen, matching the connector's IPv4-first target order; use a
/// bracketed IPv6 literal such as `[::1]:1080` to select IPv6.
pub(crate) async fn resolve_socks_proxy(proxy: &str) -> eyre::Result<SocketAddr> {
    // The system lookup runs on a blocking thread that cannot be cancelled;
    // the timeout bounds the wait, and the system resolver bounds the thread.
    let addrs: Vec<SocketAddr> =
        tokio::time::timeout(PROXY_LOOKUP_TIMEOUT, tokio::net::lookup_host(proxy))
            .await
            .map_err(|_| eyre!("proxy lookup timed out after {PROXY_LOOKUP_TIMEOUT:?}"))?
            .wrap_err("look up the proxy host")?
            .collect();
    preferred_address(&addrs).ok_or_else(|| eyre!("proxy host resolved to no addresses"))
}

fn preferred_address(addrs: &[SocketAddr]) -> Option<SocketAddr> {
    addrs
        .iter()
        .find(|addr| addr.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async, value_scenarios};

    use super::*;

    const V4: &str = "127.0.0.1:1080";
    const V6: &str = "[::1]:1080";

    fn addr(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    #[test]
    fn prefers_ipv4_then_resolver_order() {
        value_scenarios!(run = |addrs: Vec<SocketAddr>| preferred_address(&addrs);
            "IPv4 wins over an earlier IPv6 address" {
                vec![addr(V6), addr(V4)] => Some(addr(V4)),
            }
            "IPv6-only results keep the first address" {
                vec![addr(V6), addr("[::2]:1080")] => Some(addr(V6)),
            }
            "no addresses" {
                Vec::new() => None,
            }
        );
    }

    /// `localhost` comes from the hosts file, so these rows need no DNS server.
    #[tokio::test]
    async fn resolves_proxy_settings() {
        check_cases_async(
            [
                Case {
                    scenario: "host name resolves to its IPv4 address",
                    input: "localhost:1080",
                    expect: Yields(addr(V4)),
                },
                Case {
                    scenario: "bracketed IPv6 literal selects IPv6",
                    input: V6,
                    expect: Yields(addr(V6)),
                },
                Case {
                    scenario: "proxy URL without a port",
                    input: "localhost:",
                    expect: Fails,
                },
            ],
            |proxy| async move { resolve_socks_proxy(proxy).await.map_err(drop) },
        )
        .await;
    }
}
