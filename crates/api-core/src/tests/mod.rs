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

mod boot_interface_resolution;
mod client_resolution;
pub(in crate::tests) mod common;
mod domain;
mod dpf;
mod dpu_machine_update;
mod dpu_nic_firmware;
mod dpu_remediation;
mod dpu_reprovisioning;
mod expected_machine;
mod expected_switch;
mod explored_endpoint_find;
mod extension_service;
mod finder;
mod firmware_component_manager;
mod host_bmc_firmware_test;
mod host_firmware_config;
mod ib_fabric_monitor;
mod ib_instance;
mod ib_machine;
mod ib_partition_find;
mod ib_partition_lifecycle;
mod instance;
mod instance_allocate;
mod instance_batch_allocate;
mod instance_config_update;
mod instance_ipxe_behaviors;
mod instance_os;
mod instance_type;
mod ipxe;
mod lldp;
mod machine_admin_force_delete;
mod machine_boot_override;
mod machine_dhcp;
mod machine_discovery;
mod machine_find;
mod machine_health;
mod machine_history;
mod machine_interfaces;
mod machine_metadata;
mod machine_network;
mod machine_states;
mod machine_topology;
mod machine_update_manager;
mod machine_validation;
mod maintenance;
mod managed_host_decommissioning;
#[cfg(feature = "linux-build")]
mod measured_boot;
mod network_security_group;
mod network_segment;
mod network_segment_find;
mod network_segment_lifecycle;
mod nvl_instance;
mod nvl_logical_partition;
mod preingestion_dpu_nic_mode;
mod primary_interface;
mod rack_health;
mod rack_state_controller;
mod redfish_actions;
mod resource_pool;
mod site_explorer;
mod site_prefix;
mod sku;
mod spdm;
mod switch;
mod tenants;
mod tpm_ca;
mod vpc;
mod vpc_peering;
mod vpc_prefix;
// NOTE: the admin web UI tests moved to the `carbide-api-web` crate (alongside the web code they
// exercise).

pub(crate) use crate::tests::common::api_fixtures::create_test_env;
/// Make these symbol available as
/// crate::tests::sqlx_fixture_from_str, so that the
/// [`carbide_macros::sqlx_test`] can delegate to them.
pub(crate) use crate::tests::common::sqlx_fixtures::sqlx_fixture_from_str;

/// Setup logging for tests. Only our own test binary needs this global initializer (it depends on
/// the dev-only `ctor` crate); consumers of the `test-support` fixtures bring their own logging.
#[ctor::ctor(unsafe)]
fn setup_test_logging() {
    crate::test_support::setup_test_logging()
}
