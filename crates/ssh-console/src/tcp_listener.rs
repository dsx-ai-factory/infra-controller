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

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use carbide_utils::ListenAddr;
use tokio::net::TcpListener;

const MAX_EPHEMERAL_BIND_ATTEMPTS: usize = 10;

pub(crate) async fn bind(address: ListenAddr) -> io::Result<(TcpListener, SocketAddr)> {
    let mut attempt = 1;
    loop {
        let result = match address {
            ListenAddr::Any { port } => {
                let candidates = [
                    SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
                    SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
                ];
                TcpListener::bind(&candidates[..]).await
            }
            ListenAddr::Explicit(address) => TcpListener::bind(address).await,
        };
        match result {
            Ok(listener) => {
                let effective_address = listener.local_addr()?;
                return Ok((listener, effective_address));
            }
            Err(error) if should_retry(address, error.kind(), attempt) => {
                tracing::warn!(
                    configured_address = %address,
                    %error,
                    attempt,
                    max_attempts = MAX_EPHEMERAL_BIND_ATTEMPTS,
                    "ephemeral TCP listener bind conflicted; retrying"
                );
                attempt += 1;
                tokio::task::yield_now().await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn should_retry(address: ListenAddr, error_kind: io::ErrorKind, attempt: usize) -> bool {
    address.port() == 0
        && error_kind == io::ErrorKind::AddrInUse
        && attempt < MAX_EPHEMERAL_BIND_ATTEMPTS
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy, Debug)]
    struct RetryInput {
        port: u16,
        error_kind: io::ErrorKind,
        attempt: usize,
    }

    #[test]
    fn retries_only_ephemeral_address_conflicts_with_attempts_remaining() {
        value_scenarios!(run = |input: RetryInput| should_retry(
            ListenAddr::Explicit(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), input.port)),
            input.error_kind,
            input.attempt,
        );
            "ephemeral address conflict" {
                RetryInput {
                    port: 0,
                    error_kind: io::ErrorKind::AddrInUse,
                    attempt: 1,
                } => true,
                RetryInput {
                    port: 0,
                    error_kind: io::ErrorKind::AddrInUse,
                    attempt: MAX_EPHEMERAL_BIND_ATTEMPTS - 1,
                } => true,
            }

            "non-retryable bind failure" {
                RetryInput {
                    port: 22,
                    error_kind: io::ErrorKind::AddrInUse,
                    attempt: 1,
                } => false,
                RetryInput {
                    port: 0,
                    error_kind: io::ErrorKind::PermissionDenied,
                    attempt: 1,
                } => false,
                RetryInput {
                    port: 0,
                    error_kind: io::ErrorKind::AddrInUse,
                    attempt: MAX_EPHEMERAL_BIND_ATTEMPTS,
                } => false,
            }
        );
    }

    #[tokio::test]
    async fn ephemeral_bind_returns_effective_address() {
        let configured_address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let (listener, effective_address) = bind(ListenAddr::Explicit(configured_address))
            .await
            .expect("ephemeral listener should bind");

        assert_ne!(effective_address.port(), 0);
        assert_eq!(effective_address.ip(), configured_address.ip());
        assert_eq!(
            listener
                .local_addr()
                .expect("bound listener should have a local address"),
            effective_address
        );
    }

    #[tokio::test]
    async fn wildcard_bind_accepts_ipv4_connections() {
        let (listener, address) = bind(ListenAddr::Any { port: 0 }).await.unwrap();
        assert!(address.ip().is_unspecified());
        assert_ne!(address.port(), 0);
        let client = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, address.port()))
            .await
            .expect("wildcard listener should accept IPv4");
        let (_, peer) = listener.accept().await.unwrap();
        assert_eq!(peer.port(), client.local_addr().unwrap().port());
    }

    #[tokio::test]
    async fn occupied_explicit_port_returns_address_in_use() {
        let existing = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("port reservation should bind");
        let occupied_address = existing
            .local_addr()
            .expect("port reservation should have a local address");

        let error = bind(ListenAddr::Explicit(occupied_address))
            .await
            .expect_err("occupied explicit port should fail");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
    }
}
