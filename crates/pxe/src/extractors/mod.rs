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

use std::net::IpAddr;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_client_ip::ClientIp;

use crate::rpc_error::PxeRequestError;

mod machine;
pub(super) mod machine_architecture;
mod machine_interface;

/// Extracts the client address shared by both PXE RPC paths, preserving its lookup identity.
async fn client_ip<S: Sync>(parts: &mut Parts, state: &S) -> Result<IpAddr, PxeRequestError> {
    // Dual-stack listeners report IPv4 peers as mapped IPv6, but Core stores their IPv4 addresses.
    ClientIp::from_request_parts(parts, state)
        .await
        .map(|ip| ip.0.to_canonical())
        .map_err(PxeRequestError::MissingIp)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use axum_client_ip::ClientIpSource;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};

    use super::*;

    /// Both PXE RPC paths need the stored address family and must retain the TCP peer's identity.
    #[tokio::test]
    async fn client_ip_preserves_lookup_identity() {
        check_cases_async(
            [
                // IPv4 peers on a dual-stack listener must match the existing IPv4 records.
                Case {
                    scenario: "mapped IPv4 peer uses its IPv4 address",
                    input: "[::ffff:192.0.2.10]:12345",
                    expect: Yields("192.0.2.10".to_string()),
                },
                // Explicit IPv4 listeners and the IPv4 fallback must keep their existing identity.
                Case {
                    scenario: "native IPv4 peer stays IPv4",
                    input: "192.0.2.10:12345",
                    expect: Yields("192.0.2.10".to_string()),
                },
                // A real IPv6 peer must remain distinct from IPv4 address records.
                Case {
                    scenario: "native IPv6 peer stays IPv6",
                    input: "[2001:db8::10]:12345",
                    expect: Yields("2001:db8::10".to_string()),
                },
            ],
            |peer| async move {
                // Match the production source selection and include a forged forwarding header.
                let request = Request::builder()
                    .extension(ClientIpSource::ConnectInfo)
                    .extension(ConnectInfo(
                        peer.parse::<SocketAddr>().expect("peer address"),
                    ))
                    .header("X-Forwarded-For", "198.51.100.1")
                    .body(())
                    .expect("request");
                let (mut parts, _) = request.into_parts();

                // Check the address text sent to Core, independently of the untrusted header.
                client_ip(&mut parts, &())
                    .await
                    .map(|ip| ip.to_string())
                    .map_err(|error| error.to_string())
            },
        )
        .await;
    }
}
