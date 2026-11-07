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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::HostMachineId;
use carbide_uuid::network::NetworkSegmentId;
use model::instance::config::network::{
    InstanceInterfaceConfig, InstanceNetworkConfig, InterfaceFunctionId,
};
use model::machine::HostMachine;
use model::network_segment::NetworkSegmentType;
use sqlx::PgConnection;

use crate::{DatabaseError, DatabaseResult};

/// Allocate IP's for this network config, filling the InstanceInterfaceConfigs with the newly
/// allocated IP's.
pub async fn with_allocated_ips(
    value: InstanceNetworkConfig,
    txn: &mut PgConnection,
    instance_id: InstanceId,
    machine: &HostMachine,
) -> DatabaseResult<InstanceNetworkConfig> {
    crate::instance_address::allocate(txn, instance_id, value, machine).await
}

/// Batch find host_inband segments for multiple machines and return a map.
/// This allows efficient batch processing in batch_allocate_instances.
pub async fn batch_get_inband_segments_by_machine_ids(
    txn: &mut PgConnection,
    machine_ids: &[HostMachineId],
) -> DatabaseResult<HashMap<HostMachineId, Vec<NetworkSegmentId>>> {
    crate::network_segment::batch_find_ids_by_machine_ids(
        txn,
        machine_ids,
        Some(NetworkSegmentType::HostInband),
    )
    .await
}

/// Add inband interfaces to a network config based on segment IDs.
/// This is a pure function that can be used after batch querying.
///
/// This only injects when `auto_config` is set with a VpcId. If we get a non-auto
/// config, just leave as-is and return it unchanged (as in, there
/// are no inband interfaces to add).
///
/// Additionally, an `auto` config that arrives with non-empty
/// `interfaces` is rejected. We have a TryFrom implementation
/// for rpc::InstanceNetworkConfig that makes sure we're not in
/// this state to begin with, but we still check here anyway.
pub fn add_inband_interfaces_to_config(
    mut network_config: InstanceNetworkConfig,
    host_inband_segment_ids: &[NetworkSegmentId],
) -> DatabaseResult<InstanceNetworkConfig> {
    let Some(vpc_id) = network_config.auto_config.map(|c| c.vpc_id) else {
        return Ok(network_config);
    };

    if !network_config.interfaces.is_empty() {
        return Err(DatabaseError::InvalidArgument(format!(
            "InstanceNetworkConfig.auto reached the resolver with {} \
             pre-existing interfaces; auto requests must arrive with an \
             empty interfaces list",
            network_config.interfaces.len(),
        )));
    }

    for host_inband_segment_id in host_inband_segment_ids {
        network_config.interfaces.push(InstanceInterfaceConfig {
            function_id: InterfaceFunctionId::Physical {},
            network_segment_id: Some(*host_inband_segment_id),
            network_details: None,
            vpc_selection: None,
            ip_addrs: Default::default(),
            interface_prefixes: Default::default(),
            network_segment_gateways: Default::default(),
            host_inband_mac_address: None,
            device_locator: None,
            internal_uuid: uuid::Uuid::new_v4(),
            requested_ip_addr: None,
            ipv6_interface_config: None,
            routing_profile: None,
            vpc_id: Some(vpc_id),
        });
    }

    Ok(network_config)
}
