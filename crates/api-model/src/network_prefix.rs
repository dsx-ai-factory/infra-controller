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
use std::net::IpAddr;

use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use carbide_uuid::vpc::VpcPrefixId;
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPrefix {
    pub id: NetworkPrefixId,
    pub segment_id: NetworkSegmentId,
    pub prefix: IpNetwork,
    pub gateway: Option<IpAddr>,
    #[serde(default)]
    pub dhcpv6_link_address: Option<IpAddr>,
    pub num_reserved: i32,
    pub vpc_prefix_id: Option<VpcPrefixId>,
    pub vpc_prefix: Option<IpNetwork>,
    pub svi_ip: Option<IpAddr>,
    /// Exact free-address count populated when
    /// `NetworkSegmentSearchConfig::include_num_free_ips` is enabled.
    ///
    /// `None` means accounting was skipped; `Some(0)` is an exact zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_free_ips: Option<u128>,
}

#[derive(Debug)]
pub struct NewNetworkPrefix {
    pub prefix: IpNetwork,
    pub gateway: Option<IpAddr>,
    pub dhcpv6_link_address: Option<IpAddr>,
    pub num_reserved: i32,
}

impl From<NetworkPrefix> for NewNetworkPrefix {
    fn from(prefix: NetworkPrefix) -> Self {
        Self {
            prefix: prefix.prefix,
            gateway: prefix.gateway,
            dhcpv6_link_address: prefix.dhcpv6_link_address,
            num_reserved: prefix.num_reserved,
        }
    }
}

impl<'r> FromRow<'r, PgRow> for NetworkPrefix {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(NetworkPrefix {
            id: row.try_get("id")?,
            segment_id: row.try_get("segment_id")?,
            vpc_prefix_id: row.try_get("vpc_prefix_id")?,
            vpc_prefix: row.try_get("vpc_prefix")?,
            prefix: row.try_get("prefix")?,
            gateway: row.try_get("gateway")?,
            dhcpv6_link_address: row.try_get("dhcpv6_link_address")?,
            num_reserved: row.try_get("num_reserved")?,
            svi_ip: row.try_get("svi_ip")?,
            num_free_ips: None,
        })
    }
}

impl NetworkPrefix {
    /// `gateway_cidr` formats the configured gateway with this segment's
    /// prefix length.
    ///
    /// Consumers install the gateway on the segment, so a gateway of
    /// `192.0.2.1` in `192.0.2.0/24` becomes `192.0.2.1/24`, not the host
    /// route `192.0.2.1/32`. Returns `None` when no gateway is configured.
    pub fn gateway_cidr(&self) -> Option<String> {
        self.gateway
            .map(|g| format!("{}/{}", g, self.prefix.prefix()))
    }

    /// `smells_like_fnn` recognizes narrow segment prefixes generated from a
    /// VPC prefix when the persisted `can_stretch` value is unavailable.
    ///
    /// This compatibility fallback recognizes legacy FNN `/31` IPv4 and `/127`
    /// IPv6 linknets. It starts one bit wider (`/30` or `/126`) to preserve the
    /// existing IPv4 tolerance, but only matches rows associated with a
    /// `VpcPrefixId`. New generated segments, including SLAAC `/64` prefixes,
    /// persist `can_stretch` explicitly and do not depend on this heuristic.
    pub fn smells_like_fnn(&self) -> bool {
        self.vpc_prefix_id.is_some()
            && match self.prefix {
                IpNetwork::V4(v4) => v4.prefix() >= 30,
                IpNetwork::V6(v6) => v6.prefix() >= 126,
            }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// Builds the smallest complete prefix needed to exercise model helpers.
    fn network_prefix(
        prefix: &str,
        gateway: Option<&str>,
        associated_with_vpc_prefix: bool,
    ) -> NetworkPrefix {
        NetworkPrefix {
            id: NetworkPrefixId::new(),
            segment_id: NetworkSegmentId::new(),
            prefix: prefix.parse().unwrap(),
            gateway: gateway.map(|gateway| gateway.parse().unwrap()),
            dhcpv6_link_address: None,
            num_reserved: 0,
            vpc_prefix_id: associated_with_vpc_prefix.then(VpcPrefixId::new),
            vpc_prefix: None,
            svi_ip: None,
            num_free_ips: None,
        }
    }

    #[test]
    fn gateway_cidr_uses_the_segment_prefix_length() {
        value_scenarios!(run = |prefix: NetworkPrefix| prefix.gateway_cidr();
            "configured gateways" {
                network_prefix("192.0.2.0/24", Some("192.0.2.1"), false)
                    => Some("192.0.2.1/24".to_string()),
                network_prefix("2001:db8::/64", Some("2001:db8::1"), false)
                    => Some("2001:db8::1/64".to_string()),
            }

            "gateway is not configured" {
                network_prefix("192.0.2.0/24", None, false) => None,
            }
        );
    }

    #[test]
    fn smells_like_fnn_requires_a_vpc_prefix_and_narrow_linknet() {
        value_scenarios!(run = |prefix: NetworkPrefix| prefix.smells_like_fnn();
            "IPv4 cutoff" {
                network_prefix("192.0.2.0/29", None, true) => false,
                network_prefix("192.0.2.0/30", None, true) => true,
                network_prefix("192.0.2.0/31", None, true) => true,
                network_prefix("192.0.2.1/32", None, true) => true,
            }

            "IPv6 cutoff" {
                network_prefix("2001:db8::/125", None, true) => false,
                network_prefix("2001:db8::/126", None, true) => true,
                network_prefix("2001:db8::/127", None, true) => true,
                network_prefix("2001:db8::1/128", None, true) => true,
            }

            "prefix is not associated with a VPC prefix" {
                network_prefix("192.0.2.0/31", None, false) => false,
                network_prefix("2001:db8::/127", None, false) => false,
            }
        );
    }
}
