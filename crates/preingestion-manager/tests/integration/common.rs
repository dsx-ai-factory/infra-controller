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
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;

use carbide_uuid::machine::MachineId;
use model::site_explorer::{
    Chassis, ComputerSystem, ComputerSystemAttributes, EndpointExplorationReport, EndpointType,
    InitialBmcResetPhase, Inventory, PowerState, Service,
};
use sqlx::PgConnection;

pub(super) async fn insert_endpoint_version(
    txn: &mut PgConnection,
    addr: &str,
    bmc_version: &str,
    uefi_version: &str,
    powercycle_version: bool,
) -> Result<(), db::DatabaseError> {
    let model = if !powercycle_version {
        "PowerEdge R750"
    } else {
        "Powercycle Test"
    };
    insert_endpoint(
        txn,
        addr,
        "fm100hsag07peffp850l14kvmhrqjf9h6jslilfahaknhvb6sq786c0g3jg",
        "Dell Inc.",
        model,
        bmc_version,
        uefi_version,
    )
    .await
}

async fn insert_endpoint(
    txn: &mut PgConnection,
    addr: &str,
    machine_id_str: &str,
    vendor: &str,
    model: &str,
    bmc_version: &str,
    uefi_version: &str,
) -> Result<(), db::DatabaseError> {
    let address = IpAddr::V4(Ipv4Addr::from_str(addr).unwrap());
    db::explored_endpoints::insert(
        address,
        &build_exploration_report(vendor, model, bmc_version, uefi_version, machine_id_str),
        false,
        txn,
    )
    .await?;
    db::explored_endpoints::set_preingestion_initial_bmc_reset(
        address,
        InitialBmcResetPhase::WaitForExplorerRefresh,
        txn,
    )
    .await
}

fn build_exploration_report(
    vendor: &str,
    model: &str,
    bmc_version: &str,
    uefi_version: &str,
    machine_id_str: &str,
) -> EndpointExplorationReport {
    let machine_id = if machine_id_str.is_empty() {
        None
    } else {
        Some(MachineId::from_str(machine_id_str).unwrap())
    };

    let mut report = EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        vendor: Some(bmc_vendor::BMCVendor::Dell),
        last_exploration_error: None,
        last_exploration_latency: None,
        managers: vec![],
        systems: vec![ComputerSystem {
            model: Some(model.to_string()),
            ethernet_interfaces: vec![],
            id: "".to_string(),
            manufacturer: Some(vendor.to_string()),
            serial_number: None,
            attributes: ComputerSystemAttributes {
                nic_mode: None,
                is_infinite_boot_enabled: Some(true),
            },
            pcie_devices: vec![],
            base_mac: None,
            power_state: PowerState::On,
            sku: None,
            boot_order: None,
            bios_version: None,
            serial_console_ssh_port: None,
        }],
        chassis: vec![Chassis {
            model: Some(model.to_string()),
            id: "".to_string(),
            manufacturer: Some(vendor.to_string()),
            part_number: None,
            serial_number: None,
            network_adapters: vec![],
            compute_tray_index: None,
            physical_slot_number: None,
            revision_id: None,
            topology_id: None,
        }],
        service: vec![Service {
            id: "".to_string(),
            inventories: vec![
                Inventory {
                    id: "Installed-???__iDRAC.???".to_string(),
                    description: None,
                    version: Some(bmc_version.to_string()),
                    release_date: None,
                },
                Inventory {
                    id: "Current-159-1.13.2__BIOS.Setup.1-1".to_string(),
                    description: None,
                    version: Some(uefi_version.to_string()),
                    release_date: None,
                },
            ],
        }],
        machine_id,
        versions: HashMap::default(),
        model: None,
        machine_setup_status: None,
        secure_boot_status: None,
        lockdown_status: None,
        power_shelf_id: None,
        switch_id: None,
        compute_tray_index: None,
        physical_slot_number: None,
        revision_id: None,
        topology_id: None,
        remediation_error: None,
    };
    report.model = report.model();
    report
}
