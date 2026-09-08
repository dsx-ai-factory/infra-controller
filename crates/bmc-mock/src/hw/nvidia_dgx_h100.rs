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

use mac_address::MacAddress;
use serde_json::json;

use crate::json::JsonExt;
use crate::{BootOptionKind, Callbacks, hw, redfish};

pub(crate) struct NvidiaDgxH100<'a> {
    pub(crate) dgx_system_serial_number: Cow<'a, str>,
    pub(crate) dgx_chassis_serial_number: Cow<'a, str>,
    pub(crate) ib_nics: [hw::nic_nvidia_cx7::NicNvidiaCx7B<'a>; 2],
    pub(crate) mgmt_nic: hw::nic_intel_x550::NicIntelX550,
    pub(crate) dpu: hw::bluefield3::Bluefield3<'a>,
    pub(crate) storage_nic0: hw::nic_nvidia_cx7::NicNvidiaCx7A<'a>,
    pub(crate) storage_nic1: hw::nic_intel_e810::NicIntelE810,
    pub(crate) gpu_serial: [Cow<'a, str>; 8],
    pub(crate) bmc_mac_address_eth0: MacAddress,
    pub(crate) bmc_mac_address_usb0: MacAddress,
    pub(crate) hgx_bmc_mac_address_usb0: MacAddress,
}

impl NvidiaDgxH100<'_> {
    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        let bmc_manager_id = "BMC";
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
                    serial_interfaces: None,
                    firmware_version: Some("25.02.12"),
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
                    serial_interfaces: None,
                    firmware_version: Some("HGX-22.10-1-rc67"),
                    oem: None,
                },
                redfish::manager::SingleConfig {
                    id: "HGX_FabricManager_0",
                    eth_interfaces: None,
                    host_interfaces: None,
                    serial_interfaces: None,
                    firmware_version: None,
                    oem: None,
                },
            ],
        }
    }

    pub(crate) fn system_config(
        &self,
        callbacks: Arc<dyn Callbacks>,
    ) -> redfish::computer_system::Config {
        let system_id = "DGX";
        let callbacks = Some(callbacks);
        let storage_nic0_ports = self.storage_nic0.ethernet_nics();
        let storage_nic1_ports = self.storage_nic1.ethernet_nics();

        let eth_interfaces = Some(
            [
                &self.mgmt_nic.to_nic(),
                &self.dpu.host_nic_h100_variant(),
                &storage_nic0_ports[0],
                &storage_nic0_ports[1],
                &storage_nic1_ports[0],
                &storage_nic1_ports[1],
            ]
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
            .collect(),
        );

        let boot_options = [
            &[self.mgmt_nic.to_nic()] as &[hw::nic::Nic],
            &self.storage_nic0.ethernet_nics(),
            &self.storage_nic1.ethernet_nics(),
            &[self.dpu.host_nic_h100_variant()],
        ]
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(n, nic)| {
            let id = format!("{:04X}", n + 10); // Starting with 000A
            // TODO should be taken from NIC:
            let pci_path = "PciRoot(0x0)/Pci(0x10,0x0)/Pci(0x0,0x0)";
            redfish::boot_option::builder(
                &redfish::boot_option::resource(system_id, &id),
                BootOptionKind::Network,
            )
            .boot_option_reference(&format!("Boot{id}"))
            // Real DisplayName: "UEFI P0: HTTP IPv4 Nvidia Network Adapter - 94:6D:AE:00:00:00"
            .display_name(&format!("UEFI Pn: HTTP IPv4 - {}", nic.mac_address))
            .alias("UefiHttp")
            .uefi_device_path(&format!(
                "{pci_path}/MAC({},0x1)/IPv4(0.0.0.0,0x0,DHCP,0.0.0.0,0.0.0.0,0.0.0.0)/Uri()",
                nic.mac_address.to_string().replace(":", "")
            ))
            // libredfish model requires @odata.etag field.
            .odata_etag("MakeLibRedfishHappy")
            .build()
        })
        .chain(std::iter::once(
            redfish::boot_option::builder(
                &redfish::boot_option::resource(system_id, "0030"),
                BootOptionKind::Disk,
            )
            .boot_option_reference("Boot0030")
            .display_name("UEFI OS")
            .build(),
        ))
        .collect::<Vec<_>>();

        redfish::computer_system::Config {
            systems: vec![
                redfish::computer_system::SingleSystemConfig {
                    id: system_id.into(),
                    manufacturer: Some("NVIDIA".into()),
                    model: Some("DGXH100".into()),
                    bios_version: None,
                    eth_interfaces,
                    serial_number: Some(self.dgx_system_serial_number.to_string().into()),
                    boot_order_mode: redfish::computer_system::BootOrderMode::ViaSettings,
                    callbacks,
                    chassis: vec!["BMC".into()],
                    boot_options: Some(boot_options),
                    bios_mode: redfish::computer_system::BiosMode::Generic,
                    oem: redfish::computer_system::Oem::Generic,
                    base_bios: Some(base_bios(system_id)),
                    log_services: None,
                    storage: None,
                    processors: None,
                    memory: None,
                    serial_console: None,
                    secure_boot_available: true,
                },
                redfish::computer_system::SingleSystemConfig {
                    id: "HGX_Baseboard_0".into(),
                    manufacturer: Some("NVIDIA".into()),
                    model: None,
                    bios_version: None,
                    chassis: vec!["HGX_BMC_0".into()],
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
                    processors: Some(self.hgx_gpu_processors("HGX_Baseboard_0")),
                    memory: None,
                    serial_console: None,
                    secure_boot_available: false,
                },
            ],
        }
    }

    /// The eight GPUs of the HGX baseboard, as Redfish `Processor` resources.
    ///
    /// Hardware exposes each GPU both here and as an `HGX_GPU_SXM_*` chassis.
    /// The processor view is where GPU sensor readings are attributed, so a
    /// fixture that omitted it could not exercise per-GPU sensor telemetry.
    fn hgx_gpu_processors(&self, system_id: &str) -> Vec<redfish::processor::Processor> {
        (1..=8)
            .map(|index| {
                let chassis_id = format!("HGX_GPU_SXM_{index}");
                let voltage_sensor_id =
                    redfish::sensor::sensor_id(redfish::sensor::SensorKind::Voltage, 1);
                redfish::processor::gpu(
                    system_id,
                    &format!("GPU_SXM_{index}"),
                    redfish::sensor::chassis_resource(&chassis_id, &voltage_sensor_id)
                        .odata_id
                        .as_ref(),
                    &redfish::processor::GpuIdentity {
                        uuid: &gpu_uuid(index),
                        serial_number: &self.gpu_serial[index - 1],
                        model: GPU_MODEL,
                    },
                )
            })
            .collect()
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let dgx_chassis_id = "DGX";
        let net_adapter_builder = |id: &str| {
            redfish::network_adapter::builder(&redfish::network_adapter::chassis_resource(
                dgx_chassis_id,
                id,
            ))
        };
        let ib_mapping = [["10", "11", "7", "9"], ["2", "4", "5", "6"]];
        let ib_nics = self.ib_nics.each_ref().map(|nic| nic.ib_nics());
        let mut dgx_network_adapters = [
            ("0", net_adapter_builder("DevType7_NIC0").build()),
            (
                "1",
                redfish::network_adapter::builder_from_nic(
                    &redfish::network_adapter::chassis_resource(dgx_chassis_id, "DevType7_NIC1"),
                    &self.dpu.host_nic_h100_variant(),
                )
                .build(),
            ),
            (
                "3",
                redfish::network_adapter::builder_from_nic(
                    &redfish::network_adapter::chassis_resource(dgx_chassis_id, "DevType7_NIC3"),
                    &self.storage_nic0.ethernet_nics()[0],
                )
                .build(),
            ),
            (
                "8",
                redfish::network_adapter::builder_from_nic(
                    &redfish::network_adapter::chassis_resource(dgx_chassis_id, "DevType7_NIC8"),
                    &self.storage_nic1.ethernet_nics()[0],
                )
                .build(),
            ),
        ]
        .into_iter()
        .chain(ib_nics.into_iter().enumerate().flat_map(|(n1, nics)| {
            nics.into_iter().enumerate().map(move |(n2, nic)| {
                let index = ib_mapping[n1][n2];
                (
                    index,
                    redfish::network_adapter::builder_from_nic(
                        &redfish::network_adapter::chassis_resource(
                            dgx_chassis_id,
                            &format!("DevType7_NIC{index}"),
                        ),
                        &nic,
                    )
                    .build(),
                )
            })
        }))
        .collect::<Vec<_>>();
        dgx_network_adapters.sort_by_key(|v| v.0);
        let dgx_network_adapters = dgx_network_adapters.into_iter().map(|v| v.1).collect();
        redfish::chassis::ChassisConfig {
            chassis: [
                redfish::chassis::SingleChassisConfig {
                    id: Cow::Borrowed("CPUBaseboard"),
                    chassis_type: "Component".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: Some("965-24387-0002-000".into()),
                    model: Some("DGXH100".into()),
                    serial_number: Some(self.dgx_system_serial_number.to_string().into()),
                    network_adapters: Some(vec![]),
                    sensors: Some(redfish::sensor::generate_chassis_sensors(
                        "CPUBaseboard",
                        redfish::sensor::Layout {
                            temperature: 120,
                            ..Default::default()
                        },
                    )),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: dgx_chassis_id.into(),
                    chassis_type: "Other".into(),
                    manufacturer: Some("NVIDIA".into()),
                    part_number: Some("965-24387-0002-000".into()),
                    model: Some("DGXH100".into()),
                    serial_number: Some(self.dgx_chassis_serial_number.to_string().into()),
                    network_adapters: Some(dgx_network_adapters),
                    pcie_devices: Some(
                        (0..133)
                            .map(|n| {
                                redfish::pcie_device::builder(
                                    &redfish::pcie_device::chassis_resource(
                                        dgx_chassis_id,
                                        &format!("00_00_{n:02X}"),
                                    ),
                                )
                                .build()
                            })
                            .collect(),
                    ),
                    sensors: Some(redfish::sensor::generate_chassis_sensors(
                        dgx_chassis_id,
                        redfish::sensor::Layout {
                            temperature: 112 + 8, // TEMP_* + TLIMIT_*
                            fan: 36,              // FAN_*
                            power: 47,            // PWR_*
                            current: 3,           // AMP_*
                            voltage: 17,
                            // TOTAL: 223 of 253
                            // Omitted: 29
                            //     ENERGY_* = 12,
                            //     HMCReady,
                            //     OVERT_* = 2,
                            //     RST_GB1_GPU,
                            //     SEL_FULLNESS,
                            //     STATUS_* = 12,
                            //     WATCHDOG2
                        },
                    )),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
            ]
            .into_iter()
            .chain((1..=8).map(|index| hgx_gpu_sxm_chassis(index, &self.gpu_serial[index - 1])))
            .collect(),
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: [
                // version required carbide to pass ingestion test in site explorer.
                ("CPLDMB_0", "0.2.1.9"),
                // This one is needed for libredfish to setup lockdown
                ("HostBIOS_0", "01.05.03"),
                // This one is needed for libredfish to setup lockdown
                ("HostBMC_0", "24.09.17"),
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

fn base_bios(system_id: &str) -> serde_json::Value {
    redfish::bios::builder(&redfish::bios::resource(system_id))
        .attributes(json!({
            "AcpiSpcrConsoleRedirectionEnable": true,
            "ConsoleRedirectionEnable0": true,
            "AcpiSpcrPort": "COM0",
            "AcpiSpcrFlowControl": "None",
            "AcpiSpcrBaudRate": "115200",
            "BaudRate0": "115200",
            "SriovSupport": "Enabled",
            "VTdSupport": "Enable",
            "Ipv4Http": "Enabled",
            "Ipv4Pxe": "Disabled",
            "Ipv6Http": "Enabled",
            "Ipv6Pxe": "Disabled",
            "NvidiaInfiniteboot": "Enable",
        }))
        .build()
        // For some reasons libredfish requires @odata.context. This
        // patch makes it happy.
        .patch(json!({
            "@odata.context": "MakeLibRedfishHappy"
        }))
}

const GPU_MODEL: &str = "H100 80GB HBM3";

/// Stable per-slot GPU UUID, so a test can assert that a given slot reports a
/// specific device identity.
///
/// Hardware reports one UUID per GPU on the GPU's `Processor`, its enclosing
/// `Chassis` and its `PCIeDevice` alike, so every fixture for a slot uses this.
fn gpu_uuid(index: usize) -> String {
    format!("beea8cdf-7b7d-035c-ed07-360e311fcb{index:02x}")
}

fn hgx_gpu_sxm_chassis(index: usize, serial: &str) -> redfish::chassis::SingleChassisConfig {
    let id = format!("HGX_GPU_SXM_{index}");
    redfish::chassis::SingleChassisConfig {
        chassis_type: "Other".into(),
        manufacturer: Some("NVIDIA".into()),
        part_number: Some("2330-885-A1".into()),
        model: Some(GPU_MODEL.into()),
        serial_number: Some(serial.to_string().into()),
        uuid: Some(gpu_uuid(index).into()),
        pcie_devices: Some(vec![
            redfish::pcie_device::builder(&redfish::pcie_device::chassis_resource(
                &id,
                &format!("GPU_SXM_{index}"),
            ))
            .manufacturer("NVIDIA")
            .model(GPU_MODEL)
            .part_number("2330-885-A1")
            .serial_number(serial)
            .uuid(&gpu_uuid(index))
            .build(),
        ]),
        sensors: Some(redfish::sensor::generate_chassis_sensors(
            &id,
            redfish::sensor::Layout {
                temperature: 3,
                power: 2,
                voltage: 1,
                ..Default::default()
            },
        )),
        id: id.into(),
        ..redfish::chassis::SingleChassisConfig::defaults()
    }
}
