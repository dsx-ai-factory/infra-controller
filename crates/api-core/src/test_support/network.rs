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

use ipnetwork::IpNetwork;
use lazy_static::lazy_static;

use crate::ethernet_virtualization::{EthVirtData, SiteFabricPrefixList};
use crate::test_support::network_segment::{
    FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY, FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS,
    FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY,
};

/// The datacenter-level DHCP relay that is assumed for all DPU discovery
///
/// For integration testing this must match a prefix defined in fixtures/create_network_segment.sql
/// In production the relay IP is a MetalLB VIP so isn't in a network segment.
pub const FIXTURE_DHCP_RELAY_ADDRESS: &str = "192.0.2.1";

// The site fabric prefixes list that the tests run with. Double check against
// the test logic before changing it, as at least one test relies on this list
// _excluding_ certain address space.
lazy_static! {
    pub(crate) static ref TEST_SITE_PREFIXES: Vec<IpNetwork> = vec![
        IpNetwork::new(
            FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.network(),
            FIXTURE_ADMIN_NETWORK_SEGMENT_GATEWAY.prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.network(),
            FIXTURE_UNDERLAY_NETWORK_SEGMENT_GATEWAY.prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[2].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[2].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[3].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[3].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[4].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[4].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[5].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[5].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[6].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[6].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[7].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[7].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[8].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[8].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[9].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[9].prefix(),
        )
        .unwrap(),
        IpNetwork::new(
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[10].network(),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[10].prefix(),
        )
        .unwrap(),
    ];
}

pub(crate) fn default_test_eth_virt_data() -> EthVirtData {
    let site_fabric_networks = TEST_SITE_PREFIXES.iter().copied().collect::<Vec<_>>();
    let site_fabric_prefixes = { SiteFabricPrefixList::from_ipnetwork_vec(site_fabric_networks) };
    EthVirtData {
        asn: 65535,
        dhcp_servers: vec![FIXTURE_DHCP_RELAY_ADDRESS.parse().unwrap()],
        deny_prefixes: vec![],
        site_fabric_prefixes,
    }
}
