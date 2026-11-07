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

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value as JsonValue;

/// BGP neighbor collection response data, keyed by peer name.
pub type BgpNeighbors = BTreeMap<String, BgpPeerInfo>;

/// BGP VRF response data.
/// Corresponds to `#/x-defs/cue-show-schema-bgp-vrf-bgp`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BgpVrfInfo {
    // #/x-defs/schema-feature-isa-feature-default-off-config
    pub enable: Option<JsonValue>,

    // #/x-defs/schema-bgp-vrf-bgp-config
    pub autonomous_system: Option<JsonValue>,
    pub router_id: Option<JsonValue>,
    pub rd: Option<JsonValue>,

    // #/x-defs/cue-show-schema-bgp-vrf-bgp-config-children
    pub address_family: Option<JsonValue>,
    pub neighbor: Option<BgpNeighbors>,
    pub peer_group: Option<JsonValue>,
    pub path_selection: Option<JsonValue>,
    pub route_reflection: Option<JsonValue>,
    pub route_export: Option<JsonValue>,
    pub route_import: Option<JsonValue>,
    pub timers: Option<JsonValue>,
    pub confederation: Option<JsonValue>,
    pub dynamic_neighbor: Option<JsonValue>,

    // #/x-defs/schema-bgp-vrf-bgp-show
    pub configured_neighbors: Option<JsonValue>,
    pub established_neighbors: Option<JsonValue>,

    // #/x-defs/cue-show-schema-bgp-vrf-bgp-show-children
    pub nexthop: Option<JsonValue>,

    // #/x-defs/schema-bgp-bgp-action-children
    #[serde(rename = "@clear")]
    pub clear: Option<JsonValue>,
    #[serde(rename = "in")]
    pub in_: Option<JsonValue>,
    pub out: Option<JsonValue>,
    pub soft: Option<JsonValue>,
}

/// BGP peer response data.
/// Corresponds to `#/x-defs/cue-show-schema-bgp-peer-peer`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BgpPeerInfo {
    // #/x-defs/schema-bgp-peer-peer-common-config
    pub password: Option<JsonValue>,
    pub enforce_first_as: Option<JsonValue>,
    pub passive_mode: Option<JsonValue>,
    pub nexthop_connected_check: Option<JsonValue>,
    pub multihop_ttl: Option<JsonValue>,
    pub description: Option<JsonValue>,
    pub shutdown: Option<JsonValue>,
    pub update_source: Option<JsonValue>,

    // #/x-defs/cue-show-schema-bgp-peer-peer-common-config-children
    pub bfd: Option<JsonValue>,
    pub ttl_security: Option<JsonValue>,
    pub local_as: Option<JsonValue>,
    pub timers: Option<JsonValue>,

    // #/x-defs/cue-show-schema-bgp-peer-peer-address-family-config-children
    pub address_family: Option<BgpPeerAddressFamilyInfo>,

    // #/x-defs/schema-feature-isa-feature-default-on-config
    pub enable: Option<JsonValue>,

    // #/x-defs/schema-bgp-peer-peer-config
    #[serde(rename = "type")]
    pub peer_type: Option<JsonValue>,
    pub peer_group: Option<String>,
    pub remote_as: Option<JsonValue>,
    pub graceful_shutdown: Option<JsonValue>,

    // #/x-defs/cue-show-schema-bgp-peer-peer-config-children
    pub capabilities: Option<JsonValue>,
    pub graceful_restart: Option<JsonValue>,

    // #/x-defs/schema-bgp-peer-peer-show
    pub local_hostname: Option<JsonValue>,
    pub local_domain: Option<JsonValue>,
    pub remote_hostname: Option<JsonValue>,
    pub remote_domain: Option<JsonValue>,
    pub bgp_version: Option<JsonValue>,
    pub remote_router_id: Option<JsonValue>,
    pub state: Option<BgpPeerState>,
    pub uptime: Option<JsonValue>,
    pub uptime_msec: Option<JsonValue>,
    pub connection_type: Option<JsonValue>,
    pub connections_established: Option<JsonValue>,
    pub connections_dropped: Option<JsonValue>,
    pub last_reset_timer: Option<JsonValue>,
    pub last_reset_reason: Option<JsonValue>,
    pub last_reset_code: Option<JsonValue>,
    pub local_ip: Option<JsonValue>,
    pub remote_ip: Option<JsonValue>,
    pub local_port: Option<JsonValue>,
    pub remote_port: Option<JsonValue>,

    // #/x-defs/cue-show-schema-bgp-peer-peer-show-children
    pub nexthop: Option<JsonValue>,
    pub ebgp_policy: Option<JsonValue>,
    pub message_stats: Option<JsonValue>,

    // #/x-defs/schema-bgp-peer-peer-action-children
    #[serde(rename = "@clear")]
    pub clear: Option<JsonValue>,
    #[serde(rename = "in")]
    pub in_: Option<JsonValue>,
    pub out: Option<JsonValue>,
    pub soft: Option<JsonValue>,
}

/// BGP peer address-family response data.
/// Corresponds to `#/x-defs/cue-show-schema-bgp-peer-address-family-address-family`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BgpPeerAddressFamilyInfo {
    // #/x-defs/cue-show-schema-bgp-peer-address-family-address-family-config-children
    pub ipv4_unicast: Option<JsonValue>,
    pub ipv6_unicast: Option<JsonValue>,
    pub l2vpn_evpn: Option<JsonValue>,
}

/// BGP peer operational state.
/// Corresponds to the `state` field from `#/x-defs/schema-bgp-peer-peer-show`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BgpPeerState {
    Idle,
    Connect,
    Active,
    OpenSent,
    OpenConfirm,
    Established,
    Clearing,
    Deleted,
}

impl BgpPeerState {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connect => "connect",
            Self::Active => "active",
            Self::OpenSent => "opensent",
            Self::OpenConfirm => "openconfirm",
            Self::Established => "established",
            Self::Clearing => "clearing",
            Self::Deleted => "deleted",
        }
    }
}

impl std::fmt::Display for BgpPeerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_dynamic_neighbors_with_strongly_typed_fields() {
        let bgp: BgpVrfInfo = serde_json::from_str(
            r#"
            {
                "autonomous-system": 65100,
                "configured-neighbors": 1,
                "neighbor": {
                    "192.0.2.10": {
                        "state": "established",
                        "peer-group": "underlay-peers",
                        "address-family": {
                            "ipv4-unicast": { "enable": "on" },
                            "ipv6-unicast": null,
                            "l2vpn-evpn": { "enable": "off" }
                        },
                        "enforce-first-as": "off",
                        "connections-established": 4
                    }
                }
            }
            "#,
        )
        .expect("BGP VRF info should deserialize");

        assert_eq!(bgp.autonomous_system, Some(JsonValue::from(65100)));
        assert_eq!(bgp.configured_neighbors, Some(JsonValue::from(1)));

        let neighbors = bgp.neighbor.expect("neighbor map should deserialize");
        let peer = neighbors
            .get("192.0.2.10")
            .expect("dynamic peer ID should be preserved");
        assert_eq!(peer.state, Some(BgpPeerState::Established));
        assert_eq!(peer.peer_group.as_deref(), Some("underlay-peers"));
        assert_eq!(peer.enforce_first_as, Some(JsonValue::from("off")));
        assert_eq!(peer.connections_established, Some(JsonValue::from(4)));

        let address_family = peer
            .address_family
            .as_ref()
            .expect("peer address-family should deserialize");
        assert!(address_family.ipv4_unicast.is_some());
        assert!(address_family.ipv6_unicast.is_none());
        assert!(address_family.l2vpn_evpn.is_some());
    }

    #[test]
    fn deserializes_special_field_names() {
        let bgp: BgpVrfInfo = serde_json::from_str(
            r#"
            {
                "@clear": { "state": "running" },
                "in": { "@clear": { "state": "queued" } },
                "out": {},
                "soft": {},
                "neighbor": {
                    "swp1": {
                        "@clear": { "state": "complete" },
                        "in": {},
                        "out": {},
                        "soft": {},
                        "type": "unnumbered"
                    }
                }
            }
            "#,
        )
        .expect("BGP VRF info should deserialize");

        assert_eq!(
            bgp.clear
                .as_ref()
                .and_then(|clear| clear.get("state"))
                .and_then(JsonValue::as_str),
            Some("running")
        );
        assert!(bgp.in_.is_some());
        assert!(bgp.out.is_some());
        assert!(bgp.soft.is_some());

        let peer = bgp
            .neighbor
            .as_ref()
            .and_then(|neighbors| neighbors.get("swp1"))
            .expect("peer should deserialize");
        assert!(peer.clear.is_some());
        assert!(peer.in_.is_some());
        assert!(peer.out.is_some());
        assert!(peer.soft.is_some());
        assert_eq!(peer.peer_type, Some(JsonValue::from("unnumbered")));
    }

    #[test]
    fn omitted_fields_deserialize_as_none() {
        let bgp: BgpVrfInfo = serde_json::from_str(r#"{}"#).expect("empty BGP info should parse");

        assert!(bgp.neighbor.is_none());
        assert!(bgp.autonomous_system.is_none());
        assert!(bgp.clear.is_none());
    }

    #[test]
    fn deserializes_known_peer_states() {
        struct Case {
            raw: &'static str,
            expected: BgpPeerState,
        }

        let cases = [
            Case {
                raw: "idle",
                expected: BgpPeerState::Idle,
            },
            Case {
                raw: "connect",
                expected: BgpPeerState::Connect,
            },
            Case {
                raw: "active",
                expected: BgpPeerState::Active,
            },
            Case {
                raw: "opensent",
                expected: BgpPeerState::OpenSent,
            },
            Case {
                raw: "openconfirm",
                expected: BgpPeerState::OpenConfirm,
            },
            Case {
                raw: "established",
                expected: BgpPeerState::Established,
            },
            Case {
                raw: "clearing",
                expected: BgpPeerState::Clearing,
            },
            Case {
                raw: "deleted",
                expected: BgpPeerState::Deleted,
            },
        ];

        for case in cases {
            let peer: BgpPeerInfo =
                serde_json::from_str(&format!(r#"{{ "state": "{}" }}"#, case.raw))
                    .expect("peer should deserialize");

            let state = peer.state.expect("state should deserialize");
            assert_eq!(state, case.expected);
            assert_eq!(state.to_string(), case.raw);
        }
    }

    #[test]
    fn rejects_unknown_peer_state() {
        let error = serde_json::from_str::<BgpPeerInfo>(r#"{ "state": "future-state" }"#)
            .expect_err("unknown peer state should fail deserialization");

        assert!(
            error.to_string().contains("unknown variant"),
            "unexpected error: {error}"
        );
    }
}
