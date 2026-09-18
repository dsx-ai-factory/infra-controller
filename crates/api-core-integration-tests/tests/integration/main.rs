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

mod batch_instance_allocation_validation;
mod batch_instance_release;
mod compute_allocation;
mod credential_management;
mod credential_rotation;
mod dhcp_lease_expiration;
mod dns_resolution;
mod dpa_interfaces;
mod dpu_agent_upgrade;
mod dpu_info_list;
mod dpu_machine_inventory;
mod dynamic_config;
mod expected_power_shelf;
mod expected_power_shelf_crud;
mod expected_power_shelf_static_address;
mod expected_rack;
mod expected_switch_static_address;
mod explored_managed_host_find;
mod explored_mlx_devices;
mod find_by_ids_guards;
mod forge_agent_control;
mod ib_fabric_find;
mod level_filter;
mod machine_bmc_metadata;
mod machine_boot_interfaces;
mod machine_metadata;
mod nvlink_domain_health;
mod operating_system;
mod power_options;
mod power_shelf;
mod power_shelf_decommission;
mod power_shelf_delete;
mod power_shelf_find;
mod power_shelf_health;
mod power_shelf_maintenance;
mod rack_find;
mod rack_profile;
mod redfish_actions;
mod route_servers;
mod scout_firmware_upgrade_status;
mod set_primary_dpu;
mod static_address_management;
mod storage;
mod switch_find;
mod switch_health;
mod tenant_keyset_find;
mod vpc_find;
