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

use std::borrow::Cow;
use std::sync::Arc;

use serde_json::json;

use crate::{BootOptionKind, Callbacks, hw, redfish};

pub(crate) struct GenericAmi<'a> {
    pub(crate) product_serial_number: Cow<'a, str>,
    pub(crate) nics: Vec<(hw::nic::SlotNumber, hw::nic::Nic<'a>)>,
}

impl GenericAmi<'_> {
    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        let bmc_manager_id = "Self";
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: bmc_manager_id,
                eth_interfaces: Some(vec![]),
                host_interfaces: Some(vec![
                    redfish::host_interface::builder(&redfish::host_interface::manager_resource(
                        bmc_manager_id,
                        "Self",
                    ))
                    .interface_enabled(true)
                    .build(),
                ]),
                serial_interfaces: None,
                firmware_version: Some("47.20.02"),
                oem: None,
            }],
        }
    }

    pub(crate) fn system_config(
        &self,
        callbacks: Arc<dyn Callbacks>,
    ) -> redfish::computer_system::Config {
        let system_id = "Self";

        let boot_opt_builder = |id: &str, kind| {
            redfish::boot_option::builder(&redfish::boot_option::resource(system_id, id), kind)
                .boot_option_reference(id)
        };
        let boot_options = self
            .nics
            .iter()
            .map(|(slot_number, nic)| {
                (
                    format!(
                        "UEFI P{slot_number}: HTTP IPv4 Nvidia Network Adapter - {}",
                        nic.mac_address
                    ),
                    BootOptionKind::Network,
                )
            })
            .chain(std::iter::once((
                "UEFI OS".to_string(),
                BootOptionKind::Disk,
            )))
            .enumerate()
            .map(|(index, (display_name, kind))| {
                boot_opt_builder(&format!("Boot{index:04X}"), kind)
                    .display_name(&display_name)
                    .build()
            })
            .collect::<Vec<_>>();
        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed(system_id),
                manufacturer: None,
                model: None,
                bios_version: None,
                eth_interfaces: Some(vec![]),
                serial_number: Some(self.product_serial_number.to_string().into()),
                boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                callbacks: Some(callbacks),
                chassis: vec!["Self".into()],
                boot_options: Some(boot_options),
                bios_mode: redfish::computer_system::BiosMode::Generic,
                oem: redfish::computer_system::Oem::Generic,
                log_services: None,
                storage: None,
                processors: None,
                memory: None,
                base_bios: Some(
                    redfish::bios::builder(&redfish::bios::resource(system_id))
                        .attributes(json!({"EndlessBoot":""}))
                        .build(),
                ),
                serial_console: None,
                secure_boot_available: false,
            }],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let chassis_id = "Self";

        let pcie_devices = self
            .nics
            .iter()
            .map(|(slot, nic)| {
                let pcie_device_id = format!("mat_{}", slot);
                redfish::pcie_device::builder_from_nic(
                    &redfish::pcie_device::chassis_resource(chassis_id, &pcie_device_id),
                    nic,
                )
                .status(redfish::resource::Status::Ok)
                .build()
            })
            .collect();

        redfish::chassis::ChassisConfig {
            chassis: vec![redfish::chassis::SingleChassisConfig {
                id: chassis_id.into(),
                chassis_type: "Component".into(),
                pcie_devices: Some(pcie_devices),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }],
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: [
                // BMC version matches manager_config firmware_version.
                ("HostBMC_0", "47.20.02"),
                // Representative UEFI version for a generic AMI server.
                ("HostBIOS_0", "02.04.01"),
            ]
            .iter()
            .map(|(id, version)| {
                redfish::software_inventory::builder(
                    &redfish::software_inventory::firmware_inventory_resource(id),
                )
                .version(version)
                .build()
            })
            .collect(),
            host_bmc_inventory_id: Some("HostBMC_0".to_string()),
            host_uefi_inventory_id: Some("HostBIOS_0".to_string()),
            ..Default::default()
        }
    }
}
