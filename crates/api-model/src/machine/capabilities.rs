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
use std::fmt;

use carbide_uuid::machine::MachineId;
use serde::{Deserialize, Serialize};

use super::infiniband::MachineInfinibandStatusObservation;
use crate::hardware_info::{CpuInfo, InfinibandInterface, is_mnnvl_capable_gpu};
use crate::machine::{HardwareInfo, MachineInterfaceSnapshot};

lazy_static::lazy_static! {
    static ref BLOCK_STORAGE_REGEX: regex::Regex = regex::Regex::new(r"(Virtual_CDROM\d+|Virtual_SD\d+|NO_MODEL|LOGICAL_VOLUME)").unwrap();
    static ref NVME_STORAGE_REGEX: regex::Regex = regex::Regex::new(r"(NO_MODEL|LOGICAL_VOLUME)").unwrap();
}

/* ********************************** */
/*        MachineCapabilityType       */
/* ********************************** */

/// MachineCapabilityType represents a category
/// of machine capability.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub enum MachineCapabilityType {
    #[default]
    Cpu,
    Gpu,
    Memory,
    Storage,
    Network,
    Infiniband,
    Dpu,
}

impl fmt::Display for MachineCapabilityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineCapabilityType::Cpu => write!(f, "CPU"),
            MachineCapabilityType::Gpu => write!(f, "GPU"),
            MachineCapabilityType::Memory => write!(f, "MEMORY"),
            MachineCapabilityType::Storage => write!(f, "STORAGE"),
            MachineCapabilityType::Network => write!(f, "NETWORK"),
            MachineCapabilityType::Infiniband => write!(f, "INFINIBAND"),
            MachineCapabilityType::Dpu => write!(f, "DPU"),
        }
    }
}

/* ********************************** */
/*         MachineCapabilityCpu       */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityCpu {
    /// CPU model name
    pub name: String,
    /// number of sockets
    pub count: u32,
    /// CPU vendor name
    pub vendor: Option<String>,
    /// cores per socket
    pub cores: Option<u32>,
    /// threads per socket
    pub threads: Option<u32>,
}

impl From<&CpuInfo> for MachineCapabilityCpu {
    fn from(src: &CpuInfo) -> Self {
        MachineCapabilityCpu {
            name: src.model.clone(),
            count: src.sockets,
            vendor: Some(src.vendor.clone()),
            cores: Some(src.cores),
            threads: Some(src.threads),
        }
    }
}

/* ********************************** */
/*         MachineCapabilityGpu       */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityGpu {
    pub name: String,
    pub count: u32,
    pub vendor: Option<String>,
    pub frequency: Option<String>,
    pub memory_capacity: Option<String>,
    pub cores: Option<u32>,
    pub threads: Option<u32>,
    pub device_type: Option<MachineCapabilityDeviceType>,
}

/* ********************************** */
/*       MachineCapabilityMemory      */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityMemory {
    pub name: String,
    pub count: u32,
    pub vendor: Option<String>,
    pub capacity: Option<String>,
}

/* ********************************** */
/*       MachineCapabilityStorage     */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityStorage {
    pub name: String,
    pub count: u32,
    pub vendor: Option<String>,
    pub capacity: Option<String>,
}

/* ********************************** */
/*       MachineCapabilityNetwork     */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityNetwork {
    pub name: String,
    pub count: u32,
    pub vendor: Option<String>,
    pub device_type: Option<MachineCapabilityDeviceType>,
}

/* ********************************** */
/*     MachineCapabilityInfiniband    */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityInfiniband {
    pub name: String,
    pub count: u32,
    pub vendor: String,
    /// The indexes of InfiniBand Devices which are not active and thereby can
    /// not be utilized by Instances.
    /// Inactive devices are devices where for example there is no connection
    /// between the port and the InfiniBand switch.
    /// Example: A `{count: 4, inactive_devices: [1,3]}` means that the devices
    /// with index `0` and `2` of the Host can be utilized, and devices with index
    /// `1` and `3` can not be used.
    pub inactive_devices: Vec<u32>,
}

impl MachineCapabilityInfiniband {
    /// Derives a Machines Infiniband capabilities based on a hardware snapshot
    /// and the current InfiniBand connection status
    pub fn from_ib_interfaces_and_status(
        infiniband_interfaces: &[InfinibandInterface],
        ib_status: Option<&MachineInfinibandStatusObservation>,
    ) -> Vec<Self> {
        // IB interfaces get sorted by PCI Slot ID so that the inactive device
        // indices can be derived correctly
        let mut sorted_ib_interfaces = infiniband_interfaces.to_vec();
        sorted_ib_interfaces.sort_by_key(|iface| match &iface.pci_properties {
            Some(pci_properties) => pci_properties.slot.clone().unwrap_or_default(),
            None => "".to_owned(),
        });
        let mut infiniband_interface_map = HashMap::<String, MachineCapabilityInfiniband>::new();

        for infiniband_interface_info in sorted_ib_interfaces.iter() {
            // Skip any interface where we can't get PCI details.
            // This is how this data is handled in forge-cloud, but
            // does it make sense here?
            let pci_properties = match infiniband_interface_info.pci_properties.as_ref() {
                None => continue,
                Some(p) => p,
            };

            let interface_name = match pci_properties.description.as_ref() {
                None => continue,
                Some(n) => n.clone(),
            };

            // Check if the we have an observation for this device on UFM
            let is_active = ib_status
                .as_ref()
                .and_then(|ib_status| {
                    ib_status
                        .ib_interfaces
                        .iter()
                        .find(|iface| iface.guid == infiniband_interface_info.guid)
                })
                .map(|port_status| port_status.lid != 0xffff_u16)
                .unwrap_or_default();

            let cap = infiniband_interface_map
                .entry(interface_name.clone())
                .or_insert_with(|| MachineCapabilityInfiniband {
                    name: interface_name,
                    count: 0,
                    vendor: pci_properties.vendor.clone(),
                    inactive_devices: Vec::new(),
                });
            cap.count += 1;
            if !is_active {
                cap.inactive_devices.push(cap.count - 1);
            }
        }

        infiniband_interface_map.into_values().collect()
    }
}

/* ********************************** */
/*         MachineCapabilityDpu       */
/* ********************************** */

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct MachineCapabilityDpu {
    pub name: String,
    pub count: u32,
    pub hardware_revision: Option<String>,
}

/* ********************************** */
/*       MachineCapabilitiesSet       */
/* ********************************** */

/// A combined set of all known machine capabilities.
/// The content depends on the original source of the data,
/// and it's expected that that sources could include some
/// combination of discovery, topology, and BOM data.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct MachineCapabilitiesSet {
    pub cpu: Vec<MachineCapabilityCpu>,
    pub gpu: Vec<MachineCapabilityGpu>,
    pub memory: Vec<MachineCapabilityMemory>,
    pub storage: Vec<MachineCapabilityStorage>,
    pub network: Vec<MachineCapabilityNetwork>,
    pub infiniband: Vec<MachineCapabilityInfiniband>,
    pub dpu: Vec<MachineCapabilityDpu>,
}

/* ********************************************* */
/*       MachineCapabilityDeviceType       */
/* ********************************************* */

/// MachineCapabilityDeviceType describes different types of network devices.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum MachineCapabilityDeviceType {
    Unknown,
    Dpu,
    NvLink,
}

impl fmt::Display for MachineCapabilityDeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineCapabilityDeviceType::Unknown => write!(f, "UNKNOWN"),
            MachineCapabilityDeviceType::Dpu => write!(f, "DPU"),
            MachineCapabilityDeviceType::NvLink => write!(f, "NVLINK"),
        }
    }
}

impl MachineCapabilitiesSet {
    /// The arrays in each property of a capability set are not guaranteed to
    /// to have deterministic ordering, which is probably fine for most cases.
    /// When deterministic ordering is required, this function can be used to
    /// sorts the vectors found in each property of the capability set.
    pub fn sort(&mut self) {
        self.cpu.sort();
        self.gpu.sort();
        self.storage.sort();
        self.memory.sort();
        self.infiniband.sort();
        self.network.sort();
        self.dpu.sort();
    }

    pub fn from_hardware_info(
        hardware_info: HardwareInfo,
        ib_status: Option<&MachineInfinibandStatusObservation>,
        dpu_machine_ids: Vec<MachineId>,
        machine_interfaces: Vec<MachineInterfaceSnapshot>,
    ) -> Self {
        //
        //  Process GPU data
        //

        let mut gpu_map = HashMap::<String, MachineCapabilityGpu>::new();

        let dmi_product_name = hardware_info
            .dmi_data
            .as_ref()
            .map(|dmi| dmi.product_name.as_str());

        for gpu_info in hardware_info.gpus.into_iter() {
            let device_type = if is_mnnvl_capable_gpu(&gpu_info, dmi_product_name) {
                Some(MachineCapabilityDeviceType::NvLink)
            } else {
                Some(MachineCapabilityDeviceType::Unknown)
            };
            match gpu_map.get_mut(&gpu_info.name) {
                None => {
                    gpu_map.insert(
                        gpu_info.name.clone(),
                        MachineCapabilityGpu {
                            name: gpu_info.name,
                            count: 1,
                            vendor: None, // hardware_info doesn't provide this.
                            frequency: Some(gpu_info.frequency),
                            cores: None,   // hardware_info doesn't provide this.
                            threads: None, // hardware_info doesn't provide this.
                            memory_capacity: Some(gpu_info.total_memory),
                            device_type,
                        },
                    );
                }
                Some(gpu_cap) => {
                    gpu_cap.count += 1;
                }
            };
        }

        //
        //  Process memory data
        //

        let mut mem_map = HashMap::<String, usize>::new();

        for mem_info in hardware_info.memory_devices.into_iter() {
            let name = mem_info.mem_type.unwrap_or("unknown".to_string());

            mem_map
                .entry(name.clone())
                .and_modify(|e| {
                    *e = e.saturating_add(mem_info.size_mb.unwrap_or_default() as usize)
                })
                .or_insert_with(|| mem_info.size_mb.unwrap_or_default() as usize);
        }

        //
        // Process storage data.
        // NVME and block storage get flattened out into just "storage"
        //

        let mut storage_map = HashMap::<String, MachineCapabilityStorage>::new();

        // Start with any NVME devices.
        for storage_info in hardware_info.nvme_devices.into_iter() {
            // Skip missing models, logical volumes, and virtual storage.
            if NVME_STORAGE_REGEX.is_match(&storage_info.model) {
                continue;
            }

            match storage_map.get_mut(&storage_info.model) {
                None => {
                    storage_map.insert(
                        storage_info.model.clone(),
                        MachineCapabilityStorage {
                            name: storage_info.model.clone(),
                            count: 1,
                            vendor: None,   // hardware_info doesn't provide this.
                            capacity: None, // hardware_info doesn't provide this.
                        },
                    );
                }
                Some(storage_cap) => {
                    storage_cap.count += 1;
                }
            };
        }

        // Next, add in any block storage devices.
        for storage_info in hardware_info.block_devices.into_iter() {
            // Skip missing models, logical volumes, and virtual storage.
            if BLOCK_STORAGE_REGEX.is_match(&storage_info.model) {
                continue;
            }

            match storage_map.get_mut(&storage_info.model) {
                None => {
                    storage_map.insert(
                        storage_info.model.clone(),
                        MachineCapabilityStorage {
                            name: storage_info.model.clone(),
                            count: 1,
                            vendor: None,   // hardware_info doesn't provide this.
                            capacity: None, // hardware_info doesn't provide this.
                        },
                    );
                }
                Some(storage_cap) => {
                    storage_cap.count += 1;
                }
            };
        }

        //
        // Process network interface data
        //

        let mut network_interface_map = HashMap::<String, MachineCapabilityNetwork>::new();

        for network_interface_info in hardware_info.network_interfaces.into_iter() {
            // Skip any interface where we can't get PCI details.
            // This is how this data is handled in forge-cloud, but
            // does it make sense here?
            let pci_properties = match network_interface_info.pci_properties {
                None => continue,
                Some(p) => p,
            };

            let interface_name = match pci_properties.description {
                None => continue,
                Some(n) => n,
            };
            let device_type = match machine_interfaces.iter().find(|i| {
                i.mac_address == network_interface_info.mac_address
                    && i.attached_dpu_machine_id.is_some()
            }) {
                None => MachineCapabilityDeviceType::Unknown,
                Some(_i) => MachineCapabilityDeviceType::Dpu,
            };

            match network_interface_map.get_mut(&interface_name) {
                None => {
                    network_interface_map.insert(
                        interface_name.clone(),
                        MachineCapabilityNetwork {
                            name: interface_name.clone(),
                            count: 1,
                            vendor: Some(pci_properties.vendor),
                            device_type: Some(device_type),
                        },
                    );
                }
                Some(network_interface_cap) => {
                    network_interface_cap.count += 1;
                }
            };
        }

        //
        // Process infiniband data
        //

        let infiniband = MachineCapabilityInfiniband::from_ib_interfaces_and_status(
            &hardware_info.infiniband_interfaces,
            ib_status,
        );

        MachineCapabilitiesSet {
            cpu: hardware_info
                .cpu_info
                .iter()
                .map(MachineCapabilityCpu::from)
                .collect(),
            gpu: gpu_map.into_values().collect(),
            memory: mem_map
                .drain()
                .map(|(mem_type, mem_sum_mb)| MachineCapabilityMemory {
                    name: mem_type,
                    vendor: None, // hardware_info doesn't provide this
                    count: 1,     // We roll up all the memory we find
                    capacity: Some(format!("{mem_sum_mb} MB")),
                })
                .collect(),
            storage: storage_map.into_values().collect(),
            network: network_interface_map.into_values().collect(),
            infiniband,
            dpu: if dpu_machine_ids.is_empty() {
                vec![]
            } else {
                vec![MachineCapabilityDpu {
                    // This name value is what forge-cloud currently does/expects from machine capabilities.
                    // It needs to have _something_ that won't change.  If we decide to start
                    // pulling actual DPU details in the future, it would probably require
                    // forge cloud to also start allowing `name` as an optional field
                    // for instance type capability filters, and we'd have to update existing
                    // instance types in cloud to drop the `name` value while we transition.
                    name: "DPU".to_string(),
                    count: dpu_machine_ids.len().try_into().unwrap_or_else(|e| {
                        tracing::warn!(
                            error=%e,
                            "associated_dpu_machine_ids length uncountable for DPU capability",
                        );
                        0
                    }),
                    hardware_revision: None,
                }]
            },
        }
    }
}

/* ********************************** */
/*              Tests                 */
/* ********************************** */

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_test_support::{Check, value_scenarios};

    use super::*;
    use crate::hardware_info::*;
    use crate::ib::DEFAULT_IB_FABRIC_NAME;
    use crate::machine::MachineInterfaceId;
    use crate::machine::infiniband::MachineIbInterfaceStatusObservation;
    use crate::machine_interface::InterfaceType;
    use crate::{MacAddress, NetworkSegmentId};

    const X86_INFO_JSON: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/hardware_info/test_data/x86_info.json"
    ));

    #[test]
    fn test_model_capability_set_from_hw_info_conversion() {
        let mut machine_cap = MachineCapabilitiesSet {
            cpu: vec![MachineCapabilityCpu {
                name: "Intel(R) Xeon(R) Gold 6354 CPU @ 3.00GHz".to_string(),
                count: 1,
                vendor: Some("GenuineIntel".to_string()),
                cores: Some(18),
                threads: Some(72),
            }],
            gpu: vec![MachineCapabilityGpu {
                name: "NVIDIA H100 PCIe".to_string(),
                count: 1,
                vendor: None,
                frequency: Some("1755 MHz".to_string()),
                memory_capacity: Some("81559 MiB".to_string()),
                cores: None,
                threads: None,
                device_type: Some(MachineCapabilityDeviceType::Unknown),
            }],
            memory: vec![MachineCapabilityMemory {
                name: "DDR4".to_string(),
                count: 1,
                vendor: None,
                capacity: Some("2048 MB".to_string()),
            }],
            storage: vec![
                MachineCapabilityStorage {
                    name: "DELLBOSS_VD".to_string(),
                    count: 3,
                    vendor: None,
                    capacity: None,
                },
                MachineCapabilityStorage {
                    name: "Dell Ent NVMe CM6 RI 1.92TB".to_string(),
                    count: 10,
                    vendor: None,
                    capacity: None,
                },
            ],
            network: vec![
                MachineCapabilityNetwork {
                    name: "BCM57414 NetXtreme-E 10Gb/25Gb RDMA Ethernet Controller".to_string(),
                    count: 2,
                    vendor: Some("0x14e4".to_string()),
                    device_type: Some(MachineCapabilityDeviceType::Unknown),
                },
                MachineCapabilityNetwork {
                    name: "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"
                        .to_string(),
                    count: 2,
                    vendor: Some("mellanox".to_string()),
                    device_type: Some(MachineCapabilityDeviceType::Dpu),
                },
                MachineCapabilityNetwork {
                    name:
                        "NetXtreme BCM5720 2-port Gigabit Ethernet PCIe (PowerEdge Rx5xx LOM Board)"
                            .to_string(),
                    count: 2,
                    vendor: Some("0x14e4".to_string()),
                    device_type: Some(MachineCapabilityDeviceType::Unknown),
                },
            ],
            infiniband: vec![
                MachineCapabilityInfiniband {
                    name: "MT27800 Family [ConnectX-5]".to_string(),
                    count: 2,
                    vendor: "0x15b3".to_string(),
                    inactive_devices: vec![0, 1],
                },
                MachineCapabilityInfiniband {
                    name: "MT2910 Family [ConnectX-7]".to_string(),
                    count: 4,
                    vendor: "0x15b3".to_string(),
                    inactive_devices: vec![0, 1, 2, 3],
                },
            ],
            dpu: vec![MachineCapabilityDpu {
                name: "DPU".to_string(),
                count: 2,
                hardware_revision: None,
            }],
        };

        // The capabilities are built using hashmaps, so
        // the ordering of the final arrays isn't guaranteed.

        machine_cap.sort();

        let mut compare_cap = MachineCapabilitiesSet::from_hardware_info(
            serde_json::from_slice::<HardwareInfo>(X86_INFO_JSON).unwrap(),
            None,
            vec![
                "fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80"
                    .parse()
                    .unwrap(),
                "fm100dsmu2vhi1042hb8lrunopesh641tiguh6uttjr780ghbk9orl5tcg0"
                    .parse()
                    .unwrap(),
            ],
            vec![
                MachineInterfaceSnapshot {
                    id: MachineInterfaceId::from(uuid::Uuid::nil()),
                    hostname: String::new(),
                    interface_type: InterfaceType::Data,
                    primary_interface: true,
                    mac_address: MacAddress::from_str("08:c0:eb:cb:0e:96").unwrap(),
                    boot_interface_id: None,
                    attached_dpu_machine_id: Some(
                        MachineId::from_str(
                            "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0",
                        )
                        .unwrap(),
                    ),
                    domain_id: None,
                    machine_id: None,
                    segment_id: NetworkSegmentId::from(uuid::Uuid::nil()),
                    vendors: Vec::new(),
                    created: chrono::Utc::now(),
                    last_dhcp: None,
                    addresses: Vec::new(),
                    network_segment_type: None,
                    power_shelf_id: None,
                    switch_id: None,
                    association_type: None,
                },
                MachineInterfaceSnapshot {
                    id: MachineInterfaceId::from(uuid::Uuid::nil()),
                    hostname: String::new(),
                    interface_type: InterfaceType::Data,
                    primary_interface: true,
                    mac_address: MacAddress::from_str("08:c0:eb:cb:0e:97").unwrap(),
                    boot_interface_id: None,
                    attached_dpu_machine_id: Some(
                        MachineId::from_str(
                            "fm100dsg23d2f4tq4tt5m2hgib5pcldrm3gvefbduau7gj3itgc3iqg3lpg",
                        )
                        .unwrap(),
                    ),
                    domain_id: None,
                    machine_id: None,
                    segment_id: NetworkSegmentId::from(uuid::Uuid::nil()),
                    vendors: Vec::new(),
                    created: chrono::Utc::now(),
                    last_dhcp: None,
                    addresses: Vec::new(),
                    network_segment_type: None,
                    power_shelf_id: None,
                    switch_id: None,
                    association_type: None,
                },
            ],
        );

        compare_cap.sort();

        assert_eq!(machine_cap, compare_cap);
    }

    #[test]
    fn test_model_infinityband_capability_fully_connected() {
        let mut expected_ib_caps = vec![
            MachineCapabilityInfiniband {
                name: "MT27800 Family [ConnectX-5]".to_string(),
                count: 2,
                vendor: "0x15b3".to_string(),
                inactive_devices: vec![],
            },
            MachineCapabilityInfiniband {
                name: "MT2910 Family [ConnectX-7]".to_string(),
                count: 4,
                vendor: "0x15b3".to_string(),
                inactive_devices: vec![],
            },
        ];
        expected_ib_caps.sort();

        let ib_status = MachineInfinibandStatusObservation {
            ib_interfaces: vec![
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac100".to_string(),
                    lid: 1,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac101".to_string(),
                    lid: 2,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac102".to_string(),
                    lid: 3,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac103".to_string(),
                    lid: 4,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac752".to_string(),
                    lid: 5,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac753".to_string(),
                    lid: 6,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
            ],
            observed_at: chrono::Utc::now(),
        };

        let mut compare_cap = MachineCapabilitiesSet::from_hardware_info(
            serde_json::from_slice::<HardwareInfo>(X86_INFO_JSON).unwrap(),
            Some(&ib_status),
            vec![],
            vec![MachineInterfaceSnapshot {
                id: MachineInterfaceId::from(uuid::Uuid::nil()),
                hostname: String::new(),
                interface_type: InterfaceType::Data,
                primary_interface: true,
                mac_address: MacAddress::from_str("00:00:00:00:00:00").unwrap(),
                boot_interface_id: None,
                attached_dpu_machine_id: None,
                domain_id: None,
                machine_id: None,
                segment_id: NetworkSegmentId::from(uuid::Uuid::nil()),
                vendors: Vec::new(),
                created: chrono::Utc::now(),
                last_dhcp: None,
                addresses: Vec::new(),
                network_segment_type: None,
                power_shelf_id: None,
                switch_id: None,
                association_type: None,
            }],
        );

        compare_cap.sort();

        assert_eq!(expected_ib_caps, compare_cap.infiniband);
    }

    #[test]
    fn test_model_infinityband_capability_partially_connected() {
        let mut expected_ib_caps = vec![
            MachineCapabilityInfiniband {
                name: "MT27800 Family [ConnectX-5]".to_string(),
                count: 2,
                vendor: "0x15b3".to_string(),
                inactive_devices: vec![0],
            },
            MachineCapabilityInfiniband {
                name: "MT2910 Family [ConnectX-7]".to_string(),
                count: 4,
                vendor: "0x15b3".to_string(),
                inactive_devices: vec![1, 3],
            },
        ];
        expected_ib_caps.sort();

        let ib_status = MachineInfinibandStatusObservation {
            ib_interfaces: vec![
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac752".to_string(),
                    lid: 0xffff_u16,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac753".to_string(),
                    lid: 1,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac103".to_string(),
                    lid: 2,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac101".to_string(),
                    lid: 4,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
                MachineIbInterfaceStatusObservation {
                    guid: "946dae03002ac100".to_string(),
                    lid: 0xffff_u16,
                    fabric_id: DEFAULT_IB_FABRIC_NAME.to_string(),
                    associated_pkeys: None,
                    associated_partition_ids: None,
                },
            ],
            observed_at: chrono::Utc::now(),
        };

        let mut compare_cap = MachineCapabilitiesSet::from_hardware_info(
            serde_json::from_slice::<HardwareInfo>(X86_INFO_JSON).unwrap(),
            Some(&ib_status),
            vec![],
            vec![],
        );

        compare_cap.sort();

        assert_eq!(expected_ib_caps, compare_cap.infiniband);
    }

    #[test]
    fn capability_type_display_covers_every_variant() {
        value_scenarios!(
            run = |variant| variant.to_string();
            "cpu" {
                MachineCapabilityType::Cpu => "CPU".to_string(),
            }

            "gpu" {
                MachineCapabilityType::Gpu => "GPU".to_string(),
            }

            "memory" {
                MachineCapabilityType::Memory => "MEMORY".to_string(),
            }

            "storage" {
                MachineCapabilityType::Storage => "STORAGE".to_string(),
            }

            "network" {
                MachineCapabilityType::Network => "NETWORK".to_string(),
            }

            "infiniband" {
                MachineCapabilityType::Infiniband => "INFINIBAND".to_string(),
            }

            "dpu" {
                MachineCapabilityType::Dpu => "DPU".to_string(),
            }
        );
    }

    #[test]
    fn capability_type_default_is_cpu() {
        Check {
            scenario: "default capability type",
            input: (),
            expect: MachineCapabilityType::Cpu,
        }
        .check(|()| MachineCapabilityType::default());
    }

    #[test]
    fn device_type_display_covers_every_variant() {
        value_scenarios!(
            run = |variant| variant.to_string();
            "unknown" {
                MachineCapabilityDeviceType::Unknown => "UNKNOWN".to_string(),
            }

            "dpu" {
                MachineCapabilityDeviceType::Dpu => "DPU".to_string(),
            }

            "nvlink" {
                MachineCapabilityDeviceType::NvLink => "NVLINK".to_string(),
            }
        );
    }

    #[test]
    fn cpu_capability_from_cpu_info_maps_every_field() {
        fn cpu_info(model: &str, vendor: &str, sockets: u32, cores: u32, threads: u32) -> CpuInfo {
            CpuInfo {
                model: model.to_string(),
                vendor: vendor.to_string(),
                sockets,
                cores,
                threads,
            }
        }

        value_scenarios!(
            run = |info| MachineCapabilityCpu::from(&info);
            "typical dual-socket" {
                cpu_info("Xeon Gold 6354", "GenuineIntel", 2, 18, 36) => MachineCapabilityCpu {
                    name: "Xeon Gold 6354".to_string(),
                    count: 2,
                    vendor: Some("GenuineIntel".to_string()),
                    cores: Some(18),
                    threads: Some(36),
                },
            }

            "single socket" {
                cpu_info("EPYC 7763", "AuthenticAMD", 1, 64, 128) => MachineCapabilityCpu {
                    name: "EPYC 7763".to_string(),
                    count: 1,
                    vendor: Some("AuthenticAMD".to_string()),
                    cores: Some(64),
                    threads: Some(128),
                },
            }

            "all-zero / empty defaults" {
                cpu_info("", "", 0, 0, 0) => MachineCapabilityCpu {
                    name: String::new(),
                    count: 0,
                    // From always wraps the source fields in Some, even when empty/zero.
                    vendor: Some(String::new()),
                    cores: Some(0),
                    threads: Some(0),
                },
            }
        );
    }

    #[test]
    fn infiniband_caps_from_interfaces_and_status() {
        fn iface(guid: &str, pci: Option<PciDeviceProperties>) -> InfinibandInterface {
            InfinibandInterface {
                guid: guid.to_string(),
                pci_properties: pci,
            }
        }

        fn pci(vendor: &str, description: Option<&str>, slot: Option<&str>) -> PciDeviceProperties {
            PciDeviceProperties {
                vendor: vendor.to_string(),
                device: String::new(),
                path: String::new(),
                numa_node: 0,
                description: description.map(str::to_string),
                slot: slot.map(str::to_string),
            }
        }

        // No status observation: every device defaults to inactive.
        value_scenarios!(
            run = |interfaces| {
                MachineCapabilityInfiniband::from_ib_interfaces_and_status(&interfaces, None).len()
            };
            "empty interface list -> no capabilities" {
                Vec::<InfinibandInterface>::new() => 0,
            }

            "interface without pci_properties is skipped" {
                vec![iface("g0", None)] => 0,
            }

            "pci_properties without description is skipped" {
                vec![iface("g0", Some(pci("0x15b3", None, Some("0"))))] => 0,
            }

            "two devices of the same model roll into one capability" {
                vec![
                    iface("g0", Some(pci("0x15b3", Some("ConnectX-7"), Some("1")))),
                    iface("g1", Some(pci("0x15b3", Some("ConnectX-7"), Some("0")))),
                ] => 1,
            }

            "two distinct models produce two capabilities" {
                vec![
                    iface("g0", Some(pci("0x15b3", Some("ConnectX-5"), Some("0")))),
                    iface("g1", Some(pci("0x15b3", Some("ConnectX-7"), Some("1")))),
                ] => 2,
            }
        );
    }

    #[test]
    fn infiniband_caps_count_and_inactive_without_status() {
        // Without a status observation, lid lookups never succeed, so every
        // device is treated as inactive and recorded in inactive_devices.
        let interfaces = vec![
            InfinibandInterface {
                guid: "g0".to_string(),
                pci_properties: Some(PciDeviceProperties {
                    vendor: "0x15b3".to_string(),
                    device: String::new(),
                    path: String::new(),
                    numa_node: 0,
                    description: Some("ConnectX-7".to_string()),
                    slot: Some("0".to_string()),
                }),
            },
            InfinibandInterface {
                guid: "g1".to_string(),
                pci_properties: Some(PciDeviceProperties {
                    vendor: "0x15b3".to_string(),
                    device: String::new(),
                    path: String::new(),
                    numa_node: 0,
                    description: Some("ConnectX-7".to_string()),
                    slot: Some("1".to_string()),
                }),
            },
        ];

        let caps = MachineCapabilityInfiniband::from_ib_interfaces_and_status(&interfaces, None);
        assert_eq!(caps.len(), 1);

        value_scenarios!(
            run = |field| match field {
                "count" => caps[0].count,
                "inactive_len" => caps[0].inactive_devices.len() as u32,
                other => panic!("unknown field {other}"),
            };
            "rolled-up count" {
                "count" => 2u32,
            }

            "all inactive without status" {
                "inactive_len" => 2u32,
            }
        );
    }
}
