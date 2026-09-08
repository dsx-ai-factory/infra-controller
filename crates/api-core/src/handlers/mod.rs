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

pub(super) mod api;
mod astra;
pub(super) mod attestation;
pub(super) mod bmc_credential_rotation;
pub(super) mod bmc_endpoint_explorer;
pub(super) mod bmc_metadata;
pub(super) mod boot_override;
mod client_resolution;
pub(super) mod component_manager;
pub(super) mod compute_allocation;
pub(super) mod credential;
pub(super) mod credential_rotation;
pub(super) mod db;
pub(super) mod dns;
pub(super) mod domain;
pub(super) mod dpa;
pub(super) mod dpf;
pub(super) mod dpu;
pub(super) mod dpu_remediation;
pub(super) mod dpu_service_sync;
pub(super) mod expected_machine;
pub(super) mod expected_power_shelf;
pub(super) mod expected_rack;
pub(super) mod expected_switch;
pub(super) mod extension_service;
pub(super) mod finder;
pub(super) mod firmware;
pub(super) mod gpu_reset;
pub(super) mod health;
pub(super) mod host_reprovisioning;
pub(super) mod ib_fabric;
pub(super) mod ib_partition;
pub(super) mod instance;
pub(super) mod instance_type;
pub(super) mod logical_partition;
pub(super) mod machine;
pub(super) mod machine_boot_interfaces;
pub(super) mod machine_discovery;
pub(super) mod machine_hardware_info;
pub(super) mod machine_identity;
pub(super) mod machine_interface;
pub(super) mod machine_interface_address;
pub(super) mod machine_quarantine;
pub(super) mod machine_scout;
pub(super) mod machine_validation;
pub(super) mod managed_host;
pub(super) mod measured_boot;
pub(super) mod mlx_admin;
pub(super) mod network_devices;
pub(super) mod network_security_group;
pub(super) mod network_segment;
pub(super) mod nic_lockdown_credential_rotation;
pub(super) mod nmxc_browse;
pub(super) mod nvl_partition;
pub(super) mod nvlink_domain;
pub(super) mod nvlink_nmxc_endpoints;
pub(super) mod operating_system;
pub(super) mod power_options;
pub(super) mod power_shelf;
mod primary_interface;
pub(super) mod pxe;
pub(super) mod rack;
pub(super) mod redfish;
pub(super) mod resource_pool;
pub(super) mod route_server;
pub(super) mod scout_stream;
pub(super) mod secrets;
pub(super) mod site_explorer;
pub(super) mod site_prefix;
pub(super) mod sku;
pub(super) mod spx_partition;
mod static_address_metrics;
pub(super) mod svpc;
pub(super) mod switch;
pub(super) mod tenant;
pub(super) mod tenant_identity_config;
pub(super) mod tenant_keyset;
mod tenant_prefix_overlap;
pub(super) mod tpm_ca;
pub(super) mod uefi;
pub(super) mod uefi_credential_rotation;
mod utils;
pub(super) mod vpc;
pub(super) mod vpc_peering;
pub(super) mod vpc_prefix;

#[cfg(test)]
pub(crate) async fn resolve_machine_interface_for_test(
    conn: &mut sqlx::PgConnection,
    client_ip: std::net::IpAddr,
) -> Result<model::machine::MachineInterfaceSnapshot, crate::CarbideError> {
    client_resolution::resolve_machine_interface(conn, client_ip).await
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn process_scout_req_for_test(
    api: &crate::Api,
    machine_id: carbide_uuid::machine::HostMachineId,
) -> crate::CarbideResult<rpc::forge_agent_control_response::Action> {
    svpc::process_scout_req(api, machine_id).await
}

/// Exposes the private scout update path to crate-level database race tests.
#[cfg(test)]
pub(crate) async fn update_primary_interface_from_scout_for_test(
    api: &crate::Api,
    machine_id: carbide_uuid::machine::StableHostMachineId,
    hardware_info: &model::hardware_info::HardwareInfo,
) -> crate::CarbideResult<bool> {
    primary_interface::update_primary_interface_from_scout(api, machine_id, hardware_info)
        .await
        .map(|update| update.reconciliation_needed)
}
