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

use crate::{BootOptionKind, Callbacks, LogService, LogServices, hw, redfish};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Mode {
    // B4240V installed on VR NVL.
    B4240V,
    // Air Cooled Bluefield-4 DPU
    B4240,
}

pub(crate) struct Bluefield4<'a> {
    pub(crate) product_serial_number: Cow<'a, str>,
    pub(crate) host_mac_address: MacAddress,
    pub(crate) oob_mac_address: MacAddress,
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) mode: Mode,
}

impl Bluefield4<'_> {
    const SYSTEM_ID: &'static str = "BlueField_0";
    const MANAGER_ID: &'static str = "BlueField_BMC_0";
    const BMC_CHASSIS_ID: &'static str = "BlueField_BMC_0";
    const NETWORK_ADAPTER_ID: &'static str = "BlueField_NIC_0";
    const NETWORK_DEVICE_FUNCTION_ID: &'static str = "0";
    const NDF0_TO_BASE_MAC_OFFSET: u64 = 0x10;

    fn sensor_layout() -> redfish::sensor::Layout {
        // The older BF4 layout exposed these sensors below Card1. Newer
        // firmware renamed the main card chassis to BlueField_0. The generic mock
        // layout currently models Temperature, Fan, Power, Current,
        // and Voltage.  Missing BF4 ReadingType counts: Percent=64,
        // Frequency=2, EnergyJoules=1.
        redfish::sensor::Layout {
            temperature: 5,
            fan: 0,
            power: 6,
            current: 0,
            voltage: 18,
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        redfish::chassis::ChassisConfig {
            chassis: vec![
                redfish::chassis::SingleChassisConfig {
                    id: "BlueField_0".into(),
                    chassis_type: "Card".into(),
                    manufacturer: Some("Nvidia".into()),
                    model: Some(self.model().into()),
                    part_number: Some(self.part_number().into()),
                    serial_number: Some(self.product_serial_number.to_string().into()),
                    network_adapters: Some(self.network_adapters()),
                    pcie_devices: Some(vec![]),
                    sensors: Some(redfish::sensor::generate_chassis_sensors(
                        "BlueField_0",
                        Self::sensor_layout(),
                    )),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: Self::BMC_CHASSIS_ID.into(),
                    chassis_type: "Component".into(),
                    manufacturer: Some("Nvidia".into()),
                    model: Some("BlueField-4".into()),
                    part_number: Some(self.part_number().into()),
                    pcie_devices: Some(vec![]),
                    sensors: Some(vec![]),
                    serial_number: Some(self.product_serial_number.to_string().into()),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: "BlueField_ERoT_BMC_0".into(),
                    chassis_type: "Component".into(),
                    manufacturer: Some(Cow::Borrowed("NVIDIA")),
                    serial_number: Some("".into()),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: "BlueField_ERoT_CPU_0".into(),
                    chassis_type: "Component".into(),
                    manufacturer: Some(Cow::Borrowed("NVIDIA")),
                    serial_number: Some("".into()),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
                redfish::chassis::SingleChassisConfig {
                    id: "BlueField_IRoT_NIC_0".into(),
                    chassis_type: "Component".into(),
                    manufacturer: Some(Cow::Borrowed("NVIDIA")),
                    serial_number: Some("0x3BC1ADDC364432C9".into()),
                    ..redfish::chassis::SingleChassisConfig::defaults()
                },
            ],
        }
    }

    fn network_adapters(&self) -> Vec<redfish::network_adapter::NetworkAdapter> {
        let function = redfish::network_device_function::builder(
            &redfish::network_device_function::chassis_resource(
                Self::SYSTEM_ID,
                Self::NETWORK_ADAPTER_ID,
                Self::NETWORK_DEVICE_FUNCTION_ID,
            ),
        )
        .ethernet(json!({
            "PermanentMACAddress": Self::ndf0_permanent_mac(self.host_mac_address),
        }))
        .build();

        vec![
            redfish::network_adapter::builder_from_nic(
                &redfish::network_adapter::chassis_resource(
                    Self::SYSTEM_ID,
                    Self::NETWORK_ADAPTER_ID,
                ),
                &self.host_nic(),
            )
            .network_device_functions(
                &redfish::network_device_function::chassis_collection(
                    Self::SYSTEM_ID,
                    Self::NETWORK_ADAPTER_ID,
                ),
                vec![function],
            )
            .status(redfish::resource::Status::Ok)
            .build(),
        ]
    }

    fn ndf0_permanent_mac(host_mac_address: MacAddress) -> MacAddress {
        Self::offset_mac(host_mac_address, Self::NDF0_TO_BASE_MAC_OFFSET)
    }

    fn offset_mac(mac_address: MacAddress, offset: u64) -> MacAddress {
        let bytes = mac_address.bytes();
        let value = u64::from_be_bytes([
            0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
        ])
        .checked_add(offset)
        .expect("BF4 NDF0 MAC offset must not overflow");
        let bytes = value.to_be_bytes();
        MacAddress::new([bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]])
    }

    pub(crate) fn system_config(
        &self,
        callbacks: Arc<dyn Callbacks>,
    ) -> redfish::computer_system::Config {
        let system_id = Self::SYSTEM_ID;
        let boot_opt_builder = |id: &str, kind| {
            redfish::boot_option::builder(&redfish::boot_option::resource(system_id, id), kind)
                .boot_option_reference(id)
        };
        let host_mac = self
            .host_mac_address
            .to_string()
            .replace(':', "")
            .to_ascii_uppercase();
        let oob_mac = self
            .oob_mac_address
            .to_string()
            .replace(':', "")
            .to_ascii_uppercase();
        let nic_base = |function: &str| {
            format!(
                "VenHw(1E5A432C-0466-4D31-B009-D4D9239271D3)/\
                 MemoryMapped(0xB,0x14140000,0x14141FFF)/PciRoot(0x6)/\
                 Pci(0x0,0x0)/Pci({function})/MAC({host_mac},0x1)"
            )
        };
        let oob_base = format!(
            "VenHw(1E5A432C-0466-4D31-B009-D4D9239271D3)/\
             MemoryMapped(0xB,0x14180000,0x14181FFF)/PciRoot(0x8)/\
             Pci(0x0,0x0)/Pci(0x0,0x0)/Pci(0x6,0x0)/Pci(0x0,0x0)/\
             MAC({oob_mac},0x1)"
        );
        let pxe_v4_display_name = format!("UEFI PXEv4 (MAC:{host_mac})");
        let pxe_v6_display_name = format!("UEFI PXEv6 (MAC:{host_mac})");
        let http_v4_display_name = format!("UEFI HTTPv4 (MAC:{host_mac})");
        let http_v6_display_name = format!("UEFI HTTPv6 (MAC:{host_mac})");
        let network_boot_options = [
            (
                "Boot0004",
                "NET-OOB-IPV4",
                format!("{oob_base}/IPv4(0.0.0.0)"),
            ),
            (
                "Boot0005",
                "NET-OOB-IPV6",
                format!("{oob_base}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)"),
            ),
            (
                "Boot0006",
                "NET-OOB.4040-IPV4",
                format!("{oob_base}/Vlan(4040)/IPv4(0.0.0.0)"),
            ),
            (
                "Boot0007",
                "NET-OOB.4040-IPV6",
                format!(
                    "{oob_base}/Vlan(4040)/\
                     IPv6(0000:0000:0000:0000:0000:0000:0000:0000)"
                ),
            ),
            (
                "Boot0008",
                "NET-NIC_P0-IPV4",
                format!("{}/IPv4(0.0.0.0)", nic_base("0x0,0x0")),
            ),
            (
                "Boot0009",
                "NET-NIC_P0-IPV6",
                format!(
                    "{}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)",
                    nic_base("0x0,0x0")
                ),
            ),
            (
                "Boot000A",
                "NET-NIC_P0-IPV4-HTTP",
                format!("{}/IPv4(0.0.0.0)/Uri()", nic_base("0x0,0x0")),
            ),
            (
                "Boot000B",
                "NET-NIC_P0-IPV6-HTTP",
                format!(
                    "{}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)/Uri()",
                    nic_base("0x0,0x0")
                ),
            ),
            (
                "Boot000C",
                "NET-NIC_P1-IPV4",
                format!("{}/IPv4(0.0.0.0)", nic_base("0x0,0x1")),
            ),
            (
                "Boot000D",
                "NET-NIC_P1-IPV6",
                format!(
                    "{}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)",
                    nic_base("0x0,0x1")
                ),
            ),
            (
                "Boot000E",
                "NET-NIC_P1-IPV4-HTTP",
                format!("{}/IPv4(0.0.0.0)/Uri()", nic_base("0x0,0x1")),
            ),
            (
                "Boot000F",
                "NET-NIC_P1-IPV6-HTTP",
                format!(
                    "{}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)/Uri()",
                    nic_base("0x0,0x1")
                ),
            ),
            (
                "Boot0010",
                pxe_v4_display_name.as_str(),
                format!("{}/IPv4(0.0.0.0)", nic_base("0x0,0x2")),
            ),
            (
                "Boot0011",
                pxe_v6_display_name.as_str(),
                format!(
                    "{}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)",
                    nic_base("0x0,0x2")
                ),
            ),
            (
                "Boot0012",
                http_v4_display_name.as_str(),
                format!("{}/IPv4(0.0.0.0)/Uri()", nic_base("0x0,0x2")),
            ),
            (
                "Boot0013",
                http_v6_display_name.as_str(),
                format!(
                    "{}/IPv6(0000:0000:0000:0000:0000:0000:0000:0000)/Uri()",
                    nic_base("0x0,0x2")
                ),
            ),
            (
                "Boot0014",
                "NET-OOB-IPV4-HTTP",
                format!("{oob_base}/IPv4(0.0.0.0)/Uri()"),
            ),
            (
                "Boot0015",
                "NET-OOB-IPV6-HTTP",
                format!(
                    "{oob_base}/\
                     IPv6(0000:0000:0000:0000:0000:0000:0000:0000)/Uri()"
                ),
            ),
            (
                "Boot0016",
                "NET-OOB.4040-IPV4-HTTP",
                format!("{oob_base}/Vlan(4040)/IPv4(0.0.0.0)/Uri()"),
            ),
            (
                "Boot0017",
                "NET-OOB.4040-IPV6-HTTP",
                format!(
                    "{oob_base}/Vlan(4040)/\
                     IPv6(0000:0000:0000:0000:0000:0000:0000:0000)/Uri()"
                ),
            ),
        ]
        .into_iter()
        .map(|(id, display_name, uefi_device_path)| {
            boot_opt_builder(id, BootOptionKind::Network)
                .display_name(display_name)
                .uefi_device_path(&uefi_device_path)
                .build()
        });
        let boot_options = [
            boot_opt_builder("Boot0000", BootOptionKind::Disk)
                .display_name("ubuntu0")
                .uefi_device_path(
                    "HD(1,GPT,3FECD8EB-847F-49EB-A0E5-A59B9D4C0B47,0x800,0x19000)/\
                     \\EFI\\ubuntu\\shimaa64.efi",
                )
                .build(),
            boot_opt_builder("Boot0003", BootOptionKind::Disk)
                .display_name("UEFI HFS480GEJ8X176N 4425ADEAQ5394I080947 1")
                .uefi_device_path(
                    "VenHw(1E5A432C-0466-4D31-B009-D4D9239271D3)/\
                     MemoryMapped(0xB,0x141A0000,0x141A1FFF)/PciRoot(0x9)/\
                     Pci(0x0,0x0)/Pci(0x0,0x0)/\
                     NVMe(0x1,00-55-CF-55-00-2E-E4-AC)",
                )
                .build(),
        ]
        .into_iter()
        .chain(network_boot_options)
        .chain(std::iter::once(
            boot_opt_builder("Boot0019", BootOptionKind::Disk)
                .display_name("UEFI Shell")
                .uefi_device_path(
                    "Fv(9AEF2E52-DEAD-4F63-B895-3A504A3E63C4)/\
                     FvFile(7C04A583-9E3E-4F1C-AD65-E05268D0B4D1)",
                )
                .build(),
        ))
        .collect();
        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed(system_id),
                manufacturer: Some("Nvidia".into()),
                model: Some("BlueField-4".into()),
                bios_version: None,
                // BF4-26.04-10 exposes this collection with no members.
                eth_interfaces: Some(vec![]),
                chassis: vec![Self::BMC_CHASSIS_ID.into()],
                serial_number: Some(self.product_serial_number.to_string().into()),
                boot_order_mode: redfish::computer_system::BootOrderMode::ViaSettings,
                callbacks: Some(callbacks),
                boot_options: Some(boot_options),
                bios_mode: redfish::computer_system::BiosMode::Generic,
                oem: redfish::computer_system::Oem::NvidiaBluefield,
                base_bios: Some(
                    redfish::bios::builder(&redfish::bios::resource(system_id))
                        .attributes(json!({}))
                        .build(),
                ),
                log_services: Some(Arc::new(Bf4LogServices {
                    event_log: DpuEventLog {
                        entries: vec!["DPU Warm Reset".to_string()],
                    },
                })),
                storage: Some(vec![]),
                processors: Some(vec![]),
                memory: None,
                serial_console: Some(hw::openbmc::enabled_serial_console()),
                secure_boot_available: true,
            }],
        }
    }

    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: Self::MANAGER_ID,
                eth_interfaces: Some(vec![
                    redfish::ethernet_interface::builder(
                        &redfish::ethernet_interface::manager_resource(Self::MANAGER_ID, "eth0"),
                    )
                    .mac_address(self.bmc_mac_address)
                    .interface_enabled(true)
                    .build(),
                ]),
                host_interfaces: None,
                serial_interfaces: None,
                firmware_version: Some("BF4-26.04-10"),
                oem: None,
            }],
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: vec![],
            ..Default::default()
        }
    }

    pub(crate) fn host_nic(&self) -> hw::nic::Nic<'static> {
        match self.mode {
            Mode::B4240 => hw::nic::Nic {
                mac_address: self.host_mac_address,
                serial_number: Some(format!("{}", self.product_serial_number).into()),
                manufacturer: Some("Mellanox Technologies".into()),
                model: Some("B4240".into()),
                description: Some("CX9 Family [ConnectX-9]".into()),
                part_number: Some(self.part_number().into()),
                firmware_version: Some("82.48.0802".into()),
            },
            Mode::B4240V => hw::nic::Nic {
                mac_address: self.host_mac_address,
                serial_number: Some(format!("{}", self.product_serial_number).into()),
                manufacturer: Some("NVIDIA".into()),
                model: Some("NVIDIA BlueField-4 B4240V 800G Liquid Cooled DPU, Dual-port 400GbE / NDR, QSFP112, PCIe Gen6 x16, 64 Arm cores, 128GB LPDDR5x, integrated BMC, Crypto Enabled, Secure Boot Enabled".into()),
                description: None,
                part_number: Some(self.part_number().into()),
                firmware_version: None,
            },
        }
    }

    pub(super) fn model(&self) -> &'static str {
        match self.mode {
            Mode::B4240V => "B4240V",
            Mode::B4240 => "B4240",
        }
    }

    fn part_number(&self) -> &'static str {
        match self.mode {
            Mode::B4240 => "900-9D4B4-CWAA-TSA",
            Mode::B4240V => "900-9D4A4-00CB-TS4",
        }
    }
}

struct DpuEventLog {
    entries: Vec<String>,
}

impl LogService for DpuEventLog {
    fn id(&self) -> &str {
        "EventLog"
    }

    fn entries(&self, collection: &redfish::Collection<'_>) -> Vec<serde_json::Value> {
        self.entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                redfish::log_service::event_entry(collection, &idx.to_string())
                    .message(entry)
                    // These are not required by specification but
                    // required by libredfish. Making it happy. However, in future
                    // we may want to simulate these fields as well.
                    .severity("OK")
                    .created("2026-02-12T02:06:58+00:00")
                    .build()
            })
            .collect()
    }
}

struct Bf4LogServices {
    event_log: DpuEventLog,
}

impl LogServices for Bf4LogServices {
    fn services(&self) -> Vec<&(dyn LogService + '_)> {
        vec![&self.event_log as &dyn LogService]
    }
}
