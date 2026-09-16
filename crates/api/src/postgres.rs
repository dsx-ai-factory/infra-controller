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

use std::net::Ipv6Addr;

use sqlx::postgres::PgConnectOptions;

/// `postgres_connect_options` parses a database URL with SQLx, then removes
/// brackets from the selected IPv6 host for SQLx's Tokio transport.
/// Query `host` overrides keep SQLx's precedence; other options are unchanged.
/// SQLx parsing errors are returned unchanged.
pub fn postgres_connect_options(database_url: &str) -> Result<PgConnectOptions, sqlx::Error> {
    let mut options = database_url.parse::<PgConnectOptions>()?;
    if let Some(address) = options
        .get_host()
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .and_then(|host| host.parse::<Ipv6Addr>().ok())
    {
        options = options.host(&address.to_string());
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;
    use std::time::Duration;

    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::scenarios;
    use sqlx::{Connection, PgConnection};
    use tokio::net::TcpListener;

    use super::postgres_connect_options;

    #[test]
    fn effective_database_host() {
        scenarios!(run = |url| postgres_connect_options(url)
            .map(|options| options.get_host().to_string())
            .map_err(drop);
            "authority hosts" {
                "postgres://user:password@[2001:db8::5]:15432/nico" => Yields("2001:db8::5".to_string()),
                "postgres://user:password@192.0.2.5:15432/nico" => Yields("192.0.2.5".to_string()),
                "postgres://user:password@postgres.example:15432/nico" => Yields("postgres.example".to_string()),
            }
            "query host overrides authority" {
                "postgres://user:password@[2001:db8::5]:15432/nico?host=postgres.example" => Yields("postgres.example".to_string()),
                "postgres://user:password@postgres.example:15432/nico?host=2001:db8::6" => Yields("2001:db8::6".to_string()),
                "postgres://user:password@postgres.example:15432/nico?host=%5B2001:db8::6%5D" => Yields("2001:db8::6".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn postgres_ipv6_url_reaches_tcp_listener() {
        let listener = TcpListener::bind((Ipv6Addr::LOCALHOST, 0))
            .await
            .expect("bind IPv6 listener");
        let port = listener.local_addr().expect("listener address").port();
        let options = postgres_connect_options(&format!(
            "postgres://user:password@[::1]:{port}/nico?sslmode=disable"
        ))
        .expect("parse IPv6 database URL");

        // Accepting TCP proves address handling without requiring a PostgreSQL server.
        tokio::select! {
            result = PgConnection::connect_with(&options) => {
                panic!("SQLx returned before the IPv6 listener accepted: {result:?}");
            }
            accepted = listener.accept() => {
                let (_stream, peer) = accepted.expect("accept SQLx connection");
                assert_eq!(peer.ip(), Ipv6Addr::LOCALHOST);
            }
            () = tokio::time::sleep(Duration::from_secs(5)) => {
                panic!("SQLx did not reach the IPv6 listener within five seconds");
            }
        }
    }
}
