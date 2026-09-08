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

use mac_address::MacAddress;

use crate::hardware_info::HardwareInfo;
use crate::site_explorer::{
    BlueFieldOperatingMode, Chassis, ComputerSystem, ComputerSystemAttributes,
    EndpointExplorationError, EndpointExplorationReport, EndpointType, EthernetInterface,
    Inventory, Manager, PCIeDevice, PowerState, Service, UefiDevicePath,
};
use crate::test_support::HardwareInfoTemplate;

pub const DPU_INFO_JSON: &[u8] = include_bytes!("../hardware_info/test_data/dpu_info.json");

pub const DPU_BF3_INFO_JSON: &[u8] = include_bytes!("../hardware_info/test_data/dpu_bf3_info.json");

pub const DPU_BF4_INFO_JSON: &[u8] = include_bytes!("../hardware_info/test_data/dpu_bf4_info.json");

#[derive(Clone, Debug)]
pub struct DpuConfig {
    pub serial: String,
    pub host_mac_address: MacAddress,
    pub oob_mac_address: MacAddress,
    pub bmc_mac_address: MacAddress,
    pub bmc_firmware_version: String,
    pub last_exploration_error: Option<EndpointExplorationError>,
    pub override_hosts_uefi_device_path: Option<UefiDevicePath>,
    pub hardware_info_template: HardwareInfoTemplate,
    /// The `nic_mode` value included in the DPU's `EndpointExplorationReport`.
    /// Defaults to `Some(BlueFieldOperatingMode::Dpu)`; tests exercising the auto-correct
    /// path override this to `Some(BlueFieldOperatingMode::Nic)` to simulate a DPU whose
    /// hardware mode doesn't match the operator-declared mode.
    pub nic_mode: Option<BlueFieldOperatingMode>,
}

impl From<&DpuConfig> for HardwareInfo {
    fn from(value: &DpuConfig) -> Self {
        let template = match value.hardware_info_template {
            HardwareInfoTemplate::Default => DPU_INFO_JSON,
            HardwareInfoTemplate::Custom(data) => data,
        };
        let mut info = serde_json::from_slice::<HardwareInfo>(template).unwrap();
        info.dpu_info.as_mut().unwrap().factory_mac_address = value.host_mac_address.to_string();
        info.dpu_info.as_mut().unwrap().firmware_version = "24.42.1000".to_string();
        info.dmi_data.as_mut().unwrap().product_serial = value.serial.clone();
        assert!(info.is_dpu());
        info
    }
}

impl From<DpuConfig> for EndpointExplorationReport {
    fn from(value: DpuConfig) -> Self {
        Self {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: value.last_exploration_error,
            last_exploration_latency: None,
            vendor: Some(bmc_vendor::BMCVendor::Nvidia),
            machine_id: None,
            managers: vec![Manager {
                id: "bmc".to_string(),
                ipmi_port: None,
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("eth0".to_string()),
                    description: Some("Management Network Interface".to_string()),
                    interface_enabled: Some(true),
                    mac_address: Some(value.bmc_mac_address),
                    link_status: None,
                    uefi_device_path: None,
                }],
            }],
            systems: vec![ComputerSystem {
                id: "Bluefield".to_string(),
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("oob_net0".to_string()),
                    description: Some("1G DPU OOB network interface".to_string()),
                    interface_enabled: Some(true),
                    mac_address: Some(value.oob_mac_address),
                    link_status: None,
                    uefi_device_path: None,
                }],
                manufacturer: None,
                model: None,
                serial_number: Some(value.serial.clone()),
                attributes: ComputerSystemAttributes {
                    nic_mode: value.nic_mode,
                    is_infinite_boot_enabled: None,
                },
                pcie_devices: vec![PCIeDevice {
                    description: None,
                    firmware_version: None,
                    id: None,
                    manufacturer: None,
                    gpu_vendor: None,
                    name: None,
                    part_number: Some("900-9D3B6-00CV-AA0".to_string()),
                    serial_number: Some(value.serial.clone()),
                    status: None,
                }],
                base_mac: Some(value.host_mac_address.into()),
                power_state: PowerState::On,
                sku: None,
                boot_order: None,
                bios_version: None,
                serial_console_ssh_port: None,
            }],
            chassis: vec![Chassis {
                id: "Card1".to_string(),
                manufacturer: Some("Nvidia".to_string()),
                model: Some("Bluefield 3 SmartNIC Main Card".to_string()),
                part_number: Some("900-9D3B6-00CV-AA0".to_string()),
                serial_number: Some(value.serial),
                network_adapters: vec![],
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
            }],
            service: vec![Service {
                id: "FirmwareInventory".to_string(),
                inventories: vec![
                    Inventory {
                        id: "DPU_NIC".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some("32.42.1000".to_string()),
                        release_date: None,
                    },
                    Inventory {
                        id: "DPU_BSP".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some("4.5.0.12984".to_string()),
                        release_date: None,
                    },
                    Inventory {
                        id: "BMC_Firmware".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some(value.bmc_firmware_version),
                        release_date: None,
                    },
                    Inventory {
                        id: "DPU_OFED".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some("MLNX_OFED_LINUX-23.10-1.1.8".to_string()),
                        release_date: None,
                    },
                    Inventory {
                        id: "Bluefield_FW_ERoT".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some("00.02.0182.0000_n02".to_string()),
                        release_date: None,
                    },
                    Inventory {
                        id: "DPU_OS".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some(
                            "DOCA_2.5.0_BSP_4.5.0_Ubuntu_22.04-1.20231129.prod".to_string(),
                        ),
                        release_date: None,
                    },
                    Inventory {
                        id: "DPU_SYS_IMAGE".to_string(),
                        description: Some("Host image".to_string()),
                        version: Some("b83f:d203:0090:97a4".to_string()),
                        release_date: None,
                    },
                ],
            }],
            versions: Default::default(),
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
        }
    }
}
