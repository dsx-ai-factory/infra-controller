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
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::net::Ipv6Addr;
use std::str::FromStr;

use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::host_port_pair::HostPortParseError::{
    EmptyString, InvalidPort, InvalidString, UriUnsupported,
};

/// A [`HostPortPair`] is a representation of a string like `some-host.fqdn:1234`.
///
/// It represents invariants that either the host must be set, or the port, or both. It is distinct
/// from a URI because there cases where (a) we don't want to specify a scheme, and (b) we don't
/// want to specify anything else like path/etc.
#[derive(PartialEq, Eq, Hash, Debug, Clone)]
pub enum HostPortPair {
    HostOnly(String),
    PortOnly(u16),
    HostAndPort(String, u16),
}

impl HostPortPair {
    pub fn new(host: String, port: Option<u16>) -> Self {
        match port {
            Some(port) => HostPortPair::HostAndPort(host, port),
            None => HostPortPair::HostOnly(host),
        }
    }

    pub fn host(&self) -> Option<&str> {
        match self {
            HostPortPair::PortOnly(_) => None,
            HostPortPair::HostOnly(h) | HostPortPair::HostAndPort(h, _) => Some(h.as_str()),
        }
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            HostPortPair::HostOnly(_) => None,
            HostPortPair::PortOnly(p) | HostPortPair::HostAndPort(_, p) => Some(*p),
        }
    }

    /// Returns the host formatted for use in a URL authority: a bare IPv6
    /// literal is bracketed (`[2001:db8::1]`); hostnames and IPv4 literals are
    /// returned unchanged. `None` when there is no host.
    ///
    /// Use this instead of [`Self::host`] when interpolating into a URL or
    /// `host:port` authority, where an unbracketed IPv6 literal is invalid.
    pub fn url_host(&self) -> Option<Cow<'_, str>> {
        self.host().map(bracket_ipv6)
    }
}

/// Brackets `host` when it is a bare IPv6 literal; other hosts pass through.
fn bracket_ipv6(host: &str) -> Cow<'_, str> {
    if host.parse::<Ipv6Addr>().is_ok() {
        Cow::Owned(format!("[{host}]"))
    } else {
        Cow::Borrowed(host)
    }
}

impl FromStr for HostPortPair {
    type Err = HostPortParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains("://") {
            return Err(UriUnsupported);
        }

        // A bracketed host is the canonical way to write an IPv6 literal, e.g.
        // `[2001:db8::1]` or `[2001:db8::1]:443`. The host is stored without the
        // brackets so callers can use it directly. The plain `split(':')` below
        // can't handle this because an IPv6 address itself contains colons.
        if let Some(rest) = s.strip_prefix('[') {
            let (host, after) = rest.split_once(']').ok_or(InvalidString)?;
            if host.is_empty() {
                return Err(InvalidString);
            }
            return match after {
                "" => Ok(HostPortPair::HostOnly(host.to_string())),
                _ => {
                    let port = after.strip_prefix(':').ok_or(InvalidString)?;
                    let port = port
                        .parse::<u16>()
                        .map_err(|_| InvalidPort(port.to_string()))?;
                    Ok(HostPortPair::HostAndPort(host.to_string(), port))
                }
            };
        }

        // A bare (unbracketed) IPv6 literal is a host with no port; its colons
        // would otherwise be misread as host/port separators below.
        if s.parse::<Ipv6Addr>().is_ok() {
            return Ok(HostPortPair::HostOnly(s.to_string()));
        }

        match s.split(":").collect::<Vec<_>>().as_slice() {
            [h, p] => {
                let p = p.parse::<u16>().map_err(|_| InvalidPort(p.to_string()))?;

                if h.is_empty() {
                    Ok(HostPortPair::PortOnly(p))
                } else {
                    Ok(HostPortPair::HostAndPort(h.to_string(), p))
                }
            }
            [h] => {
                if h.is_empty() {
                    Err(EmptyString)
                } else {
                    Ok(HostPortPair::HostOnly(h.to_string()))
                }
            }
            _ => Err(InvalidString),
        }
    }
}

impl Display for HostPortPair {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            HostPortPair::HostOnly(h) => write!(f, "{h}"),
            HostPortPair::PortOnly(p) => write!(f, "{p}"),
            // Bracket an IPv6 literal host so the `host:port` form is
            // unambiguous and round-trips back through `from_str`.
            HostPortPair::HostAndPort(h, p) => write!(f, "{}:{p}", bracket_ipv6(h)),
        }
    }
}

impl Serialize for HostPortPair {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_str())
    }
}

impl<'de> Deserialize<'de> for HostPortPair {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(HostPortPairVisitor)
    }
}

struct HostPortPairVisitor;
impl Visitor<'_> for HostPortPairVisitor {
    type Value = HostPortPair;

    fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
        write!(formatter, "A host:port string")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Self::Value::from_str(v)
            .map_err(|e| serde::de::Error::custom(format!("Invalid host-port pair: {e}")))
    }
}

#[derive(thiserror::Error, PartialEq, Eq, Debug)]
pub enum HostPortParseError {
    #[error("is a URI, only host:port strings are supported")]
    UriUnsupported,
    #[error("empty string")]
    EmptyString,
    #[error("invalid port: {0}")]
    InvalidPort(String),
    #[error("invalid string")]
    InvalidString,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use crate::host_port_pair::HostPortPair;
    use crate::host_port_pair::HostPortParseError::{
        EmptyString, InvalidPort, InvalidString, UriUnsupported,
    };

    #[test]
    fn test_proxy_address_parsing() {
        check_cases(
            [
                Case {
                    scenario: "host and port",
                    input: "proxyhost:1234",
                    expect: Yields(HostPortPair::HostAndPort("proxyhost".to_string(), 1234)),
                },
                Case {
                    scenario: "host only, no port",
                    input: "proxyhost",
                    expect: Yields(HostPortPair::HostOnly("proxyhost".to_string())),
                },
                Case {
                    scenario: "port only, no host",
                    input: ":1234",
                    expect: Yields(HostPortPair::PortOnly(1234)),
                },
                Case {
                    scenario: "empty port keeps the offending text in the error",
                    input: "proxyhost:",
                    expect: FailsWith(InvalidPort("".to_string())),
                },
                Case {
                    scenario: "non-numeric port keeps the offending text in the error",
                    input: "proxyhost:notaport",
                    expect: FailsWith(InvalidPort("notaport".to_string())),
                },
                Case {
                    scenario: "empty string",
                    input: "",
                    expect: FailsWith(EmptyString),
                },
                Case {
                    scenario: "a URI with a bad port is rejected as a URI, not a bad port",
                    input: "https://proxyhost:notaport",
                    expect: FailsWith(UriUnsupported),
                },
                Case {
                    scenario: "a URI with a good port is still rejected",
                    input: "https://proxyhost:1234",
                    expect: FailsWith(UriUnsupported),
                },
                Case {
                    scenario: "a URI with no port is still rejected",
                    input: "https://proxyhost",
                    expect: FailsWith(UriUnsupported),
                },
                Case {
                    scenario: "bracketed IPv6 host and port",
                    input: "[2001:db8::1]:8443",
                    expect: Yields(HostPortPair::HostAndPort("2001:db8::1".to_string(), 8443)),
                },
                Case {
                    scenario: "bracketed IPv6 host only",
                    input: "[2001:db8::1]",
                    expect: Yields(HostPortPair::HostOnly("2001:db8::1".to_string())),
                },
                Case {
                    scenario: "bare IPv6 literal is a host with no port",
                    input: "2001:db8::1",
                    expect: Yields(HostPortPair::HostOnly("2001:db8::1".to_string())),
                },
                Case {
                    scenario: "bare IPv6 loopback is a host with no port",
                    input: "::1",
                    expect: Yields(HostPortPair::HostOnly("::1".to_string())),
                },
                Case {
                    scenario: "bracketed IPv6 with empty port keeps the offending text",
                    input: "[2001:db8::1]:",
                    expect: FailsWith(InvalidPort("".to_string())),
                },
                Case {
                    scenario: "bracketed IPv6 with non-numeric port keeps the offending text",
                    input: "[2001:db8::1]:notaport",
                    expect: FailsWith(InvalidPort("notaport".to_string())),
                },
                Case {
                    scenario: "unclosed bracket is rejected",
                    input: "[2001:db8::1",
                    expect: FailsWith(InvalidString),
                },
                Case {
                    scenario: "empty brackets are rejected",
                    input: "[]",
                    expect: FailsWith(InvalidString),
                },
            ],
            HostPortPair::from_str,
        );
    }

    /// An IPv6 host must survive a `Display` -> `from_str` round trip. Before
    /// brackets were emitted, `HostAndPort("2001:db8::1", 443)` displayed as the
    /// ambiguous `2001:db8::1:443`, which then failed to parse back.
    #[test]
    fn test_ipv6_display_round_trips() {
        let cases = [
            (
                HostPortPair::HostAndPort("2001:db8::1".to_string(), 443),
                "[2001:db8::1]:443",
            ),
            (
                HostPortPair::HostOnly("2001:db8::1".to_string()),
                "2001:db8::1",
            ),
            (
                HostPortPair::HostAndPort("proxyhost".to_string(), 1234),
                "proxyhost:1234",
            ),
            (
                HostPortPair::HostAndPort("10.0.0.1".to_string(), 443),
                "10.0.0.1:443",
            ),
        ];
        for (pair, rendered) in cases {
            assert_eq!(pair.to_string(), rendered, "Display of {pair:?}");
            assert_eq!(
                HostPortPair::from_str(rendered),
                Ok(pair.clone()),
                "round trip of {rendered:?}"
            );
        }
    }

    /// `url_host` brackets bare IPv6 literal hosts (which are stored
    /// unbracketed) so they can be interpolated into a URL authority;
    /// hostnames and IPv4 literals pass through unchanged.
    #[test]
    fn test_url_host_brackets_ipv6() {
        let cases = [
            (
                HostPortPair::HostAndPort("2001:db8::1".to_string(), 443),
                Some("[2001:db8::1]"),
            ),
            (
                HostPortPair::HostOnly("2001:db8::1".to_string()),
                Some("[2001:db8::1]"),
            ),
            (
                HostPortPair::HostOnly("proxyhost".to_string()),
                Some("proxyhost"),
            ),
            (
                HostPortPair::HostAndPort("10.0.0.1".to_string(), 443),
                Some("10.0.0.1"),
            ),
            (HostPortPair::PortOnly(8443), None),
        ];
        for (pair, expected) in cases {
            assert_eq!(pair.url_host().as_deref(), expected, "url_host of {pair:?}");
        }
    }
}
