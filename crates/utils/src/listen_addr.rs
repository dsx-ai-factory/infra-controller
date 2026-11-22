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

use std::fmt;
use std::net::{AddrParseError, SocketAddr};
use std::num::ParseIntError;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A TCP listen address, either family-independent or an explicit IP socket address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAddr {
    /// Try IPv6 unspecified, then IPv4 unspecified if binding fails; port 0 allocates a port.
    Any {
        /// The TCP port, from 0 through 65535.
        port: u16,
    },
    /// Bind only this IP address and family, with port 0 allocating a port.
    Explicit(SocketAddr),
}

impl ListenAddr {
    /// Returns the configured TCP port; zero requests an OS-allocated port.
    pub fn port(self) -> u16 {
        match self {
            Self::Any { port } => port,
            Self::Explicit(address) => address.port(),
        }
    }
}

/// An invalid wildcard port or explicit IP socket address.
#[derive(Debug, thiserror::Error)]
pub enum ParseListenAddrError {
    /// The port after `*:` is not a valid unsigned 16-bit integer.
    #[error("invalid wildcard listen port: {0}")]
    Port(#[from] ParseIntError),
    /// The explicit address is not an IP socket address.
    #[error("invalid explicit listen address: {0}")]
    Address(#[from] AddrParseError),
}

impl FromStr for ListenAddr {
    type Err = ParseListenAddrError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(port) = value.strip_prefix("*:") {
            Ok(Self::Any {
                port: port.parse()?,
            })
        } else {
            Ok(Self::Explicit(value.parse()?))
        }
    }
}

impl fmt::Display for ListenAddr {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any { port } => write!(formatter, "*:{port}"),
            Self::Explicit(address) => address.fmt(formatter),
        }
    }
}

impl Serialize for ListenAddr {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ListenAddr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListenAddrVisitor;

        impl<'de> serde::de::Visitor<'de> for ListenAddrVisitor {
            type Value = Result<ListenAddr, ParseListenAddrError>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(value.parse())
            }

            fn visit_bytes<E: serde::de::Error>(self, value: &[u8]) -> Result<Self::Value, E> {
                let value = std::str::from_utf8(value)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Bytes(value), &self))?;
                self.visit_str(value)
            }
        }

        deserializer
            .deserialize_string(ListenAddrVisitor)?
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn listen_addr_accepts_string_and_byte_inputs() {
        use serde::de::value::{
            BorrowedStrDeserializer, BytesDeserializer, Error, StrDeserializer, StringDeserializer,
        };

        let input = "*:1079";
        let cases = [
            (
                "borrowed string",
                ListenAddr::deserialize(BorrowedStrDeserializer::<Error>::new(input)),
            ),
            (
                "transient string",
                ListenAddr::deserialize(StrDeserializer::<Error>::new(input)),
            ),
            (
                "owned string",
                ListenAddr::deserialize(StringDeserializer::<Error>::new(input.to_string())),
            ),
            (
                "bytes",
                ListenAddr::deserialize(BytesDeserializer::<Error>::new(input.as_bytes())),
            ),
        ];
        for (scenario, result) in cases {
            let value = result.expect(scenario);
            assert_eq!(value, ListenAddr::Any { port: 1079 }, "{scenario}");
        }

        let error = ListenAddr::deserialize(BytesDeserializer::<Error>::new(&[0xff]))
            .expect_err("invalid UTF-8 is rejected");
        assert_eq!(
            error.to_string(),
            "invalid value: byte array, expected a string",
        );
    }

    #[test]
    fn serde_preserves_listen_address_strings() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(run = |value: &str| {
            let address: ListenAddr = value.parse().unwrap();
            let serialized = serde_json::to_value(address).unwrap();
            assert_eq!(serialized.as_str(), Some(value));
            serde_json::from_value::<ListenAddr>(serialized).unwrap()
        };
            "wildcard" {
                "*:1079" => ListenAddr::Any { port: 1079 },
            }
            "explicit" {
                "0.0.0.0:22" => ListenAddr::Explicit("0.0.0.0:22".parse().unwrap()),
                "[::]:1079" => ListenAddr::Explicit("[::]:1079".parse().unwrap()),
            }
        );
    }

    #[test]
    fn parses_listen_addresses() {
        scenarios!(run = |value: &str| value.parse::<ListenAddr>().map_err(drop);
            "wildcard" {
                "*:1079" => Yields(ListenAddr::Any { port: 1079 }),
                "*:0" => Yields(ListenAddr::Any { port: 0 }),
                "*:65535" => Yields(ListenAddr::Any { port: 65535 }),
            }
            "explicit address" {
                "0.0.0.0:22" => Yields(ListenAddr::Explicit("0.0.0.0:22".parse().unwrap())),
                "[::1]:1079" => Yields(ListenAddr::Explicit("[::1]:1079".parse().unwrap())),
            }
            "invalid address" {
                "*:65536" => Fails,
                "*:" => Fails,
                "*:http" => Fails,
                "*" => Fails,
                "localhost:22" => Fails,
            }
        );
    }
}
