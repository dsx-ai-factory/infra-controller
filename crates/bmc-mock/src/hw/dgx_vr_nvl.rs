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

//! DGX VR NVL compute tray.

use std::borrow::Cow;
use std::sync::Arc;

use mac_address::MacAddress;
use serde_json::json;

use crate::{BootOptionKind, Callbacks, hw, redfish};

pub(crate) struct DgxVrNvl<'a> {
    pub(crate) system_0_serial_number: Cow<'a, str>,
    pub(crate) chassis_0_serial_number: Cow<'a, str>,
    pub(crate) dpu: hw::bluefield4::Bluefield4<'a>,
    pub(crate) bmc_mac_address_eth0: MacAddress,
}

impl DgxVrNvl<'_> {
    const BLUEFIELD_CHASSIS_ID: &'static str = "BlueField_0";
    const BLUEFIELD_NIC_ID: &'static str = "BlueField_NIC_0";
    const BLUEFIELD_PCIE_DEVICE_ID: &'static str = "BlueField_0";

    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        let bmc_manager_id = "BMC_0";
        let bmc_eth_builder = |eth| {
            redfish::ethernet_interface::builder(&redfish::ethernet_interface::manager_resource(
                bmc_manager_id,
                eth,
            ))
        };
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: bmc_manager_id,
                eth_interfaces: Some(vec![
                    bmc_eth_builder("eth0")
                        .mac_address(self.bmc_mac_address_eth0)
                        .interface_enabled(true)
                        .build(),
                ]),
                host_interfaces: None,
                serial_interfaces: None,
                firmware_version: None,
                oem: None,
            }],
        }
    }

    pub(crate) fn system_config(
        &self,
        callbacks: Arc<dyn Callbacks>,
    ) -> redfish::computer_system::Config {
        let system_id = "System_0";
        let boot_options = std::iter::once(
            redfish::boot_option::builder(
                &redfish::boot_option::resource(system_id, "0002"),
                BootOptionKind::Disk,
            )
            .boot_option_reference("Boot0002")
            .display_name("ubuntu")
            .build(),
        )
        .chain(
            [&self.dpu.host_nic()]
                .into_iter()
                .enumerate()
                .map(|(n, nic)| {
                    let id = format!("{:04X}", n + 3); // Starting with 0003
                    let pci_path = "PciRoot(0x0)/Pci(0x10,0x0)/Pci(0x0,0x0)";
                    redfish::boot_option::builder(
                        &redfish::boot_option::resource(system_id, &id),
                        BootOptionKind::Network,
                    )
                    .boot_option_reference(&format!("Boot{id}"))
                    .display_name(&format!(
                        "[SlotFFFF]: PXE IPv4 Some Network Adapter - {}",
                        nic.mac_address
                    ))
                    .uefi_device_path(&format!(
                        "{pci_path}/MAC({},0x1)\
                             /IPv4(0.0.0.0,0x0,DHCP,0.0.0.0,0.0.0.0,0.0.0.0)/Uri()",
                        nic.mac_address.to_string().replace(":", "")
                    ))
                    .build()
                }),
        )
        .collect::<Vec<_>>();

        redfish::computer_system::Config {
            systems: vec![
                redfish::computer_system::SingleSystemConfig {
                    base_bios: None,
                    bios_mode: redfish::computer_system::BiosMode::Generic,
                    boot_options: None,
                    boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                    chassis: vec!["HGX_Chassis_0".into()],
                    eth_interfaces: None,
                    id: "HGX_Baseboard_0".into(),
                    log_services: None,
                    manufacturer: Some("NVIDIA".into()),
                    model: Some("VR NVL".into()),
                    bios_version: None,
                    oem: redfish::computer_system::Oem::Generic,
                    callbacks: None,
                    serial_console: None,
                    secure_boot_available: false,
                    serial_number: None,
                    storage: None,
                    processors: None,
                    memory: None,
                },
                redfish::computer_system::SingleSystemConfig {
                    base_bios: Some(base_bios(system_id)),
                    bios_mode: redfish::computer_system::BiosMode::Generic,
                    boot_options: Some(boot_options),
                    boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                    chassis: vec!["Chassis_0".into()],
                    eth_interfaces: None,
                    id: system_id.into(),
                    log_services: None,
                    manufacturer: Some("NVIDIA".into()),
                    model: Some("VR NVL72".into()),
                    bios_version: None,
                    oem: redfish::computer_system::Oem::Generic,
                    callbacks: Some(callbacks),
                    serial_console: Some(hw::openbmc::enabled_serial_console()),
                    secure_boot_available: true,
                    serial_number: Some(self.system_0_serial_number.to_string().into()),
                    storage: None,
                    processors: None,
                    memory: None,
                },
            ],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        redfish::chassis::ChassisConfig {
            chassis: vec![
                redfish::chassis::SingleChassisConfig {
                    id: "Chassis_0".into(),
                    chassis_type: "RackMount".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: None,
                    model: Some("VR NVL72".into()),
                    serial_number: Some(self.chassis_0_serial_number.to_string().into()),
                    sensors: None,
                    leak_detectors: None,
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                self.bluefield_chassis_config(),
                redfish::chassis::SingleChassisConfig {
                    id: "HGX_Chassis_0".into(),
                    chassis_type: "Zone".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: None,
                    model: Some("VR NVL144".into()),
                    serial_number: None,
                    sensors: None,
                    leak_detectors: None,
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
            ],
        }
    }

    fn bluefield_chassis_config(&self) -> redfish::chassis::SingleChassisConfig {
        let bf4 = self.dpu.host_nic();
        redfish::chassis::SingleChassisConfig {
            id: Self::BLUEFIELD_CHASSIS_ID.into(),
            chassis_type: "Component".into(),
            manufacturer: Some("NVIDIA".into()),
            model: bf4.model.clone(),
            part_number: bf4.part_number.clone(),
            serial_number: bf4.serial_number.clone(),
            network_adapters: Some(vec![self.bluefield_network_adapter()]),
            pcie_devices: Some(vec![
                redfish::pcie_device::builder_from_nic(
                    &redfish::pcie_device::chassis_resource(
                        Self::BLUEFIELD_CHASSIS_ID,
                        Self::BLUEFIELD_PCIE_DEVICE_ID,
                    ),
                    &bf4,
                )
                .status(redfish::resource::Status::Ok)
                .build(),
            ]),
            sensors: None,
            leak_detectors: None,
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }

    fn bluefield_network_adapter(&self) -> redfish::network_adapter::NetworkAdapter {
        let network_device_functions = ["0", "1"]
            .into_iter()
            .map(|id| {
                redfish::network_device_function::builder(
                    &redfish::network_device_function::chassis_resource(
                        Self::BLUEFIELD_CHASSIS_ID,
                        Self::BLUEFIELD_NIC_ID,
                        id,
                    ),
                )
                .build()
            })
            .collect();

        redfish::network_adapter::builder(&redfish::network_adapter::chassis_resource(
            Self::BLUEFIELD_CHASSIS_ID,
            Self::BLUEFIELD_NIC_ID,
        ))
        .network_device_functions(
            &redfish::network_device_function::chassis_collection(
                Self::BLUEFIELD_CHASSIS_ID,
                Self::BLUEFIELD_NIC_ID,
            ),
            network_device_functions,
        )
        .status(redfish::resource::Status::Ok)
        .build()
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: vec![],
            ..Default::default()
        }
    }
}

fn base_bios(system_id: &str) -> serde_json::Value {
    redfish::bios::builder(&redfish::bios::resource(system_id))
        .attributes(json!({
            "EmbeddedUefiShell": "Enabled"
        }))
        .build()
}
