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

//! Contains DPU related fixtures

use std::net::IpAddr;

use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use model::hardware_info::HardwareInfo;
use model::machine::machine_search_config::MachineSearchConfig;
use model::test_support::{DpuConfig, ManagedHostConfig};
use rpc::forge::forge_server::Forge;
use rpc::{DiscoveryData, DiscoveryInfo, MachineDiscoveryInfo};
use sqlx::PgConnection;
use tonic::Request;

use super::site_explorer;
use crate::tests::common::api_fixtures::{FIXTURE_DHCP_RELAY_ADDRESS, TestEnv, TestManagedHost};
use crate::tests::common::rpc_builder::DhcpDiscovery;

/// The version identifier that is used by dpu-agent in unit-tests
pub(in crate::tests) const TEST_DPU_AGENT_VERSION: &str = "test";

/// The version of HBN reported in unit-tests
pub(in crate::tests) const TEST_DOCA_HBN_VERSION: &str = "1.5.0-doca2.2.0";
/// The version of doca-telemetry reported in unit-tests
pub(in crate::tests) const TEST_DOCA_TELEMETRY_VERSION: &str = "1.14.2-doca2.2.0";

/// Creates a Machine Interface and Machine for a DPU
///
/// Returns the ID of the created machine
pub(in crate::tests) async fn create_dpu_machine(
    env: &TestEnv,
    host_config: &ManagedHostConfig,
) -> carbide_uuid::machine::DpuMachineId {
    site_explorer::new_dpu(env, host_config.clone())
        .await
        .unwrap()
}

pub(in crate::tests) async fn create_dpu_machine_in_waiting_for_network_install(
    env: &TestEnv,
    host_config: &ManagedHostConfig,
) -> TestManagedHost {
    site_explorer::new_dpu_in_network_install(env, host_config.clone())
        .await
        .unwrap()
}

pub(in crate::tests) async fn create_machine_inventory(env: &TestEnv, machine_id: MachineId) {
    tracing::debug!(
        machine_id = %machine_id,
        "Creating machine inventory",
    );
    env.api
        .update_agent_reported_inventory(Request::new(rpc::forge::DpuAgentInventoryReport {
            machine_id: Some(machine_id),
            inventory: Some(rpc::forge::MachineInventory {
                components: vec![
                    rpc::forge::MachineInventorySoftwareComponent {
                        name: "doca-hbn".to_string(),
                        version: TEST_DOCA_HBN_VERSION.to_string(),
                        url: "nvcr.io/nvidia/doca/".to_string(),
                    },
                    rpc::forge::MachineInventorySoftwareComponent {
                        name: "doca-telemetry".to_string(),
                        version: TEST_DOCA_TELEMETRY_VERSION.to_string(),
                        url: "nvcr.io/nvidia/doca/".to_string(),
                    },
                ],
            }),
        }))
        .await
        .unwrap()
        .into_inner()
}

/// Uses the `discover_dhcp` API to discover a DPU with a certain MAC address
///
/// Returns the created `machine_interface_id`
pub(in crate::tests) async fn dpu_discover_dhcp(
    env: &TestEnv,
    mac_address: &str,
) -> MachineInterfaceId {
    let response = env
        .api
        .discover_dhcp(
            DhcpDiscovery::builder(mac_address, FIXTURE_DHCP_RELAY_ADDRESS).tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();
    response
        .machine_interface_id
        .expect("machine_interface_id must be set")
}

/// Emulates DPU Machine Discovery (submitting hardware information) for the
/// DPU that uses a certain `machine_interface_id`
pub(in crate::tests) async fn dpu_discover_machine(
    env: &TestEnv,
    dpu_config: &DpuConfig,
    machine_interface_id: MachineInterfaceId,
) -> carbide_uuid::machine::MachineId {
    let response = env
        .api
        .discover_machine(Request::new(MachineDiscoveryInfo {
            machine_interface_id: Some(machine_interface_id),
            discovery_data: Some(DiscoveryData::Info(
                DiscoveryInfo::try_from(HardwareInfo::from(dpu_config)).unwrap(),
            )),
            create_machine: true,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();

    response.machine_id.expect("machine_id must be set")
}

// Convenience method for the tests to get a machine's loopback IP
pub(in crate::tests) async fn loopback_ip(
    txn: &mut PgConnection,
    dpu_machine_id: &MachineId,
) -> IpAddr {
    let dpu = db::machine::find_one(txn, dpu_machine_id, MachineSearchConfig::default())
        .await
        .unwrap()
        .unwrap();
    dpu.network_config.loopback_ip.unwrap()
}
