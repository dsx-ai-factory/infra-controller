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

//! Matching the nodes in a request against simulated hardware.
//!
//! `node_id` is a carbide database row id. machine-a-tron has never seen it
//! and cannot derive it, so it is only ever echoed back, never interpreted.
//! Matching is by address instead, which both sides do know.

use std::net::IpAddr;
use std::str::FromStr;

use mac_address::MacAddress;

use crate::inventory::SimNode;
use crate::rms;

/// One node from a request, with whatever hardware it matched.
///
/// Borrows from the request and from the inventory snapshot rather than
/// copying either: a response only needs the id of each node, and copies it
/// at the point where it is built.
pub(crate) struct NodeRef<'a> {
    /// Echoed verbatim: the caller correlates responses by this.
    pub(crate) node_id: &'a str,
    pub(crate) node: Option<&'a SimNode>,
}

impl NodeRef<'_> {
    pub(crate) fn matched(&self) -> bool {
        self.node.is_some()
    }
}

/// Resolve every node in a request against a snapshot of the inventory.
///
/// Unmatched entries are kept rather than dropped, so that responses stay
/// aligned with the request and the caller is told which nodes were not found
/// instead of silently receiving a shorter list.
pub(crate) fn resolve_nodes<'a>(
    inventory: &'a [SimNode],
    nodes: Option<&'a rms::NodeSet>,
) -> Vec<NodeRef<'a>> {
    let Some(node_set) = nodes else {
        return Vec::new();
    };

    node_set
        .nodes
        .iter()
        .map(|requested| NodeRef {
            node_id: &requested.node_id,
            node: match_node(inventory, requested),
        })
        .collect()
}

/// Match one requested node, preferring the most specific identifier present.
///
/// BMC MAC first: it is the identifier every caller that has the BMC
/// populates and the one machine-a-tron assigns itself, so it is stable
/// across re-addressing. The host MAC comes next, because a switch password
/// rotation names the switch by its NVOS endpoint only. Addresses are the
/// last resort, since a simulated device may not have been given one yet.
///
/// Every identifier is parsed before anything is compared. An interface whose
/// MAC or address does not parse names no node, and such a node is not matched
/// on its other identifiers either: the caller learns about the bad identifier
/// from the batch failure instead of getting a placement for whatever shares
/// the rest.
fn match_node<'a>(inventory: &'a [SimNode], requested: &rms::NodeInfo) -> Option<&'a SimNode> {
    let bmc = requested
        .bmc_endpoint
        .as_ref()
        .and_then(|e| e.interface.as_ref());
    let host = requested
        .host_endpoint
        .as_ref()
        .and_then(|e| e.interface.as_ref());

    let bmc_mac = requested_mac(bmc)?;
    let host_mac = requested_mac(host)?;
    let bmc_ip = requested_ip(bmc)?;
    let host_ip = requested_ip(host)?;

    if let Some(mac) = bmc_mac
        && let Some(found) = inventory.iter().find(|n| n.bmc_mac == Some(mac))
    {
        return Some(found);
    }

    if let Some(mac) = host_mac
        && let Some(found) = inventory.iter().find(|n| n.host_mac == Some(mac))
    {
        return Some(found);
    }

    if let Some(ip) = bmc_ip
        && let Some(found) = inventory.iter().find(|n| n.bmc_ip == Some(ip))
    {
        return Some(found);
    }

    if let Some(ip) = host_ip
        && let Some(found) = inventory.iter().find(|n| n.host_ip == Some(ip))
    {
        return Some(found);
    }

    None
}

/// The MAC of a requested interface: `Some(None)` when it names none, `None`
/// when what it names is not a MAC.
fn requested_mac(interface: Option<&rms::NetworkInterface>) -> Option<Option<MacAddress>> {
    requested(interface.map(|i| i.mac_address.as_str()))
}

/// The address of a requested interface: `Some(None)` when it names none,
/// `None` when what it names is not an address.
fn requested_ip(interface: Option<&rms::NetworkInterface>) -> Option<Option<IpAddr>> {
    requested(interface.map(|i| i.ip_address.as_str()))
}

/// Parse an optional identifier at the boundary. Proto3 has no absent string,
/// so an empty one means "not given" rather than "given and wrong".
fn requested<T: FromStr>(field: Option<&str>) -> Option<Option<T>> {
    match field {
        None | Some("") => Some(None),
        Some(raw) => raw.parse().ok().map(Some),
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use mac_address::MacAddress;

    use super::{match_node, requested_ip, requested_mac};
    use crate::inventory::SimNode;
    use crate::rms;

    fn mac(s: &str) -> MacAddress {
        s.parse().unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn interface(mac: &str, ip: &str) -> rms::NetworkInterface {
        rms::NetworkInterface {
            ip_address: ip.to_string(),
            mac_address: mac.to_string(),
            host_name: None,
        }
    }

    fn endpoint(mac: &str, ip: &str) -> Option<rms::Endpoint> {
        Some(rms::Endpoint {
            interface: Some(interface(mac, ip)),
            port: 0,
            credentials: None,
        })
    }

    fn request(bmc: Option<rms::Endpoint>, host: Option<rms::Endpoint>) -> rms::NodeInfo {
        rms::NodeInfo {
            node_id: "n".to_string(),
            rack_id: "r".to_string(),
            r#type: None,
            bmc_endpoint: bmc,
            host_endpoint: host,
            node_descriptor: None,
        }
    }

    fn inventory() -> Vec<SimNode> {
        vec![
            SimNode {
                bmc_mac: Some(mac("02:00:aa:aa:aa:aa")),
                bmc_ip: Some(ip("10.0.0.1")),
                host_mac: Some(mac("02:00:bb:bb:bb:bb")),
                host_ip: Some(ip("10.0.1.1")),
                slot_number: Some(1),
                ..SimNode::default()
            },
            SimNode {
                bmc_mac: Some(mac("02:00:cc:cc:cc:cc")),
                bmc_ip: Some(ip("10.0.0.2")),
                host_mac: Some(mac("02:00:dd:dd:dd:dd")),
                host_ip: Some(ip("10.0.1.2")),
                slot_number: Some(2),
                ..SimNode::default()
            },
        ]
    }

    fn slot(found: Option<&SimNode>) -> Option<u32> {
        found.and_then(|n| n.slot_number)
    }

    #[test]
    fn the_bmc_mac_wins_whatever_form_it_is_sent_in() {
        let inv = inventory();
        // A BMC MAC that names node 2 beats a host MAC and addresses naming
        // node 1.
        let req = request(
            endpoint("02:00:CC:CC:CC:CC", "10.0.0.1"),
            endpoint("02-00-bb-bb-bb-bb", "10.0.1.1"),
        );
        assert_eq!(slot(match_node(&inv, &req)), Some(2));
    }

    #[test]
    fn a_host_only_request_matches_by_nvos_mac_then_by_address() {
        let inv = inventory();
        let by_mac = request(None, endpoint("02:00:DD:DD:DD:DD", ""));
        assert_eq!(slot(match_node(&inv, &by_mac)), Some(2));

        let by_ip = request(None, endpoint("", "10.0.1.1"));
        assert_eq!(slot(match_node(&inv, &by_ip)), Some(1));

        let by_bmc_ip = request(endpoint("", "10.0.0.2"), None);
        assert_eq!(slot(match_node(&inv, &by_bmc_ip)), Some(2));
    }

    #[test]
    fn nothing_matches_an_unknown_node_or_an_empty_request() {
        let inv = inventory();
        let unknown = request(endpoint("02:00:00:00:00:99", "10.9.9.9"), None);
        assert!(match_node(&inv, &unknown).is_none());
        assert!(match_node(&inv, &request(None, None)).is_none());
    }

    #[test]
    fn a_malformed_identifier_matches_nothing_even_when_another_would() {
        let inv = inventory();
        // Stripped of its punctuation this would be node 1's BMC MAC, and the
        // address is node 1's too.
        let bmc = request(endpoint("02:00:AA:AA:AA:AA!", "10.0.0.1"), None);
        assert!(match_node(&inv, &bmc).is_none());
        let host = request(None, endpoint("0200bbbbbbbb0", "10.0.1.1"));
        assert!(match_node(&inv, &host).is_none());
        // A good MAC does not rescue a bad address either.
        let ip = request(endpoint("02:00:AA:AA:AA:AA", "10.0.0.256"), None);
        assert!(match_node(&inv, &ip).is_none());
    }

    #[test]
    fn mac_spellings_parse_to_the_same_address() {
        let expected = MacAddress::new([0x02, 0x00, 0xab, 0xcd, 0x12, 0x34]);
        for spelling in ["02:00:AB:cd:12:34", "02-00-ab-CD-12-34", "0200abcd1234"] {
            assert_eq!(
                requested_mac(Some(&interface(spelling, ""))),
                Some(Some(expected)),
                "{spelling}"
            );
        }
        assert_eq!(requested_mac(Some(&interface("", ""))), Some(None));
        assert_eq!(requested_mac(None), Some(None));
    }

    #[test]
    fn anything_that_is_not_a_mac_is_rejected() {
        let near_misses = [
            ("02:00:AB:CD:12:34!", "trailing punctuation"),
            ("0200abcd1234!", "trailing punctuation, bare"),
            ("02:00:AB:CD:12:34:", "trailing separator"),
            (":02:00:AB:CD:12:34", "leading separator"),
            (" 0200abcd1234", "leading whitespace"),
            ("02:00:AB:CD:12", "too short"),
            ("02:00:AB:CD:12:3", "one digit short"),
            ("0200abcd123", "one digit short, bare"),
            ("02:00:AB:CD:12:34:56", "too long"),
            ("0200abcd123456", "too long, bare"),
            ("02.00.AB.CD.12.34", "unsupported separator"),
            ("0200.abcd.1234", "dotted"),
            ("02:00:AB:CD:12:3G", "non-hex digit"),
            ("0200abcd12g4", "non-hex digit, bare"),
            ("02:00:AB:CD:12:\u{e9}", "non-ascii, separated"),
            ("0200abcd1\u{e9}4", "non-ascii straddling an octet, bare"),
            ("020:0AB:CD:12:34", "octets of the wrong width"),
        ];
        for (input, why) in near_misses {
            assert_eq!(
                requested_mac(Some(&interface(input, ""))),
                None,
                "{why}: {input:?}"
            );
        }
    }

    #[test]
    fn addresses_parse_or_are_rejected() {
        assert_eq!(
            requested_ip(Some(&interface("", "10.0.0.1"))),
            Some(Some(ip("10.0.0.1")))
        );
        assert_eq!(
            requested_ip(Some(&interface("", "fd00::1"))),
            Some(Some(ip("fd00::1")))
        );
        assert_eq!(requested_ip(Some(&interface("", ""))), Some(None));
        assert_eq!(requested_ip(None), Some(None));
        for (input, why) in [
            ("10.0.0.256", "octet out of range"),
            ("10.0.0", "too few octets"),
            ("10.0.0.1 ", "trailing whitespace"),
            ("10.0.0.1:443", "address with a port"),
            ("node-1.example", "a host name"),
        ] {
            assert_eq!(requested_ip(Some(&interface("", input))), None, "{why}");
        }
    }
}
