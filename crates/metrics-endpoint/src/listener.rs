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
//! TCP listeners for metrics-facing services on IPv4-only and dual-stack hosts.

use std::io;
use std::net::{Ipv4Addr, SocketAddr};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::TcpListener;

/// Bind a TCP listener, explicitly accepting IPv4-mapped connections on an IPv6 wildcard.
///
/// If IPv6 socket creation or dual-stack configuration fails, bind the IPv4 wildcard
/// on the same port and log the cause. Explicit addresses retain their address family.
/// Bind and listen errors propagate so a port conflict cannot silently change exposure.
pub async fn bind_tcp_listener(address: SocketAddr) -> io::Result<TcpListener> {
    bind_with_socket(address, || {
        Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))
    })
    .await
}

/// Keep socket creation injectable so unavailable IPv6 can be exercised without changing the host.
async fn bind_with_socket(
    address: SocketAddr,
    create_socket: impl FnOnce() -> io::Result<Socket>,
) -> io::Result<TcpListener> {
    // Only a wildcard requests both families; an explicit IPv6 address must stay IPv6.
    if !matches!(address, SocketAddr::V6(address) if address.ip().is_unspecified()) {
        return TcpListener::bind(address).await;
    }

    let socket = create_socket().and_then(|socket| {
        // Do not inherit net.ipv6.bindv6only from the node.
        socket.set_only_v6(false)?;
        Ok(socket)
    });
    let socket = match socket {
        Ok(socket) => socket,
        // IPv4-only hosts must still expose their metrics when IPv6 setup is unavailable.
        Err(error) => {
            tracing::warn!(%address, %error, "IPv6 TCP setup unavailable; binding IPv4 wildcard");
            return TcpListener::bind((Ipv4Addr::UNSPECIFIED, address.port())).await;
        }
    };

    // Preserve Tokio's nonblocking listener behavior and report ordinary bind failures.
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&address.into())?;
    socket.listen(1024)?;
    TcpListener::from_std(socket.into())
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;
    use std::time::Duration;

    use tokio::net::TcpStream;

    use super::*;

    /// Probe IPv6 loopback independently so IPv4-only hosts skip platform assertions without
    /// hiding listener regressions; require IPv4 to work before treating a failure as IPv6-specific.
    fn ipv6_loopback_available() -> bool {
        match std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) {
            Ok(_) => true,
            Err(error) => {
                // General socket or resource failures must still fail the test.
                let _listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .expect("IPv4 loopback must be available before skipping IPv6 assertions");
                eprintln!(
                    "Skipping IPv6 listener assertions: IPv6 loopback is unavailable: {error}"
                );
                false
            }
        }
    }

    /// A v6-only socket must accept both families after setup, independently of node defaults.
    #[tokio::test]
    async fn wildcard_accepts_both_families() {
        // The dual-stack assertions require a real IPv6 loopback interface.
        if !ipv6_loopback_available() {
            return;
        }

        // Start v6-only to exercise the same override required on bindv6only=1 hosts.
        let listener = bind_with_socket("[::]:0".parse().expect("wildcard"), || {
            let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
            socket.set_only_v6(true)?;
            Ok(socket)
        })
        .await
        .expect("dual-stack listener");
        let port = listener.local_addr().expect("bound address").port();

        for address in [
            // Existing IPv4 scrapers must remain reachable through the wildcard.
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            // IPv6 scrapers must reach the same listener and port.
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        ] {
            tokio::time::timeout(Duration::from_secs(5), async {
                let _client = TcpStream::connect(address).await.expect("connect");
                listener.accept().await.expect("accept");
            })
            .await
            .expect("connection deadline");
        }
    }

    /// An unavailable IPv6 stack falls back to an accepting IPv4 listener on the requested port.
    #[tokio::test]
    async fn unavailable_ipv6_falls_back_to_ipv4() {
        // Inject the failure rather than relying on the developer's IPv6 configuration.
        let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("reserve port");
        let port = reservation.local_addr().expect("reserved address").port();
        drop(reservation);
        let listener = bind_with_socket(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)), || {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        })
        .await
        .expect("IPv4 fallback");
        let address = listener.local_addr().expect("bound address");
        assert_eq!(address.ip(), Ipv4Addr::UNSPECIFIED);
        assert_eq!(address.port(), port);
        tokio::time::timeout(Duration::from_secs(5), async {
            let _client = TcpStream::connect((Ipv4Addr::LOCALHOST, address.port()))
                .await
                .expect("IPv4 connect");
            listener.accept().await.expect("IPv4 accept");
        })
        .await
        .expect("connection deadline");
    }

    /// Explicit binds and occupied ports must not be converted into broader fallback listeners.
    #[tokio::test]
    async fn explicit_address_and_bind_error_are_preserved() {
        // The explicit IPv6 bind and conflicting wildcard require IPv6 loopback.
        if !ipv6_loopback_available() {
            return;
        }

        // The socket factory must not run for an explicit IPv6 address.
        let listener = bind_with_socket("[::1]:0".parse().expect("loopback"), || {
            panic!("explicit addresses do not request dual-stack setup")
        })
        .await
        .expect("IPv6 loopback");
        let address = listener.local_addr().expect("bound address");
        assert_eq!(address.ip(), Ipv6Addr::LOCALHOST);

        // A wildcard would conflict with this port; the conflict must remain visible.
        let error = bind_tcp_listener(SocketAddr::from((Ipv6Addr::UNSPECIFIED, address.port())))
            .await
            .expect_err("occupied port");
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    }
}
