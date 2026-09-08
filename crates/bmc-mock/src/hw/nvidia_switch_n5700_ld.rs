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

const BMC_MODEL: &str = "P3809";
const BMC_PART_NUMBER: &str = "692-13809-4404-000";
const BMC_FIRMWARE_VERSION: &str = "88.0002.1978";
const SWITCH_MODEL: &str = "N5700_LD";
const SWITCH_PART_NUMBER: &str = "920-9K33D-00MV-GS0";

pub(crate) struct NvidiaSwitchN5700Ld<'a> {
    pub(crate) bmc_mac_address_eth0: MacAddress,
    pub(crate) bmc_mac_address_eth1: MacAddress,
    pub(crate) bmc_mac_address_usb0: MacAddress,
    pub(crate) bmc_serial_number: Cow<'a, str>,
    pub(crate) switch_serial_number: Cow<'a, str>,
}

impl NvidiaSwitchN5700Ld<'_> {
    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        let manager_id = "BMC_0";
        let eth_builder = |eth| {
            redfish::ethernet_interface::builder(&redfish::ethernet_interface::manager_resource(
                manager_id, eth,
            ))
        };
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: manager_id,
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
                firmware_version: Some(BMC_FIRMWARE_VERSION),
                oem: None,
            }],
        }
    }

    pub(crate) fn system_config(&self) -> redfish::computer_system::Config {
        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed("System_0"),
                manufacturer: Some("NVIDIA".into()),
                model: Some(SWITCH_MODEL.into()),
                bios_version: None,
                eth_interfaces: None,
                serial_number: Some(self.switch_serial_number.to_string().into()),
                boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                callbacks: None,
                chassis: vec!["BMC_eeprom".into()],
                boot_options: None,
                bios_mode: redfish::computer_system::BiosMode::Generic,
                oem: redfish::computer_system::Oem::Generic,
                log_services: None,
                storage: Some(vec![]),
                processors: Some(vec![]),
                memory: None,
                base_bios: None,
                serial_console: None,
                secure_boot_available: false,
            }],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let mut chassis = vec![
            self.bmc_eeprom_chassis(),
            self.cpld_chassis(),
            self.mgx_bmc_chassis(),
        ];

        chassis.extend(
            [
                ("MGX_ERoT_BMC_0", "0x0603031319002D12"),
                ("MGX_ERoT_CPU_0", "0x0711030D1910221E"),
                ("MGX_ERoT_FPGA_0", "0x0113090A180E1118"),
                ("MGX_ERoT_NVSwitch_0", "0x0811030C19103B2C"),
                ("MGX_ERoT_NVSwitch_1", "0x0211030C19103B1D"),
            ]
            .into_iter()
            .map(
                |(id, serial_number)| redfish::chassis::SingleChassisConfig {
                    id: id.into(),
                    chassis_type: "Component".into(),
                    manufacturer: Some("NVIDIA".into()),
                    serial_number: Some(serial_number.into()),
                    sku: Some("0x4D35368B".into()),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
            ),
        );

        chassis.extend((0..2).map(|index| self.nvswitch_chassis(index)));
        chassis.extend([
            self.switch_identity_chassis("SYS_eeprom", "Module"),
            self.switch_identity_chassis("System_0", "Component"),
        ]);

        redfish::chassis::ChassisConfig { chassis }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        let fw_inv_builder = |id: &str| {
            redfish::software_inventory::builder(
                &redfish::software_inventory::firmware_inventory_resource(id),
            )
        };
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: [
                ("CPLD_0", "0.00", None, None),
                (
                    "MGX_FW_BMC_0",
                    BMC_FIRMWARE_VERSION,
                    Some("0x001B"),
                    Some("/redfish/v1/Chassis/MGX_BMC_0"),
                ),
                // NVOS firmware from the switch CPU inventory resource.
                ("MGX_FW_CPU_0", "0ACTV_01.01.020", Some("0x00DA"), None),
                (
                    "MGX_FW_ERoT_BMC_0",
                    "01.04.0031.0000_n04",
                    Some("0xFF00"),
                    Some("/redfish/v1/Chassis/MGX_ERoT_BMC_0"),
                ),
                (
                    "MGX_FW_ERoT_CPU_0",
                    "01.04.0031.0000_n04",
                    Some("0xFF00"),
                    None,
                ),
                (
                    "MGX_FW_ERoT_FPGA_0",
                    "01.04.0031.0000_n04",
                    Some("0xFF00"),
                    Some("/redfish/v1/Chassis/MGX_ERoT_FPGA_0"),
                ),
                (
                    "MGX_FW_ERoT_NVSwitch_0",
                    "01.04.0031.0000_n04",
                    Some("0xFF00"),
                    Some("/redfish/v1/Chassis/MGX_ERoT_NVSwitch_0"),
                ),
                (
                    "MGX_FW_ERoT_NVSwitch_1",
                    "01.04.0031.0000_n04",
                    Some("0xFF00"),
                    Some("/redfish/v1/Chassis/MGX_ERoT_NVSwitch_1"),
                ),
                ("MGX_FW_FPGA_0", "0.24", Some("0x0050"), None),
                (
                    "MGX_FW_NVSwitch_0",
                    "35_2014_4784",
                    Some("0x00CF"),
                    Some("/redfish/v1/Chassis/MGX_NVSwitch_0"),
                ),
                (
                    "MGX_FW_NVSwitch_1",
                    "35_2014_4784",
                    Some("0x00CF"),
                    Some("/redfish/v1/Chassis/MGX_NVSwitch_1"),
                ),
            ]
            .into_iter()
            .map(|(id, version, software_id, related_item)| {
                let mut builder = fw_inv_builder(id)
                    .name("Software Inventory")
                    .version(version)
                    .status(redfish::resource::Status::Ok)
                    .updateable(true)
                    .related_items(&related_item.into_iter().collect::<Vec<_>>());
                if id != "CPLD_0" {
                    builder = builder.manufacturer("NVIDIA");
                }
                if let Some(software_id) = software_id {
                    builder = builder.software_id(software_id);
                }
                builder.build()
            })
            .collect(),
            ..Default::default()
        }
    }

    fn bmc_eeprom_chassis(&self) -> redfish::chassis::SingleChassisConfig {
        redfish::chassis::SingleChassisConfig {
            id: "BMC_eeprom".into(),
            chassis_type: "Module".into(),
            manufacturer: Some("NVIDIA".into()),
            part_number: Some(BMC_PART_NUMBER.into()),
            model: Some(BMC_MODEL.into()),
            serial_number: Some(self.bmc_serial_number.to_string().into()),
            pcie_devices: Some(vec![]),
            sensors: Some(vec![]),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }

    fn cpld_chassis(&self) -> redfish::chassis::SingleChassisConfig {
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
        }
    }

    fn mgx_bmc_chassis(&self) -> redfish::chassis::SingleChassisConfig {
        let sensor =
            redfish::sensor::builder(&redfish::sensor::chassis_resource("MGX_BMC_0", "BMC_TEMP"))
                .name("BMC TEMP")
                .reading_f64(33.875)
                .reading_type("Temperature")
                .reading_units("Cel")
                .threshold_lower_caution(5.0)
                .threshold_upper_caution(105.0)
                .threshold_upper_critical(108.0)
                .build();

        redfish::chassis::SingleChassisConfig {
            id: "MGX_BMC_0".into(),
            chassis_type: "Component".into(),
            manufacturer: Some("NVIDIA".into()),
            part_number: Some(BMC_PART_NUMBER.into()),
            model: Some(BMC_MODEL.into()),
            serial_number: Some(self.bmc_serial_number.to_string().into()),
            pcie_devices: Some(vec![]),
            sensors: Some(vec![sensor]),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }

    fn nvswitch_chassis(&self, index: usize) -> redfish::chassis::SingleChassisConfig {
        let chassis_id = format!("MGX_NVSwitch_{index}");
        let sensors = redfish::sensor::generate_chassis_sensors(
            &chassis_id,
            redfish::sensor::Layout {
                temperature: 1,
                power: 1,
                ..Default::default()
            },
        );
        redfish::chassis::SingleChassisConfig {
            id: chassis_id.into(),
            chassis_type: "Component".into(),
            manufacturer: Some("NVIDIA".into()),
            part_number: Some(SWITCH_PART_NUMBER.into()),
            model: Some(SWITCH_MODEL.into()),
            serial_number: Some(self.switch_serial_number.to_string().into()),
            sku: Some("0x331400CF".into()),
            pcie_devices: Some(vec![]),
            sensors: Some(sensors),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }

    fn switch_identity_chassis(
        &self,
        id: &'static str,
        chassis_type: &'static str,
    ) -> redfish::chassis::SingleChassisConfig {
        redfish::chassis::SingleChassisConfig {
            id: id.into(),
            chassis_type: chassis_type.into(),
            manufacturer: Some("NVIDIA".into()),
            part_number: Some(SWITCH_PART_NUMBER.into()),
            model: Some(SWITCH_MODEL.into()),
            serial_number: Some(self.switch_serial_number.to_string().into()),
            pcie_devices: Some(vec![]),
            sensors: Some(vec![]),
            ..redfish::chassis::SingleChassisConfig::defaults()
        }
    }
}
