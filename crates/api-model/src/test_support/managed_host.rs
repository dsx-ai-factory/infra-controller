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
use std::iter;
use std::net::IpAddr;
use std::str::FromStr;

use carbide_uuid::machine::DpuMachineId;
use itertools::Itertools;
use mac_address::MacAddress;

use crate::expected_machine::ExpectedMachineData;
use crate::hardware_info::{HardwareInfo, NetworkInterface, PciDeviceProperties, TpmEkCertificate};
use crate::machine::ManagedHostState;
use crate::site_explorer::{
    Chassis, ComputerSystem, ComputerSystemAttributes, EndpointExplorationReport, EndpointType,
    EthernetInterface, Inventory, Manager, NetworkAdapter, PCIeDevice, PowerState, Service,
    UefiDevicePath,
};
pub use crate::test_support::HardwareInfoTemplate;
use crate::test_support::dpu::DpuConfig;

pub const X86_INFO_JSON: &[u8] = include_bytes!("../hardware_info/test_data/x86_info.json");
pub const REQUIRED_IB_GUIDS: usize = 6;

pub struct ManagedDpuExplorationReport {
    pub dpu_index: u8,
    pub bmc_ip: IpAddr,
    pub report: EndpointExplorationReport,
}

pub struct ManagedHostExplorationResults {
    pub host_report: Option<(IpAddr, EndpointExplorationReport)>,
    pub dpu_reports: Vec<ManagedDpuExplorationReport>,
}

impl ManagedHostExplorationResults {
    pub fn dpu_machine_ids(&self) -> HashMap<u8, DpuMachineId> {
        self.dpu_reports
            .iter()
            .map(|dpu_report| {
                (
                    dpu_report.dpu_index,
                    dpu_report
                        .report
                        .machine_id
                        .expect("DPU exploration report should have a generated machine id")
                        .try_into()
                        .expect("should be a valid DPU machine ID"),
                )
            })
            .collect()
    }

    pub fn into_endpoints(self) -> Vec<(IpAddr, EndpointExplorationReport)> {
        self.dpu_reports
            .into_iter()
            .map(|dpu_report| (dpu_report.bmc_ip, dpu_report.report))
            .chain(self.host_report)
            .collect()
    }
}

/// Describes a Managed Host
#[derive(Clone)]
pub struct ManagedHostConfig {
    pub serial: String,
    pub bmc_mac_address: MacAddress,
    pub tpm_ek_cert: TpmEkCertificate,
    pub dpus: Vec<DpuConfig>,
    pub non_dpu_macs: Vec<MacAddress>,
    pub expected_state: ManagedHostState,
    pub ib_guids: Vec<String>,
    /// Control whether the test fixture should automatically generate and assign SKU
    /// when machine enters WaitingForSkuAssignment state.
    /// Default: true (maintains backward compatibility)
    pub auto_assign_sku_in_fixture: bool,
    pub hardware_info_template: HardwareInfoTemplate,
    /// When set, DPU-backed hosts try ADMIN then ADMIN2 admin-segment DHCP during fixture
    /// ingestion. Used by NVLink rack-switch tests that provision a second admin segment.
    pub admin_dhcp_fallback: bool,
    /// The contents of this will be used as ExpectedMachine entry
    /// However not all fields need to be filled
    /// - bmc username/password are not required
    /// - serial number is copied from ManagedHostConfig
    pub expected_machine_data: Option<ExpectedMachineData>,
    /// The BMC vendor the host's exploration report presents.
    /// Default: Dell. Override to exercise vendor-dependent paths
    /// (e.g. the post-`set_nic_mode` host power cycle).
    pub vendor: Option<bmc_vendor::BMCVendor>,
}

impl ManagedHostConfig {
    pub fn dhcp_mac_address(&self) -> MacAddress {
        if let Some(dpu) = self.dpus.first() {
            dpu.host_mac_address
        } else if let Some(non_dpu_mac) = self.non_dpu_macs.first() {
            *non_dpu_mac
        } else {
            panic!("No DPUs or non-DPU NICs on MockHost")
        }
    }

    pub fn get_and_assert_single_dpu(&self) -> &DpuConfig {
        let (1, Some(single_dpu)) = (self.dpus.len(), self.dpus.first()) else {
            panic!("Expected a single-DPU host, got {} DPUs", self.dpus.len());
        };
        single_dpu
    }

    pub fn exploration_results(
        &self,
        host_bmc_ip: Option<IpAddr>,
        dpu_bmc_ips: &[(u8, IpAddr)],
    ) -> eyre::Result<ManagedHostExplorationResults> {
        let mut dpu_reports = Vec::new();

        for (dpu_index, bmc_ip) in dpu_bmc_ips {
            let dpu = self
                .dpus
                .get(*dpu_index as usize)
                .ok_or_else(|| eyre::eyre!("DPU index {dpu_index} is not in the managed host"))?;
            let mut report: EndpointExplorationReport = dpu.clone().into();
            report.generate_machine_id(false)?;
            dpu_reports.push(ManagedDpuExplorationReport {
                dpu_index: *dpu_index,
                bmc_ip: *bmc_ip,
                report,
            });
        }

        let host_report = host_bmc_ip.map(|host_bmc_ip| (host_bmc_ip, self.clone().into()));

        Ok(ManagedHostExplorationResults {
            host_report,
            dpu_reports,
        })
    }
}

impl From<&ManagedHostConfig> for HardwareInfo {
    fn from(config: &ManagedHostConfig) -> Self {
        let mut info =
            serde_json::from_slice::<HardwareInfo>(match config.hardware_info_template {
                HardwareInfoTemplate::Default => X86_INFO_JSON,
                HardwareInfoTemplate::Custom(data) => data,
            })
            .unwrap();
        info.tpm_ek_certificate = Some(config.tpm_ek_cert.clone());
        info.dmi_data.as_mut().unwrap().product_serial = config.serial.clone();
        info.dmi_data.as_mut().unwrap().chassis_serial = config.serial.clone();
        info.network_interfaces = config
            .dpus
            .iter()
            .map(|d| NetworkInterface {
                mac_address: d.host_mac_address,
                pci_properties: Some(PciDeviceProperties {
                    vendor: "mellanox".to_string(),
                    device: "DPU1".to_string(),
                    path: "/x/y/z".to_string(),
                    numa_node: 1,
                    description: None,
                    slot: None,
                }),
            })
            .chain(config.non_dpu_macs.iter().map(|m| NetworkInterface {
                mac_address: *m,
                pci_properties: None,
            }))
            .collect();
        // Generate a unique GUID for each InfiniBand interface in the template.
        // For the moment this only supports hosts with a fixed amount of 6 interfaces.
        assert_eq!(
            config.ib_guids.len(),
            REQUIRED_IB_GUIDS,
            "The amount of {} IB GUIDs passed to the config does not match the {} GUIDs required by the test_data template",
            config.ib_guids.len(),
            REQUIRED_IB_GUIDS
        );
        for (ib_interface, guid) in info
            .infiniband_interfaces
            .iter_mut()
            .zip(config.ib_guids.iter())
        {
            ib_interface.guid = guid.clone();
        }
        info
    }
}

impl From<ManagedHostConfig> for EndpointExplorationReport {
    fn from(value: ManagedHostConfig) -> Self {
        let next_nic_index = value.dpus.len() + 1;

        let network_adapters = value
            .dpus
            .iter()
            .enumerate()
            .map(|(index, dpu)| NetworkAdapter {
                id: format!("slot-{}", index + 1),
                manufacturer: Some("MLNX".to_string()),
                model: Some("BlueField-3 P-Series DPU 200GbE/".to_string()),
                part_number: Some("900-9D3B6-00CV-A".to_string()),
                serial_number: Some(dpu.serial.clone()),
                port_mac_addresses: Vec::new(),
            })
            .chain(iter::once(NetworkAdapter {
                id: format!("slot-{next_nic_index}"),
                manufacturer: Some("Broadcom Limited".to_string()),
                model: Some("5720".to_string()),
                part_number: Some("SN30L21970".to_string()),
                serial_number: Some("L2NV97J018G".to_string()),
                port_mac_addresses: Vec::new(),
            }))
            .collect();

        let pcie_devices = value
            .dpus
            .iter()
            .map(|dpu| PCIeDevice {
                description: None,
                firmware_version: None,
                id: None,
                manufacturer: None,
                gpu_vendor: None,
                name: None,
                part_number: Some("900-9D3B6-00CV-A".to_string()),
                serial_number: Some(dpu.serial.clone()),
                status: None,
            })
            .collect::<Vec<_>>();

        let systems_ethernet_interfaces = value
            .non_dpu_macs
            .iter()
            .enumerate()
            .map(|(index, mac)| {
                let port = index + 1;
                EthernetInterface {
                    id: Some(format!("NIC.Embedded.{port}-1-1")),
                    description: Some(format!("Embedded NIC 1 Port {port} Partition 1")),
                    interface_enabled: Some(true),
                    mac_address: Some(*mac),
                    link_status: None,
                    uefi_device_path: None,
                }
            })
            .chain(value.dpus.iter().enumerate().map(|(index, dpu)| {
                let slot = index + 5; // DPUs start with 5.
                EthernetInterface {
                    id: Some(format!("NIC.Slot.{slot}-1")),
                    description: Some(format!("NIC in Slot {slot} Port 1")),
                    interface_enabled: Some(true),
                    mac_address: Some(dpu.host_mac_address),
                    link_status: None,
                    uefi_device_path: Some(
                        dpu.override_hosts_uefi_device_path.clone().unwrap_or(
                            UefiDevicePath::from_str(&format!(
                                "PciRoot(0x8)/Pci(0x2,0xa)/Pci(0x0,0x{:x})/MAC({},0x1)",
                                index + 1,
                                dpu.host_mac_address.to_string().replace(':', ""),
                            ))
                            .unwrap(),
                        ),
                    ),
                }
            }))
            .collect_vec();

        Self {
            endpoint_type: EndpointType::Bmc,
            last_exploration_error: None,
            last_exploration_latency: None,
            vendor: value.vendor,
            managers: vec![Manager {
                id: "iDRAC.Embedded.1".to_string(),
                ipmi_port: None,
                ethernet_interfaces: vec![EthernetInterface {
                    id: Some("NIC.1".to_string()),
                    description: Some("Management Network Interface".to_string()),
                    interface_enabled: Some(true),
                    mac_address: Some(value.bmc_mac_address),
                    link_status: None,
                    uefi_device_path: None,
                }],
            }],
            systems: vec![ComputerSystem {
                id: "System.Embedded.1".to_string(),
                manufacturer: Some("Dell Inc.".to_string()),
                model: Some("PowerEdge R750".to_string()),
                serial_number: Some(value.serial.clone()),
                ethernet_interfaces: systems_ethernet_interfaces,
                attributes: ComputerSystemAttributes::default(),
                pcie_devices,
                base_mac: None,
                power_state: PowerState::On,
                sku: None,
                boot_order: None,
                bios_version: None,
                serial_console_ssh_port: None,
            }],
            chassis: vec![Chassis {
                id: "System.Embedded.1".to_string(),
                manufacturer: Some("Dell Inc.".to_string()),
                model: Some("PowerEdge R750".to_string()),
                part_number: Some("SB27A42862".to_string()),
                serial_number: Some(value.serial),
                network_adapters,
                compute_tray_index: None,
                physical_slot_number: None,
                revision_id: None,
                topology_id: None,
            }],
            service: vec![Service {
                id: "FirmwareInventory".to_string(),
                inventories: vec![
                    Inventory {
                        id: "Installed-__iDRACz".to_string(),
                        description: Some("The information of BMC (Primary) firmware.".to_string()),
                        version: Some("5.10.20".to_string()),
                        release_date: None,
                    },
                    Inventory {
                        id: "Current-159-1.13.2__BIOS.Setup.1-1".to_string(),
                        description: Some("The information of Firmware firmware.".to_string()),
                        version: Some("1.12.0".to_string()),
                        release_date: None,
                    },
                ],
            }],
            machine_id: None,
            versions: Default::default(),
            model: None,
            machine_setup_status: None,
            secure_boot_status: None,
            lockdown_status: None,
            power_shelf_id: None,
            switch_id: None,
            physical_slot_number: None,
            compute_tray_index: None,
            revision_id: None,
            topology_id: None,
            remediation_error: None,
        }
    }
}
