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

//! Shared TCP listener setup for bmc-proxy's two listeners (the proxy
//! itself and its metrics endpoint), both of which default to binding
//! the IPv6 unspecified address.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use tokio::net::TcpListener;

/// Binds a TCP listener to `address`, falling back to the equivalent
/// IPv4 unspecified address (`0.0.0.0`, same port) if the bind fails
/// because the host doesn't support IPv6 at all (`EAFNOSUPPORT`).
///
/// This is the default `[::]` bind failing outright on hosts with IPv6
/// disabled in the kernel -- the kernel can't create an AF_INET6 socket,
/// so the bind fails immediately rather than falling back on its own.
/// Without this, the process exits at startup on such hosts.
/// Whether a failed bind on `address` should be retried on the
/// equivalent IPv4 unspecified address, rather than treated as fatal.
fn should_retry_as_ipv4(address: &SocketAddr, err: &io::Error) -> bool {
    address.ip() == IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        && err.raw_os_error() == Some(libc::EAFNOSUPPORT)
}

pub(crate) async fn bind_with_ipv4_fallback(address: SocketAddr) -> io::Result<TcpListener> {
    match TcpListener::bind(address).await {
        Ok(listener) => Ok(listener),
        Err(e) if should_retry_as_ipv4(&address, &e) => {
            let fallback = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), address.port());
            tracing::warn!(
                original_address = %address,
                fallback_address = %fallback,
                error = %e,
                "IPv6 not supported on this host, falling back to IPv4",
            );
            TcpListener::bind(fallback).await
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_retry_as_ipv4_handles_various_inputs() {
        struct Case {
            name: &'static str,
            address: SocketAddr,
            err: io::Error,
            expect_retry: bool,
        }

        let ipv6_addr: SocketAddr = "[::]:1079".parse().unwrap();
        let ipv4_addr: SocketAddr = "0.0.0.0:1079".parse().unwrap();

        let cases = [
            Case {
                name: "ipv6 address, EAFNOSUPPORT -- should retry",
                address: ipv6_addr,
                err: io::Error::from_raw_os_error(libc::EAFNOSUPPORT),
                expect_retry: true,
            },
            Case {
                name: "ipv6 address, different OS error -- should not retry",
                address: ipv6_addr,
                err: io::Error::from_raw_os_error(libc::EADDRINUSE),
                expect_retry: false,
            },
            Case {
                name: "ipv4 address, EAFNOSUPPORT -- should not retry (already IPv4)",
                address: ipv4_addr,
                err: io::Error::from_raw_os_error(libc::EAFNOSUPPORT),
                expect_retry: false,
            },
            Case {
                name: "ipv6 address, non-OS error -- should not retry",
                address: ipv6_addr,
                err: io::Error::other("some other error"),
                expect_retry: false,
            },
            Case {
                name: "ipv6 loopback address, EAFNOSUPPORT -- should not retry",
                address: "[::1]:1079".parse().unwrap(),
                err: io::Error::from_raw_os_error(libc::EAFNOSUPPORT),
                expect_retry: false,
            },
        ];

        for case in cases {
            assert_eq!(
                should_retry_as_ipv4(&case.address, &case.err),
                case.expect_retry,
                "case '{}' failed",
                case.name
            );
        }
    }

    #[tokio::test]
    async fn bind_with_ipv4_fallback_succeeds_on_available_address() {
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let listener = bind_with_ipv4_fallback(addr).await;
        assert!(listener.is_ok(), "expected successful bind on loopback");
    }
}
