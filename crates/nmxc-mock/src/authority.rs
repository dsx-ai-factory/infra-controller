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

//! Choosing a domain from the address a client used.
//!
//! NICo reaches a rack's NMX-C at that rack's switch NVOS address. When every
//! such address is routed to one machine-a-tron listener, the address survives
//! only as the request's authority, so that is what selects the domain. The
//! same idea machine-a-tron uses to pick a simulated BMC, applied inside the
//! gRPC service because gRPC cannot pass through the BMC multiplexer.

use std::net::IpAddr;

use axum::extract::Request;
use axum::http::uri::Authority;
use axum::middleware::Next;
use axum::response::Response;
use carbide_axum_utils::authority_router::request_authority;

use crate::SimDomain;

/// The authority a request was addressed to, recorded before tonic sees the
/// request because tonic does not expose the URI to service methods.
#[derive(Clone, Debug)]
pub(crate) struct RequestAuthority(pub(crate) String);

pub(crate) async fn record_authority(mut request: Request, next: Next) -> Response {
    if let Some(authority) = request_authority(&request) {
        request.extensions_mut().insert(RequestAuthority(authority));
    }
    next.run(request).await
}

/// The domain answering at `authority`.
///
/// An authority whose host is one of a domain's NVOS addresses selects that
/// domain. Anything else falls back to the only domain when there is exactly
/// one, so a single-rack host answers on any name (local tests, a loopback
/// port-forward) without a per-address route.
pub(crate) fn resolve_domain<'a>(
    authority: Option<&str>,
    domains: &'a [SimDomain],
) -> Result<&'a SimDomain, tonic::Status> {
    if let Some(ip) = authority.and_then(authority_ip)
        && let Some(domain) = domains.iter().find(|domain| domain.nvos_ips.contains(&ip))
    {
        return Ok(domain);
    }
    match domains {
        [only] => Ok(only),
        [] => Err(tonic::Status::unavailable(
            "the machine-a-tron NMX-C mock has no simulated NVLink domain",
        )),
        _ => Err(tonic::Status::not_found(format!(
            "no simulated NVLink domain answers at {}; a domain is addressed by one of its \
             switches' NVOS addresses",
            authority.unwrap_or("<no authority>")
        ))),
    }
}

/// The host of `authority` when it is an IP address, without port or IPv6
/// brackets.
fn authority_ip(authority: &str) -> Option<IpAddr> {
    let authority: Authority = authority.parse().ok()?;
    authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, FailsWith, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    fn domain(key: &str, nvos_ip: &str) -> SimDomain {
        SimDomain {
            key: key.into(),
            nvos_ips: vec![nvos_ip.parse().unwrap()],
            ..SimDomain::default()
        }
    }

    #[test]
    fn resolves_domain_by_authority() {
        let two: &[SimDomain] = &[domain("rack-a", "10.0.0.1"), domain("rack-b", "fd00::2")];
        let one: &[SimDomain] = &[domain("rack-a", "10.0.0.1")];
        let none: &[SimDomain] = &[];

        scenarios!(run = |(authority, domains): (Option<&str>, &[SimDomain])| {
            resolve_domain(authority, domains)
                .map(|domain| domain.key.clone())
                .map_err(|status| status.code())
        };
            "an NVOS address selects its domain" {
                (Some("10.0.0.1:9370"), two) => Yields("rack-a".to_string()),
                (Some("10.0.0.1"), two) => Yields("rack-a".to_string()),
                (Some("[fd00::2]:9370"), two) => Yields("rack-b".to_string()),
            }

            "a single domain answers on any name" {
                (Some("localhost:1266"), one) => Yields("rack-a".to_string()),
                (Some("10.9.9.9:9370"), one) => Yields("rack-a".to_string()),
                (None, one) => Yields("rack-a".to_string()),
            }

            "otherwise nothing answers" {
                (Some("10.9.9.9:9370"), two) => FailsWith(tonic::Code::NotFound),
                (Some("localhost:1266"), two) => FailsWith(tonic::Code::NotFound),
                (None, two) => FailsWith(tonic::Code::NotFound),
                (Some("10.0.0.1:9370"), none) => FailsWith(tonic::Code::Unavailable),
                (Some("not a valid authority ]["), one) => Yields("rack-a".to_string()),
                (Some("not a valid authority ]["), two) => Fails,
            }
        );
    }
}
