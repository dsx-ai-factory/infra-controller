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

pub(crate) struct WiwynnGB200Nvl<'a> {
    pub(crate) system_serial_number: Cow<'a, str>,
    pub(crate) chassis_serial_number: Cow<'a, str>,
    pub(crate) compute_board: [hw::nvidia_gb200::BiancaBoard<'a>; 2],
    pub(crate) dpu1: hw::bluefield3::Bluefield3<'a>,
    pub(crate) dpu2: hw::bluefield3::Bluefield3<'a>,
    pub(crate) topology: Option<hw::nvidia_gbx00::Topology>,
    pub(crate) io_board: [hw::nvidia_gb200::IoBoard<'a>; 2],
}

impl WiwynnGB200Nvl<'_> {
    fn sensor_layout() -> redfish::sensor::Layout {
        redfish::sensor::Layout {
            temperature: 40,
            fan: 10,
            power: 10,
            current: 10,
            voltage: 4,
        }
    }

    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        redfish::manager::Config {
            managers: vec![
                redfish::manager::SingleConfig {
                    id: "BMC_0",
                    // TODO: Add eth0, eth1, and hmcusb0.
                    eth_interfaces: Some(vec![
                        redfish::ethernet_interface::builder(
                            &redfish::ethernet_interface::manager_resource("BMC_0", "hostusb0"),
                        )
                        .static_ipv4_address("10.0.1.1", "255.255.255.0", "0.0.0.0")
                        .interface_enabled(true)
                        .build(),
                    ]),
                    host_interfaces: Some(vec![
                        redfish::host_interface::builder(
                            &redfish::host_interface::manager_resource("BMC_0", "hostusb0"),
                        )
                        .interface_enabled(true)
                        .build(),
                    ]),
                    serial_interfaces: None,
                    firmware_version: Some("25.06-2_NV_WW_02"),
                    oem: None,
                },
                redfish::manager::SingleConfig {
                    id: "HGX_BMC_0",
                    eth_interfaces: Some(vec![]), // TODO: usb0
                    host_interfaces: None,
                    serial_interfaces: None,
                    firmware_version: Some("GB200Nvl-25.06-A"),
                    oem: None,
                },
            ],
        }
    }

    pub(crate) fn system_config(
        &self,
        callbacks: Arc<dyn Callbacks>,
    ) -> redfish::computer_system::Config {
        let system_id = "System_0";
        let callbacks = Some(callbacks);
        let serial_number = Some(self.system_serial_number.to_string().into());
        let boot_opt_builder = |id: &str, kind| {
            redfish::boot_option::builder(&redfish::boot_option::resource(system_id, id), kind)
                .boot_option_reference(id)
        };
        let boot_options = [
            boot_opt_builder("Boot0020", BootOptionKind::Disk)
                .display_name("Ubuntu")
                .uefi_device_path("HD(1,GPT,C07AA982-7D30-4663-9538-776771BBED85,0x800,0x219800)/\\EFI\\ubuntu\\shimaa64.efi")
                .build()
        ].into_iter().chain([&self.dpu1, &self.dpu2].into_iter().enumerate().map(|(index, dpu)| {
            let mac = dpu.host_mac_address.to_string().replace(":", "").to_uppercase();
            let display_name = format!("UEFI HTTPv4 (MAC:{mac})");
            boot_opt_builder(&format!("Boot{index:04X}"), BootOptionKind::Network)
                .display_name(&display_name)
                .uefi_device_path(&format!("MAC({mac},0x1)/IPv4(0.0.0.0,0x0,DHCP,0.0.0.0,0.0.0.0,0.0.0.0)/Uri()"))
                .build()
        })).collect::<Vec<_>>();

        let hgx_baseboard_id = "HGX_Baseboard_0";

        redfish::computer_system::Config {
            systems: vec![
                redfish::computer_system::SingleSystemConfig {
                    id: system_id.into(),
                    manufacturer: Some("WIWYNN".into()),
                    model: Some("GB200 NVL".into()),
                    bios_version: None,
                    eth_interfaces: None,
                    serial_number,
                    boot_order_mode: redfish::computer_system::BootOrderMode::ViaSettings,
                    callbacks,
                    chassis: vec!["BMC_0".into()],
                    boot_options: Some(boot_options),
                    bios_mode: redfish::computer_system::BiosMode::Generic,
                    oem: redfish::computer_system::Oem::Generic,
                    base_bios: Some(
                        redfish::bios::builder(&redfish::bios::resource(system_id))
                            .attributes(json!({
                                "EmbeddedUefiShell": "Enabled",
                            }))
                            .build(),
                    ),
                    log_services: None,
                    storage: None,
                    processors: None,
                    memory: None,
                    serial_console: None,
                    secure_boot_available: true,
                },
                redfish::computer_system::SingleSystemConfig {
                    id: hgx_baseboard_id.into(),
                    manufacturer: Some("NVIDIA".into()),
                    model: Some("GB200 NVL".into()),
                    bios_version: None,
                    chassis: vec!["HGX_Chassis_0".into()],
                    eth_interfaces: None,
                    callbacks: None,
                    boot_options: None,
                    serial_number: None,
                    boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                    oem: redfish::computer_system::Oem::Generic,
                    bios_mode: redfish::computer_system::BiosMode::Generic,
                    base_bios: None,
                    log_services: None,
                    storage: None,
                    processors: Some(
                        self.compute_board
                            .iter()
                            .flat_map(|board| board.hgx_gpu_processors(hgx_baseboard_id))
                            .collect(),
                    ),
                    memory: Some(
                        self.compute_board
                            .iter()
                            .flat_map(|board| board.hgx_gpu_memory(hgx_baseboard_id))
                            .collect(),
                    ),
                    serial_console: None,
                    secure_boot_available: false,
                },
            ],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let dpu_chassis = |chassis_id: &'static str, bf3: &hw::bluefield3::Bluefield3<'_>| {
            let nic = bf3.host_nic();
            let network_adapters = Some(vec![
                redfish::network_adapter::builder_from_nic(
                    &redfish::network_adapter::chassis_resource(chassis_id, chassis_id),
                    &nic,
                )
                .status(redfish::resource::Status::Ok)
                .build(),
            ]);

            redfish::chassis::SingleChassisConfig {
                id: chassis_id.into(),
                chassis_type: "Card".into(),
                manufacturer: nic.manufacturer,
                part_number: nic.part_number,
                model: Some("GB200 NVL".into()),
                network_adapters,
                pcie_devices: Some(vec![]),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }
        };
        redfish::chassis::ChassisConfig {
            chassis: std::iter::once(redfish::chassis::SingleChassisConfig {
                id: "BMC_0".into(),
                chassis_type: "Module".into(),
                manufacturer: Some("WIWYNN".into()),
                part_number: Some("B81.11810.0005".into()),
                model: Some("GB200 NVL".into()),
                pcie_devices: Some(vec![]),
                ..redfish::chassis::SingleChassisConfig::defaults()
            })
            .chain(std::iter::once(redfish::chassis::SingleChassisConfig {
                id: "Chassis_0".into(),
                chassis_type: "RackMount".into(),
                manufacturer: Some("NVIDIA".into()),
                part_number: Some("B81.11810.000D".into()),
                model: Some("GB200 NVL".into()),
                sensors: Some(redfish::sensor::generate_chassis_sensors(
                    "Chassis_0",
                    Self::sensor_layout(),
                )),
                leak_detectors: Some(redfish::leak_detector::generate_chassis_leak_detectors(4)),
                assembly: Some(
                    redfish::assembly::builder(&redfish::assembly::chassis_resource("Chassis_0"))
                        .add_data(
                            redfish::assembly::data_builder("0".into())
                                .serial_number(&self.chassis_serial_number)
                                .build(),
                        )
                        .build(),
                ),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }))
            .chain((0..4).map(|index| {
                hw::nvidia_gbx00::cbc_chassis(format!("CBC_{index}").into(), self.topology.as_ref())
            }))
            .chain(
                [(0, "HGX_CPU_0"), (1, "HGX_CPU_1")]
                    .map(|(index, id)| self.compute_board[index].hgx_cpu_chassis(id.into())),
            )
            .chain(
                self.compute_board
                    .iter()
                    .flat_map(|board| board.hgx_gpu_chassis()),
            )
            .chain(
                self.io_board
                    .iter()
                    .zip(["IO_Board_0", "IO_Board_1"])
                    .map(|(board, id)| board.as_chassis(id.into())),
            )
            .chain(std::iter::once(dpu_chassis(
                "Riser_Slot1_BlueField_3_Card",
                &self.dpu1,
            )))
            .chain(std::iter::once(dpu_chassis(
                "Riser_Slot2_BlueField_3_Card",
                &self.dpu2,
            )))
            .collect(),
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        let fw_inv_builder = |id: &str| {
            redfish::software_inventory::builder(
                &redfish::software_inventory::firmware_inventory_resource(id),
            )
        };
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: [
                // Different examples from real H/W:
                ("FW_BMC_0", "25.06-2_NV_WW_02"),
                ("FW_BMC_1", "    "),
                ("FW_CPLD_0", "0x00 0x0b 0x03 0x04"),
                ("FW_ERoT_AP_CFG_0", "0128"),
                ("NIC_0", "32.47.1026"),
                ("TPM_Firmware", "15.23"),
                ("UEFI", "02.04.12-dde0f655"),
                ("HGX_FW_BMC_0", "GB200Nvl-25.06-A"),
                ("HGX_FW_CPLD_0", "0.22"),
                ("HGX_FW_CPU_0", "00000082"),
                ("HGX_FW_ERoT_BMC_0", "01.04.0031.0000_n04"),
            ]
            .iter()
            .map(|(id, version)| fw_inv_builder(id).version(version).build())
            .collect(),
            host_bmc_inventory_id: Some("FW_BMC_0".to_string()),
            host_uefi_inventory_id: Some("HGX_FW_CPU_0".to_string()),
            ..Default::default()
        }
    }
}
