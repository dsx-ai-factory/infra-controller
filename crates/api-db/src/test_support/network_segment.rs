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

use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{AllocationStrategy, NetworkSegmentType, NewNetworkSegment};

pub(crate) fn admin_segment(
    name: &str,
    prefix: &str,
    gateway: &str,
    num_reserved: i32,
) -> NewNetworkSegment {
    NewNetworkSegment {
        name: name.to_string(),
        subdomain_id: None,
        vpc_id: None,
        mtu: 1500,
        prefixes: vec![NewNetworkPrefix {
            prefix: prefix.parse().unwrap(),
            gateway: Some(gateway.parse().unwrap()),
            dhcpv6_link_address: None,
            num_reserved,
        }],
        vlan_id: None,
        vni: None,
        segment_type: NetworkSegmentType::Admin,
        id: uuid::Uuid::new_v4().into(),
        can_stretch: None,
        allocation_strategy: AllocationStrategy::Dynamic,
        infer_slaac_eui64_addresses: false,
    }
}
