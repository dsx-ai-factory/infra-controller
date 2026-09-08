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

use mac_address::MacAddress;

use crate::redfish;

pub(crate) struct NvidiaSwitchNd5200Ld<'a> {
    pub(crate) bmc_mac_address_eth0: MacAddress,
    pub(crate) bmc_mac_address_eth1: MacAddress,
    pub(crate) bmc_mac_address_usb0: MacAddress,
    pub(crate) bmc_serial_number: Cow<'a, str>,
    pub(crate) switch_serial_number: Cow<'a, str>,
}

impl NvidiaSwitchNd5200Ld<'_> {
    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        let manager_id = "BMC_0";
        let eth_builder = |eth| {
            redfish::ethernet_interface::builder(&redfish::ethernet_interface::manager_resource(
                manager_id, eth,
            ))
        };
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: "BMC_0",
                eth_interfaces: Some(vec![
                    eth_builder("eth0")
                        .mac_address(self.bmc_mac_address_eth0)
                        .interface_enabled(true)
                        .build(),
                    eth_builder("eth1")
                        .mac_address(self.bmc_mac_address_eth1)
                        .interface_enabled(true)
                        .build(),
                    eth_builder("usb0")
                        .mac_address(self.bmc_mac_address_usb0)
                        .interface_enabled(true)
                        .build(),
                ]),
                host_interfaces: None,
                serial_interfaces: None,
                firmware_version: Some("88.0002.1333"),
                oem: None,
            }],
        }
    }

    pub(crate) fn system_config(&self) -> redfish::computer_system::Config {
        let system_id = "System_0";

        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed(system_id),
                manufacturer: None,
                model: None,
                bios_version: None,
                eth_interfaces: None,
                serial_number: None,
                boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                callbacks: None,
                chassis: vec!["BMC_eeprom".into()],
                boot_options: None,
                bios_mode: redfish::computer_system::BiosMode::Generic,
                oem: redfish::computer_system::Oem::Generic,
                log_services: None,
                storage: Some(vec![]),
                processors: None,
                memory: None,
                base_bios: None,
                serial_console: None,
                secure_boot_available: false,
            }],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        redfish::chassis::ChassisConfig {
            chassis: [
                redfish::chassis::SingleChassisConfig {
                    id: "BMC_eeprom".into(),
                    chassis_type: "Module".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: Some("692-13809-1404-000".into()),
                    model: Some("P3809".into()),
                    serial_number: Some(self.bmc_serial_number.to_string().into()),
                    sensors: Some(vec![]),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: "CPLD_0".into(),
                    chassis_type: "Module".into(),
                    manufacturer: Some("Lattice".into()),
                    part_number: Some("".into()),
                    model: Some("LCMXO3D-9400HC-5BG256C".into()),
                    serial_number: Some("CPLDSerialNumber".into()),
                    pcie_devices: Some(vec![]),
                    sensors: Some(vec![]),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: "MGX_BMC_0".into(),
                    chassis_type: "Component".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: Some("692-13809-1404-000".into()),
                    model: Some("P3809".into()),
                    serial_number: Some(self.bmc_serial_number.to_string().into()),
                    pcie_devices: Some(vec![]),
                    sensors: Some(redfish::sensor::generate_chassis_sensors(
                        "MGX_BMC_0",
                        redfish::sensor::Layout {
                            temperature: 1,
                            ..Default::default()
                        },
                    )),
                    leak_detectors: Some(redfish::leak_detector::generate_chassis_leak_detectors(
                        8,
                    )),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
            ]
            .into_iter()
            .chain(
                [
                    "MGX_ERoT_BMC_0",
                    "MGX_ERoT_CPU_0",
                    "MGX_ERoT_FPGA_0",
                    "MGX_ERoT_NVSwitch_0",
                    "MGX_ERoT_NVSwitch_1",
                ]
                .into_iter()
                .enumerate()
                .map(|(n, erot_chassis_id)| {
                    redfish::chassis::SingleChassisConfig {
                        id: erot_chassis_id.into(),
                        chassis_type: "Component".into(),
                        manufacturer: Some("NVIDIA".into()),
                        serial_number: Some(format!("0xFEEEEEEE{n:04X}").into()),
                        ..redfish::chassis::SingleChassisConfig::defaults()
                    }
                }),
            )
            .chain((0..2).map(|n| {
                let chassis_id = format!("MGX_NVSwitch_{n}");
                redfish::chassis::SingleChassisConfig {
                    chassis_type: "Component".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: Some("920-9K36W-00MV-GS0".into()),
                    model: Some("N5200_LD".into()),
                    serial_number: Some(self.switch_serial_number.to_string().into()),
                    network_adapters: None,
                    pcie_devices: Some(vec![]),
                    sensors: Some(redfish::sensor::generate_chassis_sensors(
                        &chassis_id,
                        redfish::sensor::Layout {
                            temperature: 1,
                            power: 1,
                            ..Default::default()
                        },
                    )),
                    assembly: None,
                    id: chassis_id.into(),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                }
            }))
            .collect(),
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: vec![],
            ..Default::default()
        }
    }
}
