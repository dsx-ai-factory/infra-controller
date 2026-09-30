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

//! Which BMC a request is for: the `Forwarded` header's `host`, `mac`, or
//! `serial`, resolved to the BMC's IP.

use std::net::{AddrParseError, IpAddr};
use std::str::FromStr;
use std::time::Duration;

use http::HeaderMap;
use mac_address::{MacAddress, MacParseError};
use moka::future::Cache as MokaCache;
use rpc::forge;
use rpc::forge::find_bmc_ips_request::LookupBy;

use crate::proxy::BmcProxyState;

/// Resolved `Forwarded: mac=`/`serial=` targets. A BMC that moves to a new
/// IP produces connection errors, not 401s, so no request-path signal evicts
/// these -- the TTL is what heals a stale resolution.
pub(super) type LookupToIpCache = MokaCache<LookupBy, IpAddr>;

/// How long a resolved BMC IP may be served before the API is asked again.
pub(super) const IP_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(super) enum ForwardedTarget<'a> {
    Ip(IpAddr),
    Mac(MacAddress),
    Serial(&'a str),
}

#[derive(thiserror::Error, Debug)]
pub(super) enum ForwardedHeaderParseError {
    #[error("invalid IP in forwarded host header: {0}")]
    Ip(#[from] AddrParseError),
    #[error("invalid MAC address in forwarded host header: {0}")]
    Mac(#[from] MacParseError),
}

pub(super) async fn ip_for_forwarded_target(
    forwarded_target: &ForwardedTarget<'_>,
    state: &BmcProxyState,
) -> Result<Option<IpAddr>, tonic::Status> {
    let lookup_by = match forwarded_target {
        ForwardedTarget::Ip(ip) => {
            // No need to look up
            return Ok(Some(*ip));
        }
        ForwardedTarget::Mac(mac) => LookupBy::MacAddress(mac.to_string()),
        ForwardedTarget::Serial(serial) => LookupBy::Serial(serial.to_string()),
    };

    if let Some(ip) = state.ip_cache.get(&lookup_by).await {
        return Ok(Some(ip));
    }

    let lookup_by_str = match &lookup_by {
        LookupBy::Serial(serial) => format!("Serial number {serial}"),
        LookupBy::MacAddress(mac) => format!("MAC address {mac}"),
    };

    let ips = state
        .api_client
        .find_bmc_ips(forge::FindBmcIpsRequest {
            lookup_by: Some(lookup_by.clone()),
        })
        .await?
        .bmc_ips
        .iter()
        .filter_map(|s| {
            IpAddr::from_str(s)
                .inspect_err(|e| tracing::error!(error = %e, "Invalid IP address returned by API"))
                .ok()
        })
        .collect::<Vec<_>>();

    if ips.is_empty() {
        return Ok(None);
    }

    let (v4_ips, v6_ips): (Vec<IpAddr>, Vec<IpAddr>) = ips.into_iter().partition(|ip| ip.is_ipv4());

    let ip = match (v4_ips.len(), v6_ips.len()) {
        (0, 1..) => {
            if v6_ips.len() > 1 {
                tracing::warn!(
                    lookup_by = %lookup_by_str,
                    ip_addresses = ?v6_ips,
                    "Multiple IPv6 BMC IP's found, using first one",
                );
            }
            v6_ips.into_iter().next()
        }
        _ => {
            // TODO: We may want to be smart about when to pick IPv6 vs IPv4, but for now just pick IPv4
            // first, in case of broken dual-stack setups.
            if v4_ips.len() > 1 {
                tracing::warn!(
                    lookup_by = %lookup_by_str,
                    ip_addresses = ?v4_ips,
                    "Multiple IPv4 BMC IP's found, using first one",
                );
            }
            v4_ips.into_iter().next()
        }
    };

    if let Some(ip) = ip {
        state.ip_cache.insert(lookup_by, ip).await;
    }
    Ok(ip)
}

pub(super) fn forwarded_header_value(
    headers: &HeaderMap,
) -> Result<Option<ForwardedTarget<'_>>, ForwardedHeaderParseError> {
    let values = headers.get_all("forwarded");
    for raw_value in values {
        let Ok(raw_value) = raw_value.to_str() else {
            continue;
        };
        for element in raw_value.split(',') {
            for pair in element.split(';') {
                let Some((key, value)) = pair.trim().split_once('=') else {
                    continue;
                };
                let key = key.trim();
                if key.eq_ignore_ascii_case("host") {
                    return Ok(Some(ForwardedTarget::Ip(parse_forwarded_host_value(
                        value.trim(),
                    )?)));
                } else if key.eq_ignore_ascii_case("mac") {
                    return Ok(Some(ForwardedTarget::Mac(MacAddress::from_str(
                        value.trim(),
                    )?)));
                } else if key.eq_ignore_ascii_case("serial") {
                    return Ok(Some(ForwardedTarget::Serial(value.trim())));
                }
            }
        }
    }
    Ok(None)
}

fn parse_forwarded_host_value(value: &str) -> Result<IpAddr, AddrParseError> {
    let value = value.trim_matches('"');

    let result = IpAddr::from_str(value);
    if let Ok(ip) = result {
        return Ok(ip);
    }

    // If it failed to parse, maybe it's a bracked ipv6 address, support that
    if let Some(rest) = value.strip_prefix('[')
        && let Some((host, _)) = rest.split_once(']')
    {
        IpAddr::from_str(host)
    } else {
        // Nope, just return the failure
        result
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use axum::http::{HeaderMap, HeaderName, HeaderValue};
    use carbide_test_support::value_scenarios;
    use mac_address::MacAddress;
    use rpc::forge::find_bmc_ips_request::LookupBy;

    use super::{
        ForwardedTarget, forwarded_header_value, ip_for_forwarded_target,
        parse_forwarded_host_value,
    };
    use crate::proxy::test_support::*;

    #[derive(Clone, Copy)]
    enum ForwardedHeaderCase {
        Missing,
        InvalidUtf8ThenHost,
        HostAmongParameters,
        HostInLaterElement,
        QuotedIpv4Host,
        Mac,
        Serial,
        InvalidHost,
        InvalidMac,
    }

    #[derive(Debug, PartialEq)]
    enum ForwardedTargetSummary {
        None,
        Ip(String),
        Mac(String),
        Serial(String),
        Error(&'static str),
    }

    fn forwarded_headers(case: ForwardedHeaderCase) -> HeaderMap {
        let mut headers = HeaderMap::new();
        match case {
            ForwardedHeaderCase::Missing => {}
            ForwardedHeaderCase::InvalidUtf8ThenHost => {
                headers.append(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_bytes(&[0xff]).expect("non-UTF8 header value"),
                );
                headers.append(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https;host=10.1.2.3"),
                );
            }
            ForwardedHeaderCase::HostAmongParameters => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https;host=10.1.2.3;for=10.0.0.1"),
                );
            }
            ForwardedHeaderCase::HostInLaterElement => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("for=10.0.0.1, proto=https; host=10.2.3.4"),
                );
            }
            ForwardedHeaderCase::QuotedIpv4Host => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static(r#"host="10.3.4.5""#),
                );
            }
            ForwardedHeaderCase::Mac => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https;mac=00:11:22:33:44:55;for=10.0.0.1"),
                );
            }
            ForwardedHeaderCase::Serial => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("proto=https; serial = DGX-A100-0001 ; for=10.0.0.1"),
                );
            }
            ForwardedHeaderCase::InvalidHost => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("host=not-an-ip"),
                );
            }
            ForwardedHeaderCase::InvalidMac => {
                headers.insert(
                    HeaderName::from_static("forwarded"),
                    HeaderValue::from_static("mac=not-a-mac-address"),
                );
            }
        }
        headers
    }

    fn summarize_forwarded_header(case: ForwardedHeaderCase) -> ForwardedTargetSummary {
        match forwarded_header_value(&forwarded_headers(case)) {
            Ok(Some(ForwardedTarget::Ip(ip))) => ForwardedTargetSummary::Ip(ip.to_string()),
            Ok(Some(ForwardedTarget::Mac(mac))) => ForwardedTargetSummary::Mac(mac.to_string()),
            Ok(Some(ForwardedTarget::Serial(serial))) => {
                ForwardedTargetSummary::Serial(serial.to_string())
            }
            Ok(None) => ForwardedTargetSummary::None,
            Err(super::ForwardedHeaderParseError::Ip(_)) => ForwardedTargetSummary::Error("ip"),
            Err(super::ForwardedHeaderParseError::Mac(_)) => ForwardedTargetSummary::Error("mac"),
        }
    }

    #[test]
    fn forwarded_host_value_parsing() {
        value_scenarios!(
            run = |value| {
                parse_forwarded_host_value(value)
                    .ok()
                    .map(|ip| ip.to_string())
            };
            "IPv4" {
                "10.0.0.5" => Some("10.0.0.5".to_string()),
            }

            "raw IPv6" {
                "2001:db8::1" => Some("2001:db8::1".to_string()),
            }

            "quoted bracketed IPv6 with port" {
                "\"[2001:db8::1]:443\"" => Some("2001:db8::1".to_string()),
            }

            "bracketed IPv6 without port" {
                "[2001:db8::2]" => Some("2001:db8::2".to_string()),
            }

            "hostname rejected" {
                "bmc.example.com" => None,
            }

            "IPv4 with port rejected" {
                "10.0.0.5:443" => None,
            }
        );
    }

    #[test]
    fn forwarded_header_targets() {
        value_scenarios!(
            run = summarize_forwarded_header;
            "missing forwarded header" {
                ForwardedHeaderCase::Missing => ForwardedTargetSummary::None,
            }

            "invalid UTF-8 value skipped" {
                ForwardedHeaderCase::InvalidUtf8ThenHost => ForwardedTargetSummary::Ip("10.1.2.3".to_string()),
            }

            "host among parameters" {
                ForwardedHeaderCase::HostAmongParameters => ForwardedTargetSummary::Ip("10.1.2.3".to_string()),
            }

            "host in later element" {
                ForwardedHeaderCase::HostInLaterElement => ForwardedTargetSummary::Ip("10.2.3.4".to_string()),
            }

            "quoted IPv4 host" {
                ForwardedHeaderCase::QuotedIpv4Host => ForwardedTargetSummary::Ip("10.3.4.5".to_string()),
            }

            "MAC target" {
                ForwardedHeaderCase::Mac => ForwardedTargetSummary::Mac("00:11:22:33:44:55".to_string()),
            }

            "serial target" {
                ForwardedHeaderCase::Serial => ForwardedTargetSummary::Serial("DGX-A100-0001".to_string()),
            }

            "invalid host" {
                ForwardedHeaderCase::InvalidHost => ForwardedTargetSummary::Error("ip"),
            }

            "invalid MAC" {
                ForwardedHeaderCase::InvalidMac => ForwardedTargetSummary::Error("mac"),
            }
        );
    }

    #[tokio::test]
    async fn forwarded_ip_target_resolves_without_lookup() {
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let state = test_state_with_ip_cache(HashMap::new()).await;

        assert_eq!(
            ip_for_forwarded_target(&ForwardedTarget::Ip(ip), &state)
                .await
                .unwrap(),
            Some(ip)
        );
    }

    #[tokio::test]
    async fn forwarded_mac_target_resolves_from_ip_cache() {
        let mac = MacAddress::from_str("00:11:22:33:44:55").unwrap();
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let state =
            test_state_with_ip_cache(HashMap::from([(LookupBy::MacAddress(mac.to_string()), ip)]))
                .await;

        assert_eq!(
            ip_for_forwarded_target(&ForwardedTarget::Mac(mac), &state)
                .await
                .unwrap(),
            Some(ip)
        );
    }

    #[tokio::test]
    async fn forwarded_serial_target_resolves_from_ip_cache() {
        let serial = "DGX-A100-0001";
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5));
        let state =
            test_state_with_ip_cache(HashMap::from([(LookupBy::Serial(serial.to_string()), ip)]))
                .await;

        assert_eq!(
            ip_for_forwarded_target(&ForwardedTarget::Serial(serial), &state)
                .await
                .unwrap(),
            Some(ip)
        );
    }
}
