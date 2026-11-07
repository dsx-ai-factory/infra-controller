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
/// A representation of an address family, which makes certain APIs more
/// composable if we can construct this as a type.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IpAddressFamily {
    Ipv4,
    Ipv6,
}

impl IpAddressFamily {
    /// Returns the prefix length for a single interface address in this family
    /// (32 for IPv4, 128 for IPv6).
    pub const fn interface_prefix_len(self) -> u8 {
        match self {
            IpAddressFamily::Ipv4 => 32,
            IpAddressFamily::Ipv6 => 128,
        }
    }

    /// pg_family returns the Postgre `family()` integer for
    /// this address family (4 for IPv4, 6 for IPv6).
    pub const fn pg_family(self) -> i32 {
        match self {
            IpAddressFamily::Ipv4 => 4,
            IpAddressFamily::Ipv6 => 6,
        }
    }
}

pub trait IdentifyAddressFamily {
    /// Return the address family for this value.
    fn address_family(&self) -> IpAddressFamily;

    /// Check whether this value matches the specified `address_family`.
    fn is_address_family(&self, address_family: IpAddressFamily) -> bool {
        address_family == self.address_family()
    }

    fn require_address_family_or_else<F, E>(
        self,
        address_family: IpAddressFamily,
        err: F,
    ) -> Result<Self, E>
    where
        Self: Sized,
        F: FnOnce(Self) -> E,
    {
        match self.is_address_family(address_family) {
            true => Ok(self),
            false => Err(err(self)),
        }
    }
}

impl IdentifyAddressFamily for std::net::IpAddr {
    fn address_family(&self) -> IpAddressFamily {
        use IpAddressFamily::*;
        match self {
            std::net::IpAddr::V4(_) => Ipv4,
            std::net::IpAddr::V6(_) => Ipv6,
        }
    }
}

impl IdentifyAddressFamily for ipnet::IpNet {
    fn address_family(&self) -> IpAddressFamily {
        use IpAddressFamily::*;
        match self {
            ipnet::IpNet::V4(_) => Ipv4,
            ipnet::IpNet::V6(_) => Ipv6,
        }
    }
}

#[cfg(feature = "ipnetwork")]
impl IdentifyAddressFamily for ipnetwork::IpNetwork {
    fn address_family(&self) -> IpAddressFamily {
        use IpAddressFamily::*;
        match self {
            ipnetwork::IpNetwork::V4(_) => Ipv4,
            ipnetwork::IpNetwork::V6(_) => Ipv6,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, scenarios, value_scenarios};
    use ipnet::IpNet;

    use super::*;

    #[test]
    fn test_interface_prefix_len() {
        value_scenarios!(
            run = |family| family.interface_prefix_len();
            "ipv4 is /32" {
                IpAddressFamily::Ipv4 => 32,
            }

            "ipv6 is /128" {
                IpAddressFamily::Ipv6 => 128,
            }
        );
    }

    #[test]
    fn test_pg_family() {
        value_scenarios!(
            run = |family| family.pg_family();
            "ipv4 is postgres family 4" {
                IpAddressFamily::Ipv4 => 4,
            }

            "ipv6 is postgres family 6" {
                IpAddressFamily::Ipv6 => 6,
            }
        );
    }

    #[test]
    fn test_ipaddr_address_family() {
        value_scenarios!(
            run = |s| s.parse::<IpAddr>().unwrap().address_family();
            "ipv4 loopback" {
                "127.0.0.1" => IpAddressFamily::Ipv4,
            }

            "ipv4 unspecified" {
                "0.0.0.0" => IpAddressFamily::Ipv4,
            }

            "ipv4 broadcast" {
                "255.255.255.255" => IpAddressFamily::Ipv4,
            }

            "ipv4 routable" {
                "10.0.0.1" => IpAddressFamily::Ipv4,
            }

            "ipv6 loopback" {
                "::1" => IpAddressFamily::Ipv6,
            }

            "ipv6 unspecified" {
                "::" => IpAddressFamily::Ipv6,
            }

            "ipv6 unique-local" {
                "fd00::1" => IpAddressFamily::Ipv6,
            }

            "ipv6 link-local" {
                "fe80::1" => IpAddressFamily::Ipv6,
            }

            "ipv4-mapped ipv6 stays ipv6" {
                "::ffff:192.0.2.1" => IpAddressFamily::Ipv6,
            }
        );
    }

    #[test]
    fn test_ipnet_address_family() {
        value_scenarios!(
            run = |s| s.parse::<IpNet>().unwrap().address_family();
            "ipv4 host route" {
                "10.0.0.1/32" => IpAddressFamily::Ipv4,
            }

            "ipv4 default route" {
                "0.0.0.0/0" => IpAddressFamily::Ipv4,
            }

            "ipv4 subnet" {
                "192.168.0.0/24" => IpAddressFamily::Ipv4,
            }

            "ipv6 host route" {
                "fd00::1/128" => IpAddressFamily::Ipv6,
            }

            "ipv6 default route" {
                "::/0" => IpAddressFamily::Ipv6,
            }

            "ipv6 subnet" {
                "2001:db8::/64" => IpAddressFamily::Ipv6,
            }
        );
    }

    #[test]
    fn test_is_address_family() {
        struct Row {
            value: &'static str,
            family: IpAddressFamily,
        }

        value_scenarios!(
            run = |row| {
                row.value
                    .parse::<IpAddr>()
                    .unwrap()
                    .is_address_family(row.family)
            };
            "ipv4 matches ipv4" {
                Row {
                    value: "10.0.0.1",
                    family: IpAddressFamily::Ipv4,
                } => true,
            }

            "ipv4 does not match ipv6" {
                Row {
                    value: "10.0.0.1",
                    family: IpAddressFamily::Ipv6,
                } => false,
            }

            "ipv6 matches ipv6" {
                Row {
                    value: "fd00::1",
                    family: IpAddressFamily::Ipv6,
                } => true,
            }

            "ipv6 does not match ipv4" {
                Row {
                    value: "fd00::1",
                    family: IpAddressFamily::Ipv4,
                } => false,
            }
        );
    }

    #[test]
    fn test_require_address_family_or_else() {
        struct Row {
            value: &'static str,
            required: IpAddressFamily,
        }

        scenarios!(
            run = |row| {
                let addr = row.value.parse::<IpAddr>().unwrap();
                addr.require_address_family_or_else(row.required, |_| ())
            };
            "ipv4 required and present yields the address" {
                Row {
                    value: "127.0.0.1",
                    required: IpAddressFamily::Ipv4,
                } => Yields("127.0.0.1".parse::<IpAddr>().unwrap()),
            }

            "ipv6 required and present yields the address" {
                Row {
                    value: "fd00::1",
                    required: IpAddressFamily::Ipv6,
                } => Yields("fd00::1".parse::<IpAddr>().unwrap()),
            }

            "ipv6 required but ipv4 given fails" {
                Row {
                    value: "127.0.0.1",
                    required: IpAddressFamily::Ipv6,
                } => Fails,
            }

            "ipv4 required but ipv6 given fails" {
                Row {
                    value: "fd00::1",
                    required: IpAddressFamily::Ipv4,
                } => Fails,
            }
        );
    }

    #[test]
    fn test_require_address_family_or_else_passes_value_to_err() {
        // The error closure receives the rejected value; assert it is threaded
        // through so callers can fold the original address into their error.
        let addr: IpAddr = "127.0.0.1".parse().unwrap();
        Case {
            scenario: "rejected value reaches the error closure",
            input: addr,
            expect: FailsWith(addr),
        }
        .check(|addr| {
            addr.require_address_family_or_else(IpAddressFamily::Ipv6, |rejected| rejected)
        });
    }
}
