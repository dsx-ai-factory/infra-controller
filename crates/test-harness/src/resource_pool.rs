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

use std::collections::HashMap;
use std::iter;

use ipnetwork::IpNetwork;
use model::resource_pool;
use model::resource_pool::{ResourcePoolDef, ResourcePoolType};

#[derive(Default)]
pub struct ResourcePoolBuilder {
    loopback_ip_v6: Option<IpNetwork>,
    vlan_id_ranges: Option<Vec<(u32, u32)>>,
    vni_ranges: Option<Vec<(u32, u32)>>,
}

impl ResourcePoolBuilder {
    pub fn with_loopback_ip_v6(self, addr: &str) -> Self {
        Self {
            loopback_ip_v6: Some(
                addr.parse()
                    .expect("correct IP address / mask must be provided"),
            ),
            ..self
        }
    }

    pub fn with_vlan_ids(mut self, start: u32, end: u32) -> Self {
        assert!(
            start <= end,
            "VLAN ID range start must be less than or equal to end"
        );
        self.vlan_id_ranges = Some(vec![(start, end)]);
        self
    }

    pub fn with_vnis(mut self, start: u32, end: u32) -> Self {
        assert!(
            start <= end,
            "VNI range start must be less than or equal to end"
        );
        self.vni_ranges = Some(vec![(start, end)]);
        self
    }

    pub fn build(self) -> HashMap<String, ResourcePoolDef> {
        let int_range_pool = |ranges: &[(u32, u32)]| resource_pool::ResourcePoolDef {
            pool_type: resource_pool::ResourcePoolType::Integer,
            ranges: ranges
                .iter()
                .map(|(start, end)| resource_pool::Range {
                    start: start.to_string(),
                    end: end.to_string(),
                    auto_assign: true,
                })
                .collect(),
            prefix: None,
            delegate_prefix_len: None,
        };
        let vlan_id_ranges = self.vlan_id_ranges.unwrap_or_else(|| vec![(1, 2)]);
        let vni_ranges = self.vni_ranges.unwrap_or_else(|| vec![(10001, 10002)]);

        [
            (
                resource_pool::common::LOOPBACK_IP.to_string(),
                resource_pool::ResourcePoolDef {
                    pool_type: resource_pool::ResourcePoolType::Ipv4,
                    prefix: Some("172.20.0.0/24".to_string()),
                    ranges: vec![],
                    delegate_prefix_len: None,
                },
            ),
            (
                resource_pool::common::VLANID.to_string(),
                int_range_pool(&vlan_id_ranges),
            ),
            (
                resource_pool::common::VNI.to_string(),
                int_range_pool(&vni_ranges),
            ),
            (
                resource_pool::common::VPC_VNI.to_string(),
                int_range_pool(&[(20001, 20002), (60001, 60002)]),
            ),
        ]
        .into_iter()
        .map(Some)
        .chain(iter::once(self.loopback_ip_v6.map(|network| {
            (
                resource_pool::common::LOOPBACK_IP_V6.to_string(),
                ResourcePoolDef {
                    pool_type: ResourcePoolType::Ipv6,
                    prefix: Some(network.to_string()),
                    ranges: vec![],
                    delegate_prefix_len: None,
                },
            )
        })))
        .flatten()
        .collect()
    }
}
