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

//! DGX GB300 NVL compute tray. Unlike the Lenovo GB300 (AMI host BMC), the DGX
//! variant runs the NVIDIA "GB BMC" -- the same host-BMC family as the Wiwynn
//! GB200 -- so its ServiceRoot reports vendor "NVIDIA" / product "GB BMC", with
//! an NVIDIA host chassis. Values mirror a real DGX GB300 Redfish scrape.

use std::borrow::Cow;
use std::sync::Arc;

use mac_address::MacAddress;
use serde_json::json;

use crate::{BootOptionKind, Callbacks, hw, redfish};

pub(crate) struct DgxGB300Nvl<'a> {
    pub(crate) system_0_serial_number: Cow<'a, str>,
    pub(crate) chassis_0_serial_number: Cow<'a, str>,
    pub(crate) dpu: hw::bluefield3::Bluefield3<'a>,
    pub(crate) embedded_1g_nic: hw::nic_intel_i210::NicIntelI210,
    pub(crate) bmc_mac_address_eth0: MacAddress,
    pub(crate) bmc_mac_address_eth1: MacAddress,
    pub(crate) bmc_mac_address_usb0: MacAddress,
    pub(crate) hgx_bmc_mac_address_usb0: MacAddress,
    pub(crate) hgx_serial_number: Cow<'a, str>,
    pub(crate) topology: Option<hw::nvidia_gbx00::Topology>,
    pub(crate) cpu: [hw::nvidia_gb300::NvidiaGB300Cpu<'a>; 2],
    pub(crate) gpu: [hw::nvidia_gb300::NvidiaGB300Gpu<'a>; 4],
    pub(crate) io_board: [hw::nvidia_gb300::NvidiaGB300IoBoard<'a>; 2],
}

impl DgxGB300Nvl<'_> {
    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        let bmc_manager_id = "BMC_0";
        let bmc_eth_builder = |eth| {
            redfish::ethernet_interface::builder(&redfish::ethernet_interface::manager_resource(
                bmc_manager_id,
                eth,
            ))
        };
        redfish::manager::Config {
            managers: vec![
                redfish::manager::SingleConfig {
                    id: bmc_manager_id,
                    eth_interfaces: Some(vec![
                        bmc_eth_builder("eth0")
                            .mac_address(self.bmc_mac_address_eth0)
                            .interface_enabled(true)
                            .build(),
                        bmc_eth_builder("eth1")
                            .mac_address(self.bmc_mac_address_eth1)
                            .interface_enabled(true)
                            .build(),
                        bmc_eth_builder("usb0")
                            .mac_address(self.bmc_mac_address_usb0)
                            .interface_enabled(true)
                            .build(),
                    ]),
                    host_interfaces: Some(vec![
                        redfish::host_interface::builder(
                            &redfish::host_interface::manager_resource(bmc_manager_id, "Self"),
                        )
                        .interface_enabled(true)
                        .build(),
                    ]),
                    // NVIDIA "GB BMC" host BMC firmware, per the DGX GB300 scrape.
                    serial_interfaces: None,
                    firmware_version: Some("HR-2511-02.0"),
                    oem: None,
                },
                redfish::manager::SingleConfig {
                    id: "HGX_BMC_0",
                    eth_interfaces: Some(vec![
                        redfish::ethernet_interface::builder(
                            &redfish::ethernet_interface::manager_resource("HGX_BMC_0", "usb0"),
                        )
                        .mac_address(self.hgx_bmc_mac_address_usb0)
                        .interface_enabled(true)
                        .build(),
                    ]),
                    host_interfaces: None,
                    // Family-wide NVL HMC firmware label (same on GB200/GB300).
                    serial_interfaces: None,
                    firmware_version: Some("GB200Nvl-25.08-B"),
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
        let boot_options = std::iter::once(
            redfish::boot_option::builder(
                &redfish::boot_option::resource(system_id, "Boot0002"),
                BootOptionKind::Disk,
            )
            .boot_option_reference("Boot0002")
            .display_name("ubuntu")
            .build(),
        )
        .chain(
            [&self.embedded_1g_nic.ethernet_nic(), &self.dpu.host_nic()]
                .into_iter()
                .enumerate()
                .map(|(n, nic)| {
                    let id = format!("Boot{:04X}", n + 3); // Starting with 0003
                    let pci_path = "PciRoot(0x0)/Pci(0x10,0x0)/Pci(0x0,0x0)";
                    redfish::boot_option::builder(
                        &redfish::boot_option::resource(system_id, &id),
                        BootOptionKind::Network,
                    )
                    .boot_option_reference(&id)
                    .display_name(&format!(
                        "UEFI HTTPv4 (MAC:{})",
                        nic.mac_address.to_string().replace(":", ""),
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

        let eth_interfaces = [&self.embedded_1g_nic.ethernet_nic()]
            .iter()
            .enumerate()
            .map(|(index, nic)| {
                redfish::ethernet_interface::builder(&redfish::ethernet_interface::system_resource(
                    system_id,
                    &format!("EthernetInterface{index}"),
                ))
                .mac_address(nic.mac_address)
                .interface_enabled(false)
                .build()
            })
            .collect();

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
                    model: Some("GB300 1CPU:2GPU Board PC".into()),
                    bios_version: None,
                    oem: redfish::computer_system::Oem::Generic,
                    callbacks: None,
                    serial_console: None,
                    secure_boot_available: false,
                    serial_number: Some(self.hgx_serial_number.to_string().into()),
                    storage: None,
                    processors: None,
                    memory: None,
                },
                redfish::computer_system::SingleSystemConfig {
                    base_bios: Some(base_bios(system_id)),
                    bios_mode: redfish::computer_system::BiosMode::Generic,
                    boot_options: Some(boot_options),
                    boot_order_mode: redfish::computer_system::BootOrderMode::ViaSettings,
                    chassis: vec!["Chassis_0".into()],
                    eth_interfaces: Some(eth_interfaces),
                    id: system_id.into(),
                    log_services: None,
                    // DGX GB300: NVIDIA host system (vs Lenovo's "HG634N_V2").
                    manufacturer: Some("NVIDIA".into()),
                    model: Some("GB300 Titania-Bianca Compute Tray".into()),
                    bios_version: None,
                    oem: redfish::computer_system::Oem::Generic,
                    callbacks: Some(callbacks),
                    serial_console: None,
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
        let dpu_chassis = |chassis_id: &'static str, bf3: &hw::bluefield3::Bluefield3<'_>| {
            let nic = bf3.host_nic();
            redfish::chassis::SingleChassisConfig {
                id: chassis_id.into(),
                chassis_type: "Component".into(),
                manufacturer: Some("Nvidia".into()),
                part_number: nic.part_number.map(|v| format!("{v}           ",).into()),
                model: Some("BlueField-3 DPU".into()),
                serial_number: nic.serial_number.map(|v| v.to_string().into()),
                sensors: Some(redfish::sensor::generate_chassis_sensors(
                    chassis_id,
                    redfish::sensor::Layout {
                        temperature: 4,
                        ..Default::default()
                    },
                )),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }
        };
        redfish::chassis::ChassisConfig {
            chassis: (0..=3)
                .map(|n| {
                    hw::nvidia_gbx00::cbc_chassis(format!("CBC_{n}").into(), self.topology.as_ref())
                })
                .chain(std::iter::once(redfish::chassis::SingleChassisConfig {
                    id: "Chassis_0".into(),
                    chassis_type: "RackMount".into(),
                    // DGX GB300: NVIDIA host chassis (vs Lenovo). Part number from the scrape.
                    manufacturer: Some("NVIDIA".into()),
                    part_number: Some("699-25859-0331-RC2".into()),
                    model: Some("GB300 Titania-Bianca Compute Tray".into()),
                    serial_number: Some(self.chassis_0_serial_number.to_string().into()),
                    sensors: Some(redfish::sensor::generate_chassis_sensors(
                        "Chassis_0",
                        redfish::sensor::Layout {
                            temperature: 47,
                            power: 2,
                            voltage: 12,
                            fan: 24,
                            current: 0,
                        },
                    )),
                    leak_detectors: Some(redfish::leak_detector::generate_chassis_leak_detectors(
                        4,
                    )),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                }))
                .chain(self.cpu.iter().enumerate().map(|(n, cpu)| {
                    let id = format!("HGX_CPU_{n}");
                    cpu.as_hgx_chassis(id.into())
                }))
                .chain(self.gpu.iter().enumerate().map(|(n, gpu)| {
                    let id = format!("HGX_GPU_{n}");
                    gpu.as_hgx_chassis(id.into())
                }))
                .chain(self.io_board.iter().enumerate().map(|(n, ioboard)| {
                    let id = format!("IO_Board_{n}");
                    ioboard.as_chassis(id.into())
                }))
                .chain(std::iter::once(dpu_chassis(
                    "Riser_Slot1_BlueField_3_Card",
                    &self.dpu,
                )))
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

fn base_bios(system_id: &str) -> serde_json::Value {
    // DGX GB300 carries the NVIDIA EmbeddedUefiShell knob (confirmed in the scrape).
    redfish::bios::builder(&redfish::bios::resource(system_id))
        .attributes(json!({
            "EmbeddedUefiShell": "Enabled"
        }))
        .build()
}
