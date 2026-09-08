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

//! Reverse-DNS (`in-addr.arpa` / `ip6.arpa`) name parsing.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnetwork::IpNetwork;

/// Convert a reverse-DNS name into the address range it stands for.
///
/// Each label under `in-addr.arpa` is one octet and each label under
/// `ip6.arpa` is one nibble, so the number of labels sets the prefix length:
///
/// - `in-addr.arpa` → `0.0.0.0/0`
/// - `2.0.192.in-addr.arpa` → `192.0.2.0/24`
/// - `1.2.0.192.in-addr.arpa` → `192.0.2.1/32`
/// - `8.b.d.0.1.0.0.2.ip6.arpa` → `2001:db8::/32`
///
/// A partial name is accepted. Queries at those names are normal: a resolver
/// doing qname minimisation asks for them on the way down to the full address.
/// Use [`arpa_qname_to_ip`] when only a complete host address is meaningful.
///
/// Case is ignored and one trailing root dot is accepted. Returns `None` if the
/// name is not under `in-addr.arpa` / `ip6.arpa`, ends in a second dot (an
/// empty label), has too many labels, or has a label that is not a valid
/// decimal octet or hex nibble.
pub fn arpa_qname_to_prefix(qname: &str) -> Option<IpNetwork> {
    // One root dot is presentation form. A second one is an empty label.
    let name = qname.strip_suffix('.').unwrap_or(qname);
    if name.ends_with('.') {
        return None;
    }
    let name = name.to_ascii_lowercase();

    if name == "in-addr.arpa" {
        return IpNetwork::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).ok();
    }
    if name == "ip6.arpa" {
        return IpNetwork::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).ok();
    }

    if let Some(reversed) = name.strip_suffix(".in-addr.arpa") {
        // Decimal octets, least-significant label first.
        let octets: Vec<&str> = reversed.split('.').collect();
        if octets.len() > 4 {
            return None;
        }
        let mut addr = [0u8; 4];
        for (byte, octet) in addr.iter_mut().zip(octets.iter().rev()) {
            *byte = octet.parse().ok()?;
        }
        let prefix_len = (octets.len() * 8) as u8;
        IpNetwork::new(IpAddr::V4(Ipv4Addr::from(addr)), prefix_len).ok()
    } else if let Some(reversed) = name.strip_suffix(".ip6.arpa") {
        // Hex nibbles, least-significant label first.
        let nibbles: Vec<&str> = reversed.split('.').collect();
        if nibbles.len() > 32 {
            return None;
        }
        let mut addr = [0u8; 16];
        for (i, nibble) in nibbles.iter().rev().enumerate() {
            if nibble.len() != 1 {
                return None;
            }
            let value = u8::from_str_radix(nibble, 16).ok()?;
            if i % 2 == 0 {
                addr[i / 2] = value << 4;
            } else {
                addr[i / 2] |= value;
            }
        }
        let prefix_len = (nibbles.len() * 4) as u8;
        IpNetwork::new(IpAddr::V6(Ipv6Addr::from(addr)), prefix_len).ok()
    } else {
        None
    }
}

/// Parse a reverse-DNS (PTR) query name into the host address it points at.
///
/// Only a complete name (four octets or thirty-two nibbles) is an address;
/// anything shorter names a prefix, not a host, and yields `None`. See
/// [`arpa_qname_to_prefix`] for the accepted syntax.
pub fn arpa_qname_to_ip(qname: &str) -> Option<IpAddr> {
    arpa_qname_to_prefix(qname)
        .filter(|network| network.prefix() == host_prefix_len(network))
        .map(|network| network.ip())
}

fn host_prefix_len(network: &IpNetwork) -> u8 {
    match network {
        IpNetwork::V4(_) => 32,
        IpNetwork::V6(_) => 128,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn parses_arpa_qname_to_ip() {
        value_scenarios!(
            run = arpa_qname_to_ip;
            "ipv4 in-addr.arpa" {
                "1.0.168.192.in-addr.arpa." => Some(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))),
                "3.2.1.10.in-addr.arpa." => Some(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))),
            }
            "ipv6 ip6.arpa" {
                "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.b.d.0.1.0.0.2.ip6.arpa."
                    => Some("2001:db8::1".parse::<IpAddr>().unwrap()),
                "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.ip6.arpa."
                    => Some("::1".parse::<IpAddr>().unwrap()),
            }
            "rejects non-arpa, partial, and malformed" {
                "host.example.com." => None,
                "in-addr.arpa." => None,
                "1.2.3.in-addr.arpa." => None,
                "300.0.0.0.in-addr.arpa." => None,
                "1.0.168.192.in-addr.arpa.extra." => None,
            }
            "normalizes case" {
                "1.0.168.192.IN-ADDR.ARPA." => Some(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1))),
                "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.B.D.0.1.0.0.2.IP6.ARPA."
                    => Some("2001:db8::1".parse::<IpAddr>().unwrap()),
            }
        );
    }

    #[test]
    fn parses_partial_arpa_qname_to_prefix() {
        let net = |cidr: &str| Some(cidr.parse::<IpNetwork>().unwrap());
        value_scenarios!(
            run = arpa_qname_to_prefix;
            "intermediate reverse names cover a prefix" {
                "2.0.192.in-addr.arpa." => net("192.0.2.0/24"),
                "10.in-addr.arpa." => net("10.0.0.0/8"),
                "8.b.d.0.1.0.0.2.ip6.arpa." => net("2001:db8::/32"),
            }
            "the arpa roots cover everything" {
                "in-addr.arpa." => net("0.0.0.0/0"),
                "IP6.ARPA" => net("::/0"),
            }
            "a full address is a host prefix" {
                "1.2.0.192.in-addr.arpa." => net("192.0.2.1/32"),
            }
            "rejects non-arpa and malformed" {
                "host.example.com." => None,
                "300.0.192.in-addr.arpa." => None,
                "1.1.2.0.192.in-addr.arpa." => None,
                "zz.ip6.arpa." => None,
                "2.0.192.in-addr.arpa.." => None,
            }
        );
    }
}
