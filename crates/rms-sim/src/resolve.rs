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

use crate::inventory::{SimNode, normalize_mac};
use crate::rms;

/// One node from a request, with whatever hardware it matched.
pub(crate) struct NodeRef {
    /// Echoed verbatim: the caller correlates responses by this.
    pub(crate) node_id: String,
    #[allow(dead_code)]
    pub(crate) rack_id: String,
    pub(crate) node: Option<SimNode>,
}

impl NodeRef {
    pub(crate) fn matched(&self) -> bool {
        self.node.is_some()
    }
}

/// Resolve every node in a request against a snapshot of the inventory.
///
/// Unmatched entries are kept rather than dropped, so that responses stay
/// aligned with the request and the caller is told which nodes were not found
/// instead of silently receiving a shorter list.
pub(crate) fn resolve_nodes(inventory: &[SimNode], nodes: Option<&rms::NodeSet>) -> Vec<NodeRef> {
    let Some(node_set) = nodes else {
        return Vec::new();
    };

    node_set
        .nodes
        .iter()
        .map(|requested| NodeRef {
            node_id: requested.node_id.clone(),
            rack_id: requested.rack_id.clone(),
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
/// Stored MACs are already normalised (see `SimNode::bmc_mac`), so only the
/// requested one is normalised here.
fn match_node(inventory: &[SimNode], requested: &rms::NodeInfo) -> Option<SimNode> {
    let bmc = requested
        .bmc_endpoint
        .as_ref()
        .and_then(|e| e.interface.as_ref());
    let host = requested
        .host_endpoint
        .as_ref()
        .and_then(|e| e.interface.as_ref());

    if let Some(mac) = requested_mac(bmc)
        && let Some(found) = inventory
            .iter()
            .find(|n| n.bmc_mac.as_deref() == Some(&*mac))
    {
        return Some(found.clone());
    }

    if let Some(mac) = requested_mac(host)
        && let Some(found) = inventory
            .iter()
            .find(|n| n.host_mac.as_deref() == Some(&*mac))
    {
        return Some(found.clone());
    }

    if let Some(ip) = requested_ip(bmc)
        && let Some(found) = inventory.iter().find(|n| n.bmc_ip.as_deref() == Some(ip))
    {
        return Some(found.clone());
    }

    if let Some(ip) = requested_ip(host)
        && let Some(found) = inventory.iter().find(|n| n.host_ip.as_deref() == Some(ip))
    {
        return Some(found.clone());
    }

    None
}

/// The normalised MAC of a requested interface, if it names one.
fn requested_mac(interface: Option<&rms::NetworkInterface>) -> Option<String> {
    interface
        .map(|i| normalize_mac(&i.mac_address))
        .filter(|m| !m.is_empty())
}

/// The address of a requested interface, if it names one.
fn requested_ip(interface: Option<&rms::NetworkInterface>) -> Option<&str> {
    interface
        .map(|i| i.ip_address.as_str())
        .filter(|ip| !ip.is_empty())
}

#[cfg(test)]
mod tests {
    use super::match_node;
    use crate::inventory::SimNode;
    use crate::rms;

    fn endpoint(mac: &str, ip: &str) -> Option<rms::Endpoint> {
        Some(rms::Endpoint {
            interface: Some(rms::NetworkInterface {
                ip_address: ip.to_string(),
                mac_address: mac.to_string(),
                host_name: None,
            }),
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
                bmc_mac: Some("0200aaaaaaaa".to_string()),
                bmc_ip: Some("10.0.0.1".to_string()),
                host_mac: Some("0200bbbbbbbb".to_string()),
                host_ip: Some("10.0.1.1".to_string()),
                slot_number: Some(1),
                ..SimNode::default()
            },
            SimNode {
                bmc_mac: Some("0200cccccccc".to_string()),
                bmc_ip: Some("10.0.0.2".to_string()),
                host_mac: Some("0200dddddddd".to_string()),
                host_ip: Some("10.0.1.2".to_string()),
                slot_number: Some(2),
                ..SimNode::default()
            },
        ]
    }

    fn slot(found: Option<SimNode>) -> Option<u32> {
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
}
