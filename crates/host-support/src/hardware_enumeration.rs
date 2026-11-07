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
use std::fmt::Write;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;
use std::str::Utf8Error;

use ::carbide_rpc_utils::machine_discovery::aggregate_cpus;
use ::carbide_utils::arch::{CpuArchitecture, UnsupportedCpuArchitecture};
use ::carbide_utils::cmd::CmdError;
use ::rpc::machine_discovery as rpc_discovery;
use base64::prelude::*;
use carbide_utils::{BF2_PRODUCT_NAME, BF3_PRODUCT_NAME};
use libudev::Device;
use procfs::{CpuInfo, FromRead};
use rpc::machine_discovery::{MemoryDevice, MemoryDeviceGroup};
use tracing::warn;
use uname::uname;

pub mod dpu;
mod gpu;
mod tpm;

/// Path where the init container writes the hardware snapshot, and the containerized agent reads it from.
pub const HW_CACHE_PATH: &str = "/data/hw_output.json";

pub(crate) const PCI_SUBCLASS: &str = "ID_PCI_SUBCLASS_FROM_DATABASE";
const PCI_DEV_PATH: &str = "DEVPATH";
const PCI_MODEL: &str = "ID_MODEL_FROM_DATABASE";
const PCI_SLOT_NAME: &str = "PCI_SLOT_NAME";
const MEMORY_TYPE: &str = "MEMORY_DEVICE_0_MEMORY_TECHNOLOGY";
const PCI_VENDOR_FROM_DB: &str = "ID_VENDOR_FROM_DATABASE";
const PCI_DEVICE_ID: &str = "ID_MODEL_ID";
const BF_PRODUCT_NAME_REGEX: &str = "BlueField";
const BF3_CPU_PART: &str = "0xd42";
const NVIDIA_VENDOR_ID: &str = "0x10de";
const NVIDIA_VENDOR_DRIVER: &str = "nvidia";

#[derive(thiserror::Error, Debug)]
pub enum HardwareEnumerationError {
    #[error("hardware enumeration error: {0}")]
    GenericError(String),
    #[error("udev failed with error: {0}")]
    UdevError(#[from] libudev::Error),
    #[error("udev string {0} is not a valid MAC address")]
    InvalidMacAddress(String),
    #[error("{0}")]
    UnsupportedCpuArchitecture(String),
    #[error("discovered memory device count {total} exceeds maximum of {max}")]
    MemoryDeviceCountExceeded { total: u64, max: u32 },
    #[error("command error {0}")]
    CmdError(#[from] CmdError),
}

pub type HardwareEnumerationResult<T> = Result<T, HardwareEnumerationError>;

pub const LINK_TYPE_P1: &str = "LINK_TYPE_P1";

#[derive(Debug)]
pub struct PciDevicePropertiesExt {
    pub sub_class: String,
    pub pci_properties: rpc_discovery::PciDeviceProperties,
    pub device_id: String,
}

impl PciDevicePropertiesExt {
    // This function decides on well known Mellanox PCI ids taken from the https://pci-ids.ucw.cz/read/PC/15b3
    // all BF DPUs start with 0xa2xx or 0xc2xx
    pub fn is_dpu(&self) -> bool {
        self.device_id.starts_with("0xa2") || self.device_id.starts_with("0xc2")
    }

    //pub fn mlnx_ib_capable(device: &str, pci_subclass: &str, vendor: &str) -> bool {
    pub fn mlnx_ib_capable(&self) -> bool {
        // Check only Mellanox port which is presented as a separate network interface
        // ID_PCI_CLASS_FROM_DATABASE='Network controller'
        //   - It is assumption for SUBSYSTEM=[net|infiniband]
        // ID_PCI_SUBCLASS_FROM_DATABASE='Infiniband controller' or 'Ethernet controller'
        //   - Because ports for VPI Mellanox device can be configured in IB(1) or ETH(2) type
        if let Some(slot) = self.pci_properties.slot.as_ref()
            && !slot.is_empty()
            && self
                .pci_properties
                .vendor
                .eq_ignore_ascii_case("Mellanox Technologies")
        {
            return self.sub_class.eq_ignore_ascii_case("Infiniband controller");
        }
        false
    }
}

impl TryFrom<&Device> for PciDevicePropertiesExt {
    type Error = HardwareEnumerationError;
    fn try_from(device: &Device) -> Result<Self, Self::Error> {
        let slot = match device.parent() {
            Some(parent) => convert_property_to_string(PCI_SLOT_NAME, "", &parent)?.to_string(),
            None => String::new(),
        };

        Ok(PciDevicePropertiesExt {
            sub_class: convert_property_to_string(PCI_SUBCLASS, "", device)?.to_string(),
            pci_properties: rpc_discovery::PciDeviceProperties {
                vendor: convert_property_to_string(PCI_VENDOR_FROM_DB, "NO_VENDOR_NAME", device)?
                    .to_string(),
                device: convert_property_to_string(PCI_MODEL, "NO_PCI_MODEL", device)?.to_string(),
                path: convert_property_to_string(PCI_DEV_PATH, "", device)?.to_string(),
                numa_node: get_numa_node_from_syspath(device.syspath())?,
                description: Some(
                    convert_property_to_string(PCI_MODEL, "NO_PCI_MODEL", device)?.to_string(),
                ),
                slot: Some(slot),
            },
            device_id: convert_property_to_string(PCI_DEVICE_ID, "", device)?.to_string(),
        })
    }
}

fn convert_udev_to_mac(udev: String) -> Result<String, HardwareEnumerationError> {
    // udevs format is enx112233445566 first, then the string of octets without a colon
    // remove the enx characters
    let (_, removed_enx) = udev.split_at(3);
    // chunk into 2 length
    let chunks = removed_enx
        .as_bytes()
        .chunks(2)
        .map(std::str::from_utf8)
        .collect::<Result<Vec<&str>, Utf8Error>>()
        .map_err(|_| HardwareEnumerationError::InvalidMacAddress(udev.clone()))?;
    // add colons
    let mut mac = chunks.into_iter().fold(String::new(), |mut s, chunk| {
        let _ = write!(s, "{chunk}:");
        s
    });
    // remove trailing colon from the above format
    mac.pop();

    Ok(mac)
}

pub fn convert_property_to_string<'a>(
    name: &'a str,
    default_value: &'a str,
    device: &'a Device,
) -> Result<&'a str, HardwareEnumerationError> {
    match device.property_value(name) {
        None => match default_value.is_empty() {
            true => Err(HardwareEnumerationError::GenericError(format!(
                "Could not find property {} on device {:?}",
                name,
                device.devpath()
            ))),
            false => Ok(default_value),
        },
        Some(p) => p.to_str().map(|s| s.trim()).ok_or_else(|| {
            HardwareEnumerationError::GenericError(format!(
                "Could not transform os string to string for property {} on device {:?}",
                name,
                device.devpath()
            ))
        }),
    }
}

fn convert_sysattr_to_string<'a>(
    name: &'a str,
    device: &'a Device,
) -> Result<&'a str, HardwareEnumerationError> {
    match device.attribute_value(name) {
        None => Ok(""),
        Some(p) => p.to_str().map(|s| s.trim()).ok_or_else(|| {
            HardwareEnumerationError::GenericError(format!(
                "Could not transform os string to string for attribute {name}"
            ))
        }),
    }
}

// NUMA_NODE is not exposed in libudev but the full path to a device is.
// We have to convert from String -> i32 which is full of cases where conversion
// can fail.
fn get_numa_node_from_syspath(syspath: Option<&Path>) -> Result<i32, HardwareEnumerationError> {
    let syspath = syspath
        .ok_or_else(|| HardwareEnumerationError::GenericError("Syspath is None".to_string()))?;
    let numa_node_full_path = syspath.join("device/numa_node");

    let file = fs::File::open(&numa_node_full_path).map_err(|e| {
        HardwareEnumerationError::GenericError(format!(
            "Failed to open {}: {}",
            numa_node_full_path.display(),
            e
        ))
    })?;

    let mut file_reader = BufReader::new(file);
    let mut numa_node_value = String::new();
    file_reader.read_line(&mut numa_node_value).map_err(|e| {
        HardwareEnumerationError::GenericError(format!(
            "Failed to read line from {}: {}",
            numa_node_full_path.display(),
            e
        ))
    })?;

    numa_node_value.trim().parse::<i32>().map_err(|e| {
        HardwareEnumerationError::GenericError(format!(
            "Failed to parse NUMA node value to i32: {e}"
        ))
    })
}

// Total capacity of an NVMe controller in MB, summed across its block
// namespaces. The `size` sysattr on a block device is a count of 512-byte
// sectors (1 MB == 2048 sectors). Returns None when no namespace size can be
// read, so callers can treat size as "unknown" rather than zero.
fn get_nvme_size_mb(context: &libudev::Context, controller: &Device) -> Option<u32> {
    let mut enumerator = libudev::Enumerator::new(context).ok()?;
    enumerator.match_subsystem("block").ok()?;
    enumerator.match_parent(controller).ok()?;

    let mut total_sectors = 0_u64;
    let mut found_namespace = false;
    for ns in enumerator.scan_devices().ok()? {
        // Only whole-namespace disks. `match_parent` walks the full block
        // subtree, so without this a partitioned drive would have its
        // partition sizes summed on top of the namespace size, inflating
        // the reported capacity.
        if ns.property_value("DEVTYPE").and_then(|v| v.to_str()) != Some("disk") {
            continue;
        }
        found_namespace = true;
        let sectors = ns
            .attribute_value("size")
            .and_then(|v| v.to_str())
            .and_then(|v| v.trim().parse::<u64>().ok())?;
        total_sectors = total_sectors.checked_add(sectors)?;
    }
    if !found_namespace {
        return None;
    }
    u32::try_from(total_sectors / 2048).ok()
}

// discovery all the non-DPU IB devices
pub fn discovery_ibs() -> HardwareEnumerationResult<Vec<rpc_discovery::InfinibandInterface>> {
    let device_debug_log = |device: &Device| {
        tracing::debug!(syspath = ?device.syspath(), "SysPath");
        for p in device.properties() {
            tracing::trace!(
                property_name = ?p.name(),
                property_value = ?p.value(),
                "Property"
            );
        }
        for a in device.attributes() {
            tracing::trace! {
                attribute_name = ?a.name(),
                attribute_value = ?a.value(),
                "attribute"
            }
        }
    };

    let context = libudev::Context::new()?;
    let mut ibs: Vec<rpc_discovery::InfinibandInterface> = Vec::new();
    let mut enumerator = libudev::Enumerator::new(&context)?;
    enumerator.match_subsystem("infiniband")?;
    let devices = enumerator.scan_devices()?;
    for device in devices {
        device_debug_log(&device);

        let properties_ext = match PciDevicePropertiesExt::try_from(&device) {
            Ok(properties_ext) => properties_ext,
            Err(e) => {
                tracing::error!(
                    device_path = ?device.devpath(),
                    error = %e,
                    "Failed to enumerate properties of device"
                );
                continue;
            }
        };

        // SUBSYSTEM=infiniband
        // Skip DPU
        if properties_ext.is_dpu() {
            continue;
        }

        // SUBSYSTEM=infiniband
        // ID_PCI_CLASS_FROM_DATABASE='Network controller'
        //   - It is assumption for SUBSYSTEM=[net|infiniband]
        // ID_PCI_SUBCLASS_FROM_DATABASE='Infiniband controller' or 'Ethernet controller'
        //   - because ports for VPI device can be configured in IB(1) or ETH(2) types
        if properties_ext.mlnx_ib_capable() {
            ibs.push(rpc_discovery::InfinibandInterface {
                guid: convert_sysattr_to_string("node_guid", &device)?
                    .to_string()
                    .replace(':', ""),
                pci_properties: Some(properties_ext.pci_properties),
            });
        }
    }
    Ok(ibs)
}

// `lscpu` fields in exact case-sensitive form
// if present, these fields can be assumed to have these standard names
const LSCPU_VENDOR: &str = "Vendor ID";
const LSCPU_MODEL: &str = "Model name";
const LSCPU_SOCKETS: &str = "Socket(s)";
const LSCPU_CORES_PER_SOCKET: &str = "Core(s) per socket";
const LSCPU_THREADS_PER_CORE: &str = "Thread(s) per core";

fn get_lscpu_info() -> HashMap<&'static str, String> {
    let keys = [
        LSCPU_VENDOR,
        LSCPU_MODEL,
        LSCPU_SOCKETS,
        LSCPU_CORES_PER_SOCKET,
        LSCPU_THREADS_PER_CORE,
    ];

    let mut lscpu_info: HashMap<&'static str, String> = HashMap::new();
    let output = Command::new("lscpu").output();

    if let Ok(out) = output
        && let Ok(text) = std::str::from_utf8(&out.stdout)
    {
        for line in text.lines() {
            // `lscpu` output format is "  <key>:   <value>" with
            // various levels of indentation before <key>
            if let Some((k, v)) = line.split_once(':') {
                let trimmed_key = k.trim();
                if let Some(key) = keys.iter().find(|&&s| s == trimmed_key).copied() {
                    lscpu_info.insert(key, v.trim().to_string());
                }
            }
        }
    }

    lscpu_info
}

fn can_parse_int(s: &str) -> bool {
    if let Some(hex) = s.strip_prefix("0x") {
        i32::from_str_radix(hex, 16).is_ok()
    } else {
        s.parse::<i32>().is_ok()
    }
}

fn get_cpu_info(
    lscpu_info: &HashMap<&'static str, String>,
    proc_cpu_info: rpc_discovery::CpuInfo,
) -> rpc_discovery::CpuInfo {
    // Prefer vendor from `lscpu` only if the value from procfs is an
    // unmapped integer.
    let preferred_vendor = if can_parse_int(&proc_cpu_info.vendor) {
        lscpu_info.get(LSCPU_VENDOR).cloned()
    } else {
        None
    };

    // Prefer model from `lscpu` only if the value from procfs is an
    // unmapped integer.
    let preferred_model = if can_parse_int(&proc_cpu_info.model) {
        lscpu_info.get(LSCPU_MODEL).cloned()
    } else {
        None
    };

    // Prefer topology from `lscpu` only if it completely specifies sockets,
    // cores, and threads (all or nothing).
    let (preferred_sockets, preferred_cores, preferred_threads) = match (
        lscpu_info
            .get(LSCPU_SOCKETS)
            .and_then(|s| s.parse::<u32>().ok()),
        lscpu_info
            .get(LSCPU_CORES_PER_SOCKET)
            .and_then(|s| s.parse::<u32>().ok()),
        lscpu_info
            .get(LSCPU_THREADS_PER_CORE)
            .and_then(|s| s.parse::<u32>().ok()),
    ) {
        (Some(s), Some(c), Some(t)) => (Some(s), Some(c), Some(c * t)),
        _ => (None, None, None),
    };

    rpc_discovery::CpuInfo {
        vendor: preferred_vendor.unwrap_or(proc_cpu_info.vendor),
        model: preferred_model.unwrap_or(proc_cpu_info.model),
        sockets: preferred_sockets.unwrap_or(proc_cpu_info.sockets),
        cores: preferred_cores.unwrap_or(proc_cpu_info.cores),
        threads: preferred_threads.unwrap_or(proc_cpu_info.threads),
    }
}

pub fn enumerate_hardware() -> Result<rpc_discovery::DiscoveryInfo, HardwareEnumerationError> {
    enumerate_hardware_inner("/proc/cpuinfo", "/proc/meminfo")
}

fn enumerate_hardware_inner(
    cpu_info_path: &str,
    mem_info_path: &str,
) -> Result<rpc_discovery::DiscoveryInfo, HardwareEnumerationError> {
    let context = libudev::Context::new()?;

    // uname to detect type
    let info = uname().map_err(|e| HardwareEnumerationError::GenericError(e.to_string()))?;
    let arch = info
        .machine
        .parse()
        .map_err(|e: UnsupportedCpuArchitecture| {
            HardwareEnumerationError::UnsupportedCpuArchitecture(e.0)
        })?;

    // IBs
    let ibs = discovery_ibs()?;

    // Nics
    let mut enumerator = libudev::Enumerator::new(&context)?;
    enumerator.match_subsystem("net")?;
    let devices = enumerator.scan_devices()?;

    // mellanox ID_MODEL_ID = "0xa2d6"
    // mellanox ID_VENDOR_FROM_DATABASE = "Mellanox Technologies"
    // mellanox ID_MODEL_FROM_DATABASE = "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller"
    // pci_device_path = DEVPATH = "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/net/enp8s0f0np0"
    // let fff = devices.map(|device|DiscoveryNic { mac: "".to_string(), dev: "".to_string() });
    let mut nics: Vec<rpc_discovery::NetworkInterface> = Vec::new();

    for device in devices {
        let sys_path = device.syspath();
        tracing::debug!(syspath = ?sys_path, "SysPath");
        for p in device.properties() {
            tracing::trace!(
                property_name = ?p.name(),
                property_value = ?p.value(),
                "net device property"
            );
        }
        //for a in device.attributes() {
        //    tracing::trace!("attribute - {:?} - {:?}", a.name(), a.value());
        //}

        if let Ok(pci_subclass) = convert_property_to_string(PCI_SUBCLASS, "", &device)
            && pci_subclass.eq_ignore_ascii_case("Ethernet controller")
        {
            let properties_ext = match PciDevicePropertiesExt::try_from(&device) {
                Ok(properties_ext) => properties_ext,
                Err(e) => {
                    tracing::error!(
                        device_path = ?device.devpath(),
                        error = %e,
                        "Failed to enumerate properties of device"
                    );
                    continue;
                }
            };

            tracing::trace!(?properties_ext, "properties");

            // discovery DPU and non ib capable device
            // Note:
            //   Probably current logic does not allow to detect non DPU network interfaces
            //   with following properties
            //     SUBSYSTEM=infiniband
            //     ID_PCI_CLASS_FROM_DATABASE='Network controller'
            //     ID_PCI_SUBCLASS_FROM_DATABASE='Ethernet controller'
            if properties_ext.is_dpu() || !properties_ext.mlnx_ib_capable() {
                nics.push(rpc_discovery::NetworkInterface {
                    mac_address: convert_udev_to_mac(
                        convert_property_to_string("ID_NET_NAME_MAC", &info.machine, &device)?
                            .to_string(),
                    )?,
                    pci_properties: Some(properties_ext.pci_properties),
                });
            }
        }
    }

    // cpus
    // TODO(baz): make this work with udev one day... I tried and it gave me useless information on the cpu subsystem
    let cpu_info = {
        let file = File::open(cpu_info_path)
            .map_err(|e| HardwareEnumerationError::GenericError(e.to_string()))?;
        let reader = BufReader::new(file);
        CpuInfo::from_read(reader)
            .map_err(|e| HardwareEnumerationError::GenericError(e.to_string()))?
    };

    let cpu_part = cpu_info
        .get_info(0)
        .and_then(|info| info.get("CPU part").copied())
        .map(str::to_string)
        .unwrap_or_default();

    let mut cpus: Vec<rpc_discovery::Cpu> = Vec::new();
    for cpu_num in 0..cpu_info.num_cores() {
        //tracing::debug!("CPU info: {:?}", cpu_info.get_info(cpu_num));
        match arch {
            CpuArchitecture::Aarch64 => {
                cpus.push(rpc_discovery::Cpu {
                    vendor: cpu_info
                        .get_info(cpu_num)
                        .and_then(|mut m| m.remove("CPU implementer"))
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get arm vendor name".to_string(),
                            )
                        })?
                        .to_string(),
                    model: cpu_info
                        .get_info(cpu_num)
                        .and_then(|mut m| m.remove("CPU variant"))
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get arm model name".to_string(),
                            )
                        })?
                        .to_string(),
                    frequency: cpu_info
                        .get_info(cpu_num)
                        .and_then(|mut m| m.remove("BogoMIPS"))
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get arm frequency".to_string(),
                            )
                        })?
                        .to_string(),
                    number: cpu_num as u32,
                    socket: 0,
                    core: 0,
                    node: 2,
                });
            }
            CpuArchitecture::X86_64 => {
                cpus.push(rpc_discovery::Cpu {
                    vendor: cpu_info
                        .vendor_id(cpu_num)
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get vendor name".to_string(),
                            )
                        })?
                        .to_string(),
                    model: cpu_info
                        .model_name(cpu_num)
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get model name".to_string(),
                            )
                        })?
                        .to_string(),
                    frequency: cpu_info
                        .get_info(cpu_num)
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get cpu info".to_string(),
                            )
                        })?
                        .get("cpu MHz")
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get cpu MHz field".to_string(),
                            )
                        })?
                        .to_string(),
                    number: cpu_num as u32,
                    socket: cpu_info.physical_id(cpu_num).ok_or_else(|| {
                        HardwareEnumerationError::GenericError("Could not get cpu info".to_string())
                    })?,
                    core: cpu_info
                        .get_info(cpu_num)
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get cpu info".to_string(),
                            )
                        })?
                        .get("core id")
                        .map(|c| c.parse::<u32>().unwrap_or(0))
                        .ok_or_else(|| {
                            HardwareEnumerationError::GenericError(
                                "Could not get cpu core id field".to_string(),
                            )
                        })?,
                    node: 0,
                });
            }
            CpuArchitecture::Unknown => {
                tracing::error!(
                    cpu_number = cpu_num,
                    arch = info.machine,
                    "CPU has unsupported architecture. Ignoring."
                );
            }
        }
    }

    let mut cpu_aggregation = aggregate_cpus(&cpus);
    if let CpuArchitecture::Aarch64 = arch {
        let lscpu_info = get_lscpu_info();
        cpu_aggregation = cpu_aggregation
            .into_iter()
            .map(|elem| get_cpu_info(&lscpu_info, elem))
            .collect();
    }

    // disks
    let mut enumerator = libudev::Enumerator::new(&context)?;
    enumerator.match_subsystem("block")?;
    let devices = enumerator.scan_devices()?;

    let mut disks: Vec<rpc_discovery::BlockDevice> = Vec::new();

    for device in devices {
        // tracing::info!("Block device syspath: {:?}", device.syspath());
        // for p in device.properties() {
        //     tracing::info!("prop:{:?} - {:?}", p.name(), p.value());
        // }
        // for p in device.attributes() {
        //     tracing::info!("attr:{:?} - {:?}", p.name(), p.value());
        // }

        // skip the device if its hidden
        if convert_sysattr_to_string("hidden", &device).is_ok_and(|v| v == "1") {
            tracing::info!(
                syspath = device
                    .syspath()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default(),
                "Ignoring hidden device"
            );
            continue;
        }

        // skip the device if its removable
        if convert_sysattr_to_string("removable", &device).is_ok_and(|v| v != "0") {
            tracing::info!(
                syspath = device
                    .syspath()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default(),
                "Ignoring removable device"
            );
            continue;
        }

        if convert_property_to_string(PCI_DEV_PATH, "", &device)
            .is_ok_and(|v| v.contains("virtual"))
        {
            tracing::info!(
                syspath = device
                    .syspath()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default(),
                "Ignoring virtual device"
            );
            continue;
        }

        disks.push(rpc_discovery::BlockDevice {
            model: convert_property_to_string("ID_MODEL", "NO_MODEL", &device)?.to_string(),
            revision: convert_property_to_string("ID_REVISION", "NO_REVISION", &device)?
                .to_string(),
            serial: convert_property_to_string("ID_SERIAL_SHORT", "NO_SERIAL", &device)?
                .to_string(),
            device_type: convert_property_to_string("DEVTYPE", "NO_TYPE", &device)?.to_string(),
        });
    }

    // Nvme
    let mut enumerator = libudev::Enumerator::new(&context)?;
    enumerator.match_subsystem("nvme")?;
    let devices = enumerator.scan_devices()?;

    let mut nvmes: Vec<rpc_discovery::NvmeDevice> = Vec::new();

    for device in devices {
        // tracing::info!("NVME device syspath: {:?}", device.syspath());
        // for p in device.properties() {
        //     tracing::info!("prop:{:?} - {:?}", p.name(), p.value());
        // }
        // for p in device.attributes() {
        //     tracing::info!("attr:{:?} - {:?}", p.name(), p.value());
        // }

        if device
            .property_value(PCI_DEV_PATH)
            .map(|v| v.to_str())
            .ok_or_else(|| {
                HardwareEnumerationError::GenericError("Could not decode DEVPATH".to_string())
            })?
            .filter(|v| !v.contains("virtual"))
            .is_some()
        {
            let pci_path = convert_property_to_string(PCI_DEV_PATH, "", &device)
                .ok()
                .map(|v| v.to_string());
            nvmes.push(rpc_discovery::NvmeDevice {
                model: convert_sysattr_to_string("model", &device)?.to_string(),
                firmware_rev: convert_sysattr_to_string("firmware_rev", &device)?.to_string(),
                serial: convert_sysattr_to_string("serial", &device)?
                    .trim()
                    .to_string(),
                size_mb: get_nvme_size_mb(&context, &device),
                pci_path,
            });
        }
    }

    // Dmi
    let mut enumerator = libudev::Enumerator::new(&context)?;
    enumerator.match_subsystem("dmi")?;
    let mut devices = enumerator.scan_devices()?;
    let mut backup_ram_type = None;
    let mut dmi = rpc_discovery::DmiData::default();
    // We only enumerate the first set of dmi data
    // There is only expected to be a single set, and we don't want to
    // accidentally overwrite it with other data
    if let Some(device) = devices.next() {
        tracing::debug!(syspath = ?device.syspath(), "DMI device syspath");

        // e.g. 'DRAM'. We will use this later if smbios fails.
        backup_ram_type = device
            .property_value(MEMORY_TYPE)
            .map(|v| v.to_string_lossy().to_string());

        if device
            .property_value(PCI_DEV_PATH)
            .map(|v| v.to_str())
            .ok_or_else(|| {
                HardwareEnumerationError::GenericError("Could not decode DEVPATH".to_string())
            })?
            .is_some()
        {
            //for a in device.attributes() {
            //    tracing::debug!("Attribute: {:?} - {:?}", a.name(), a.value());
            //}

            dmi.board_name = convert_sysattr_to_string("board_name", &device)?.to_string();
            dmi.board_version = convert_sysattr_to_string("board_version", &device)?.to_string();
            dmi.bios_version = convert_sysattr_to_string("bios_version", &device)?.to_string();
            dmi.bios_date = convert_sysattr_to_string("bios_date", &device)?.to_string();
            dmi.product_serial = convert_sysattr_to_string("product_serial", &device)?.to_string();
            dmi.product_name = convert_sysattr_to_string("product_name", &device)?.to_string();
            if cpu_part == BF3_CPU_PART && dmi.product_name == BF2_PRODUCT_NAME {
                tracing::info!(
                    product_name = dmi.product_name.as_str(),
                    new_product_name = BF3_PRODUCT_NAME,
                    "Overriding product name"
                );
                dmi.product_name = BF3_PRODUCT_NAME.to_owned();
            }
            dmi.sys_vendor = convert_sysattr_to_string("sys_vendor", &device)?.to_string();

            // TODO (spyda): reach out to the NBU team. We recently found DPUs that reports a
            // serial number for board_serial instead of what was previously found: "Unspecified Base Board Serial Number".
            // Figure out a longer term strategy to use all three serial numbers. Keeping the commented out code below for future reference.
            // Possible Values for dmi.product_name: BlueField SoC (BF2), BlueField-3 SmartNIC Main Card (BF3), BlueField-3 DPU (BF3)
            if dmi.product_name.contains(BF_PRODUCT_NAME_REGEX) {
                dmi.board_serial = carbide_utils::DEFAULT_DPU_DMI_BOARD_SERIAL_NUMBER.to_string();
                dmi.chassis_serial =
                    carbide_utils::DEFAULT_DPU_DMI_CHASSIS_SERIAL_NUMBER.to_string();
            } else {
                dmi.board_serial = convert_sysattr_to_string("board_serial", &device)?.to_string();
                dmi.chassis_serial =
                    convert_sysattr_to_string("chassis_serial", &device)?.to_string();
            }
        }
    }

    let is_dpu_dmi = dmi.product_name.contains(BF_PRODUCT_NAME_REGEX);
    let tpm_ek_certificate = match tpm::get_ek_certificate() {
        Ok(cert) => Some(BASE64_STANDARD.encode(cert)),
        Err(e) if !is_dpu_dmi && tpm::is_tpm_present() => {
            return Err(HardwareEnumerationError::GenericError(format!(
                "TPM is present but EK certificate collection failed; refusing serial fallback: {e}"
            )));
        }
        Err(e) => {
            tracing::error!(error = ?e, "Could not read TPM EK certificate");
            None
        }
    };

    let dpu_vpd = match dmi.sys_vendor.as_str() {
        "https://www.mellanox.com" | "Nvidia" => match dpu::get_dpu_info() {
            Ok(dpu_data) => Some(dpu_data),
            Err(e) => {
                tracing::error!(error = ?e, "Could not get DPU data");
                None
            }
        },
        _ => None,
    };

    let mut enumerator = libudev::Enumerator::new(&context)?;
    // It is currently assumed all GPUs are from vendor nvidia and use the nvidia driver
    enumerator.match_attribute("vendor", NVIDIA_VENDOR_ID)?;
    enumerator.match_attribute("driver", NVIDIA_VENDOR_DRIVER)?;

    let device_count = enumerator.scan_devices()?.count();

    // If there are no GPUs present on the host we do not want to run nvidia-smi as it will fail
    let gpus = if device_count > 0 {
        gpu::get_nvidia_smi_data()?
    } else {
        tracing::debug!("No GPUs detected, skipping");
        vec![]
    };

    let mut memory_devices = vec![];
    match smbioslib::table_load_from_device() {
        Ok(smbios_info) => {
            for i in smbios_info.collect::<smbioslib::SMBiosMemoryDevice>() {
                let size_mb = match i.size() {
                    Some(smbioslib::MemorySize::Kilobytes(size)) => size as u32 / 1024,
                    Some(smbioslib::MemorySize::Megabytes(size)) => size as u32,
                    Some(smbioslib::MemorySize::SeeExtendedSize) => {
                        match i.extended_size() {
                            Some(extended_size) => match extended_size {
                                smbioslib::MemorySizeExtended::Megabytes(size) => size,
                                smbioslib::MemorySizeExtended::SeeSize => 0u32, // size was already checked, just return 0
                            },
                            None => 0u32,
                        }
                    }
                    Some(smbioslib::MemorySize::NotInstalled) => 0u32,
                    Some(smbioslib::MemorySize::Unknown) => 0u32,
                    None => 0u32,
                };

                // do not include the module if any of the above conditions ended up with a 0.
                if size_mb == 0 {
                    continue;
                }

                let mem_type = match i.memory_type() {
                    Some(smbioslib::MemoryDeviceTypeData { value, .. }) => {
                        Some(format!("{value:?}").to_uppercase())
                    }
                    _ => backup_ram_type.clone(),
                };
                memory_devices.push(MemoryDevice {
                    size_mb: Some(size_mb),
                    mem_type,
                });
            }
        }
        Err(err) => {
            warn!(
                memory_info_path = mem_info_path,
                error = %err,
                "Could not discover host memory using smbios device"
            );
            let meminfo = std::fs::read_to_string(mem_info_path).map_err(|e| {
                HardwareEnumerationError::GenericError(format!("Err reading {mem_info_path}: {e}"))
            })?;
            let mem = parse_memtotal_kb(&meminfo);

            memory_devices.push(MemoryDevice {
                size_mb: Some(mem / 1024),
                mem_type: backup_ram_type,
            });
        }
    }

    tracing::debug!(?disks, "Discovered Disks");
    if !cpus.is_empty() {
        tracing::debug!(cpu = ?cpus[0], "Discovered CPUs[0]");
    }
    tracing::debug!(?nics, "Discovered NICS");
    tracing::debug!(?ibs, "Discovered IBS");
    tracing::debug!(?nvmes, "Discovered NVMES");
    tracing::debug!(?dmi, "Discovered DMI");
    tracing::debug!(?gpus, "Discovered GPUs");
    tracing::debug!(
        architecture = info.machine.as_str(),
        "Discovered Machine Architecture"
    );
    tracing::debug!(dpu_vpd = ?dpu_vpd, "Discovered DPU");
    if let Some(cert) = tpm_ek_certificate.as_ref() {
        tracing::debug!(certificate = cert.as_str(), "TPM EK certificate (base64)");
    }

    Ok(rpc_discovery::DiscoveryInfo {
        network_interfaces: nics,
        infiniband_interfaces: ibs,
        cpu_info: cpu_aggregation,
        block_devices: disks,
        nvme_devices: nvmes,
        dmi_data: Some(dmi),
        machine_type: arch.to_string(),
        machine_arch: Some(rpc::utils::cpu_architecture_to_rpc(arch)),
        tpm_ek_certificate,
        dpu_info: dpu_vpd,
        gpus,
        #[allow(deprecated)]
        memory_devices: vec![],
        memory_device_groups: condense_rpc_memory_devices(memory_devices)?,
        tpm_description: None,
        attest_key_info: None,
    })
}

/// Rolls up a flat list of RPC [`MemoryDevice`]s into [`MemoryDeviceGroup`]s, merging consecutive
/// devices with the same `(size_mb, mem_type)` into a single group.
///
/// Fails if the total device count exceeds [`MemoryDeviceGroup::MAX_REHYDRATE_COUNT`], so a
/// malformed SMBIOS table can't produce discovery data that only fails later, at RPC conversion.
fn condense_rpc_memory_devices(
    devices: Vec<MemoryDevice>,
) -> Result<Vec<MemoryDeviceGroup>, HardwareEnumerationError> {
    let groups = devices.into_iter().map(|device| MemoryDeviceGroup {
        size_mb: device.size_mb,
        mem_type: device.mem_type,
        count: 1,
    });
    carbide_utils::memory_device_group::condense_memory_device_groups(
        groups,
        MemoryDeviceGroup::MAX_REHYDRATE_COUNT,
        |total| HardwareEnumerationError::MemoryDeviceCountExceeded {
            total,
            max: MemoryDeviceGroup::MAX_REHYDRATE_COUNT,
        },
    )
}

/// Path where the host's `/proc/cpuinfo` is bind-mounted inside the init container.
const INIT_CPU_INFO_PATH: &str = "/host-cpu-info";

/// Path where the host's `/proc/meminfo` is bind-mounted inside the init container.
const INIT_MEM_INFO_PATH: &str = "/host-mem-info";

/// Validate that an enumerated [`rpc_discovery::DiscoveryInfo`] is complete
/// enough for downstream use. Returns `Err` describing the missing piece so
/// the caller can retry the probe.
///
/// Today the only required field is `dpu_info` — DPU VPD probing can race
/// with device init and produce a `None` if read too early.
fn validate_enumerated(
    info: &rpc_discovery::DiscoveryInfo,
) -> Result<(), HardwareEnumerationError> {
    if info.dpu_info.is_none() {
        return Err(HardwareEnumerationError::GenericError(
            "Hardware enumeration is missing dpu_info".to_string(),
        ));
    }
    Ok(())
}

/// Enumerate hardware and save the result as JSON to [`HW_CACHE_PATH`].
///
/// Used by the init container to snapshot host hardware info so the containerized agent can
/// read it via [`load_hardware_from_cache`] without needing direct access to host devices.
///
/// Reads CPU info from [`INIT_CPU_INFO_PATH`] (`/host-cpu-info`) where the init container
/// bind-mounts the host's `/proc/cpuinfo`.
pub async fn enumerate_and_save_hardware()
-> Result<rpc_discovery::DiscoveryInfo, HardwareEnumerationError> {
    let mut last_err = String::new();

    macro_rules! try_or_retry {
        ($expr:expr, $msg:literal, $attempt:expr) => {
            match $expr {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(attempt = $attempt, error = %e, $msg);
                    last_err = e.to_string();
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    continue;
                }
            }
        };
    }

    for attempt in 1..10 {
        let info = try_or_retry!(
            enumerate_hardware_inner(INIT_CPU_INFO_PATH, INIT_MEM_INFO_PATH),
            "Hardware enumeration failed; retrying in 10s",
            attempt
        );
        try_or_retry!(
            validate_enumerated(&info),
            "Hardware enumeration incomplete; retrying in 10s",
            attempt
        );
        try_or_retry!(
            save_hardware_to(&info, HW_CACHE_PATH),
            "Failed to save hardware cache; retrying in 10s",
            attempt
        );
        return Ok(info);
    }

    tracing::error!(
        error = %last_err,
        "Init container failed to generate hardware info. Try to delete the pod to recover."
    );

    Err(HardwareEnumerationError::GenericError(last_err))
}

/// Load the hardware snapshot from [`HW_CACHE_PATH`] written by the init container.
///
/// Used by the containerized agent instead of direct hardware probing.
pub fn load_hardware_from_cache() -> Result<rpc_discovery::DiscoveryInfo, HardwareEnumerationError>
{
    load_hardware_from(HW_CACHE_PATH)
}

fn save_hardware_to(
    info: &rpc_discovery::DiscoveryInfo,
    path: &str,
) -> Result<(), HardwareEnumerationError> {
    let json = serde_json::to_string_pretty(info)
        .map_err(|e| HardwareEnumerationError::GenericError(e.to_string()))?;
    fs::write(path, json).map_err(|e| {
        HardwareEnumerationError::GenericError(format!(
            "Failed to write hardware cache to {path}: {e}"
        ))
    })
}

fn load_hardware_from(
    path: &str,
) -> Result<rpc_discovery::DiscoveryInfo, HardwareEnumerationError> {
    let contents = fs::read_to_string(path).map_err(|e| {
        HardwareEnumerationError::GenericError(format!(
            "Failed to read hardware cache from {path}: {e}"
        ))
    })?;
    serde_json::from_str(&contents).map_err(|e| {
        HardwareEnumerationError::GenericError(format!(
            "Failed to parse hardware cache from {path}: {e}"
        ))
    })
}

/// Parse `MemTotal` from `/proc/meminfo` content, returning the value in kB.
/// Returns 0 if the line is absent or unparseable.
fn parse_memtotal_kb(meminfo: &str) -> u32 {
    for line in meminfo.lines() {
        // line format: "MemTotal:       32572708 kB"
        if line.starts_with("MemTotal:") {
            return line
                .split_ascii_whitespace()
                .nth(1)
                .unwrap_or("0")
                .parse()
                .unwrap_or_default();
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{
        Case, Check, check_cases, check_values, scenarios, value_scenarios,
    };
    use tempfile::NamedTempFile;

    use super::*;

    fn minimal_discovery_info() -> rpc_discovery::DiscoveryInfo {
        rpc_discovery::DiscoveryInfo {
            machine_type: "aarch64".to_string(),
            ..Default::default()
        }
    }

    /// Build a `PciDevicePropertiesExt` from the few fields the pure predicates
    /// (`is_dpu`, `mlnx_ib_capable`) actually read.
    fn props_ext(
        device_id: &str,
        vendor: &str,
        slot: Option<&str>,
        sub_class: &str,
    ) -> PciDevicePropertiesExt {
        PciDevicePropertiesExt {
            sub_class: sub_class.to_string(),
            pci_properties: rpc_discovery::PciDeviceProperties {
                vendor: vendor.to_string(),
                slot: slot.map(|s| s.to_string()),
                ..Default::default()
            },
            device_id: device_id.to_string(),
        }
    }

    fn proc_cpu(
        vendor: &str,
        model: &str,
        sockets: u32,
        cores: u32,
        threads: u32,
    ) -> rpc_discovery::CpuInfo {
        rpc_discovery::CpuInfo {
            vendor: vendor.to_string(),
            model: model.to_string(),
            sockets,
            cores,
            threads,
        }
    }

    // save_hardware_to + load_hardware_from round-trip and the load failure
    // paths. Each row exercises the fallible save/load cluster against a temp
    // path; `Yields(true)` means the round-tripped marker field survived, a
    // `FailsWith` row pins the path/parse error text in the returned message.
    #[test]
    fn save_load_cluster() {
        // round-trip: minimal info survives save -> load
        Case {
            scenario: "minimal info round-trips machine_type",
            input: minimal_discovery_info(),
            expect: Yields(true),
        }
        .check(|info| {
            let tmp = NamedTempFile::new().map_err(drop)?;
            let path = tmp.path().to_str().ok_or(())?;
            save_hardware_to(&info, path).map_err(drop)?;
            let loaded = load_hardware_from(path).map_err(drop)?;
            Ok::<_, ()>(loaded.machine_type == "aarch64")
        });

        // round-trip: nested block_devices fields survive
        Case {
            scenario: "block device fields round-trip",
            input: rpc_discovery::DiscoveryInfo {
                machine_type: "x86_64".to_string(),
                block_devices: vec![rpc_discovery::BlockDevice {
                    model: "test-disk".to_string(),
                    serial: "SN123".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            expect: Yields(true),
        }
        .check(|info| {
            let tmp = NamedTempFile::new().map_err(drop)?;
            let path = tmp.path().to_str().ok_or(())?;
            save_hardware_to(&info, path).map_err(drop)?;
            let loaded = load_hardware_from(path).map_err(drop)?;
            Ok::<_, ()>(
                loaded.machine_type == "x86_64"
                    && loaded.block_devices.len() == 1
                    && loaded.block_devices[0].model == "test-disk"
                    && loaded.block_devices[0].serial == "SN123",
            )
        });

        // load failure: missing file -> error mentions the path
        Case {
            scenario: "missing file error mentions path",
            input: "/nonexistent/path/hw.json",
            expect: Yields(true),
        }
        .check(|path| {
            let err = load_hardware_from(path).unwrap_err();
            Ok::<_, ()>(err.to_string().contains("/nonexistent/path/hw.json"))
        });

        // load failure: invalid JSON -> "Failed to parse" error
        Case {
            scenario: "invalid json reports parse failure",
            input: &b"not valid json { {"[..],
            expect: Yields(true),
        }
        .check(|bytes| {
            let tmp = NamedTempFile::new().map_err(drop)?;
            let path = tmp.path().to_str().ok_or(())?;
            fs::write(path, bytes).map_err(drop)?;
            let err = load_hardware_from(path).unwrap_err();
            Ok::<_, ()>(err.to_string().contains("Failed to parse hardware cache"))
        });
    }

    // Init-container bind-mount paths and the cache path are part of the
    // daemonset contract; pin each constant exactly.
    #[test]
    fn init_container_path_constants() {
        value_scenarios!(
            // identity: the constant under test is handed in as the input
            run = |c| c;
            "init cpu info path" {
                INIT_CPU_INFO_PATH => "/host-cpu-info",
            }

            "init mem info path" {
                INIT_MEM_INFO_PATH => "/host-mem-info",
            }

            "hw cache path" {
                HW_CACHE_PATH => "/data/hw_output.json",
            }
        );
    }

    // parse_memtotal_kb: total fn returning the kB value, 0 when the line is
    // absent or the value is unparseable. Covers each lines() / split branch.
    #[test]
    fn parse_memtotal_kb_cases() {
        check_values(
            [
                Check {
                    scenario: "typical first line",
                    input: "MemTotal:       32572708 kB\nMemFree:        16000000 kB\n",
                    expect: 32_572_708,
                },
                Check {
                    scenario: "MemTotal absent -> 0",
                    input: "MemFree:        16000000 kB\nSwapTotal:      0 kB\n",
                    expect: 0,
                },
                Check {
                    scenario: "empty input -> 0",
                    input: "",
                    expect: 0,
                },
                Check {
                    scenario: "non-numeric value -> 0",
                    input: "MemTotal:       not_a_number kB\n",
                    expect: 0,
                },
                Check {
                    scenario: "MemTotal not first line",
                    input: "HugePages_Total: 0\nMemTotal:        8192000 kB\nMemFree:         4096000 kB\nBuffers:          512000 kB\n",
                    expect: 8_192_000,
                },
                // boundary: only the key with no value field -> nth(1) None -> "0" -> 0
                Check {
                    scenario: "MemTotal key with no value -> 0",
                    input: "MemTotal:\n",
                    expect: 0,
                },
                // a line that merely starts with "MemTotal" but is not the
                // "MemTotal:" key is NOT matched (starts_with check is "MemTotal:")
                Check {
                    scenario: "MemTotalSwap not matched -> 0",
                    input: "MemTotalSwap:   123 kB\n",
                    expect: 0,
                },
            ],
            parse_memtotal_kb,
        );
    }

    // can_parse_int: hex (0x-prefixed, base 16) or decimal i32. True only when
    // the remaining text parses; false on bad hex digits, bad decimal, empty,
    // or overflow.
    #[test]
    fn can_parse_int_cases() {
        value_scenarios!(
            run = can_parse_int;
            "plain decimal" {
                "42" => true,
            }

            "negative decimal" {
                "-7" => true,
            }

            "hex value" {
                "0x10de" => true,
            }

            "hex with no digits after prefix" {
                "0x" => false,
            }

            "non-numeric text" {
                "GenuineIntel" => false,
            }

            "empty string" {
                "" => false,
            }

            "decimal overflows i32" {
                "9999999999" => false,
            }

            "bad hex digit" {
                "0xZZ" => false,
            }
        );
    }

    // PciDevicePropertiesExt::is_dpu: device_id prefix decides. BlueField DPUs
    // start 0xa2xx or 0xc2xx; everything else is not a DPU.
    #[test]
    fn is_dpu_cases() {
        value_scenarios!(
            run = |id| {
                props_ext(
                    id,
                    "Mellanox Technologies",
                    Some("0000:08:00.0"),
                    "Ethernet controller",
                )
                .is_dpu()
            };
            "0xa2 prefix is dpu" {
                "0xa2d6" => true,
            }

            "0xc2 prefix is dpu" {
                "0xc2d2" => true,
            }

            "other mellanox id not dpu" {
                "0x1021" => false,
            }

            "empty id not dpu" {
                "" => false,
            }

            "0xa not enough to match" {
                "0xa1ff" => false,
            }
        );
    }

    // PciDevicePropertiesExt::mlnx_ib_capable: true only when slot is present
    // and non-empty, vendor is Mellanox (case-insensitive), and the subclass is
    // "Infiniband controller" (case-insensitive). Any other combination false.
    #[test]
    fn mlnx_ib_capable_cases() {
        value_scenarios!(
            run = |p| p.mlnx_ib_capable();
            "mellanox + slot + infiniband subclass" {
                props_ext(
                    "0xa2d6",
                    "Mellanox Technologies",
                    Some("0000:08:00.0"),
                    "Infiniband controller",
                ) => true,
            }

            "vendor case-insensitive match" {
                props_ext(
                    "0xa2d6",
                    "mellanox technologies",
                    Some("0000:08:00.0"),
                    "Infiniband controller",
                ) => true,
            }

            "subclass case-insensitive match" {
                props_ext(
                    "0xa2d6",
                    "Mellanox Technologies",
                    Some("0000:08:00.0"),
                    "infiniband controller",
                ) => true,
            }

            "ethernet subclass -> false" {
                props_ext(
                    "0xa2d6",
                    "Mellanox Technologies",
                    Some("0000:08:00.0"),
                    "Ethernet controller",
                ) => false,
            }

            "non-mellanox vendor -> false" {
                props_ext(
                    "0xa2d6",
                    "Intel Corporation",
                    Some("0000:08:00.0"),
                    "Infiniband controller",
                ) => false,
            }

            "slot None -> false" {
                props_ext(
                    "0xa2d6",
                    "Mellanox Technologies",
                    None,
                    "Infiniband controller",
                ) => false,
            }

            "empty slot -> false" {
                props_ext(
                    "0xa2d6",
                    "Mellanox Technologies",
                    Some(""),
                    "Infiniband controller",
                ) => false,
            }
        );
    }

    // convert_udev_to_mac: strips the leading "enx", chunks the rest into byte
    // pairs, and colon-joins. Fails only on non-UTF-8 (unreachable for valid
    // String input), so exercise the formatting branches here.
    #[test]
    fn convert_udev_to_mac_cases() {
        check_cases(
            [
                Case {
                    scenario: "standard 6-octet udev name",
                    input: "enx112233445566".to_string(),
                    expect: Yields("11:22:33:44:55:66".to_string()),
                },
                Case {
                    scenario: "single octet after prefix",
                    input: "enxab".to_string(),
                    expect: Yields("ab".to_string()),
                },
                Case {
                    scenario: "exactly the prefix, no octets",
                    input: "enx".to_string(),
                    expect: Yields(String::new()),
                },
                // trailing odd nibble becomes its own one-char chunk
                Case {
                    scenario: "odd-length remainder",
                    input: "enx112233445".to_string(),
                    expect: Yields("11:22:33:44:5".to_string()),
                },
            ],
            |s| convert_udev_to_mac(s).map_err(drop),
        );
    }

    // get_cpu_info: lscpu values override procfs only under specific conditions.
    // vendor/model override iff the procfs value is an unmapped integer AND
    // lscpu has the key. Topology is all-or-nothing and threads becomes
    // cores*threads-per-core.
    #[test]
    fn get_cpu_info_vendor_model_cases() {
        // procfs vendor/model are integers -> lscpu values win
        let mut lscpu = HashMap::new();
        lscpu.insert(LSCPU_VENDOR, "ARM".to_string());
        lscpu.insert(LSCPU_MODEL, "Neoverse-N1".to_string());
        let got = get_cpu_info(&lscpu, proc_cpu("0x41", "0x1", 1, 1, 1));
        assert_eq!(got.vendor, "ARM", "integer procfs vendor replaced by lscpu");
        assert_eq!(
            got.model, "Neoverse-N1",
            "integer procfs model replaced by lscpu"
        );

        // procfs vendor/model already human-readable -> lscpu ignored
        let got = get_cpu_info(&lscpu, proc_cpu("GenuineIntel", "Xeon", 1, 1, 1));
        assert_eq!(got.vendor, "GenuineIntel", "named procfs vendor kept");
        assert_eq!(got.model, "Xeon", "named procfs model kept");

        // procfs integer but lscpu has no key -> procfs value kept
        let empty = HashMap::new();
        let got = get_cpu_info(&empty, proc_cpu("0x41", "0x1", 2, 4, 8));
        assert_eq!(got.vendor, "0x41", "no lscpu vendor -> keep procfs");
        assert_eq!(got.model, "0x1", "no lscpu model -> keep procfs");
    }

    #[test]
    fn get_cpu_info_topology_cases() {
        // complete lscpu topology -> threads = cores_per_socket * threads_per_core
        let mut full = HashMap::new();
        full.insert(LSCPU_SOCKETS, "2".to_string());
        full.insert(LSCPU_CORES_PER_SOCKET, "16".to_string());
        full.insert(LSCPU_THREADS_PER_CORE, "2".to_string());
        let got = get_cpu_info(&full, proc_cpu("0x41", "0x1", 9, 9, 9));
        assert_eq!(got.sockets, 2, "lscpu sockets win");
        assert_eq!(got.cores, 16, "lscpu cores win");
        assert_eq!(got.threads, 32, "threads = cores * threads_per_core");

        // partial topology (missing threads-per-core) -> all procfs values kept
        let mut partial = HashMap::new();
        partial.insert(LSCPU_SOCKETS, "2".to_string());
        partial.insert(LSCPU_CORES_PER_SOCKET, "16".to_string());
        let got = get_cpu_info(&partial, proc_cpu("0x41", "0x1", 9, 9, 9));
        assert_eq!(got.sockets, 9, "incomplete topology -> keep procfs sockets");
        assert_eq!(got.cores, 9, "incomplete topology -> keep procfs cores");
        assert_eq!(got.threads, 9, "incomplete topology -> keep procfs threads");

        // unparseable topology value -> all procfs values kept
        let mut bad = HashMap::new();
        bad.insert(LSCPU_SOCKETS, "two".to_string());
        bad.insert(LSCPU_CORES_PER_SOCKET, "16".to_string());
        bad.insert(LSCPU_THREADS_PER_CORE, "2".to_string());
        let got = get_cpu_info(&bad, proc_cpu("0x41", "0x1", 9, 9, 9));
        assert_eq!(
            got.sockets, 9,
            "unparseable topology -> keep procfs sockets"
        );
        assert_eq!(
            got.threads, 9,
            "unparseable topology -> keep procfs threads"
        );
    }

    // validate_enumerated: the sole required field today is dpu_info. None ->
    // Err, Some -> Ok(()). Error is not PartialEq so use Fails + Yields(()).
    #[test]
    fn validate_enumerated_cases() {
        scenarios!(
            run = |info| validate_enumerated(&info).map_err(drop);
            "missing dpu_info fails" {
                rpc_discovery::DiscoveryInfo {
                    dpu_info: None,
                    ..Default::default()
                } => Fails,
            }

            "present dpu_info passes" {
                rpc_discovery::DiscoveryInfo {
                    dpu_info: Some(Default::default()),
                    ..Default::default()
                } => Yields(()),
            }
        );
    }

    #[test]
    fn condense_rpc_memory_devices_cases() {
        scenarios!(
            run = |devices| condense_rpc_memory_devices(devices).map_err(drop);

            "empty input yields empty output" {
                vec![] => Yields(vec![]),
            }

            "identical consecutive devices are merged" {
                vec![
                    MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) },
                    MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) },
                    MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) },
                ] => Yields(vec![MemoryDeviceGroup { size_mb: Some(16384), mem_type: Some("DDR5".into()), count: 3 }]),
            }

            "non-consecutive identical devices are not merged" {
                vec![
                    MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) },
                    MemoryDevice { size_mb: Some(32768), mem_type: Some("DDR4".into()) },
                    MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) },
                ] => Yields(vec![
                    MemoryDeviceGroup { size_mb: Some(16384), mem_type: Some("DDR5".into()), count: 1 },
                    MemoryDeviceGroup { size_mb: Some(32768), mem_type: Some("DDR4".into()), count: 1 },
                    MemoryDeviceGroup { size_mb: Some(16384), mem_type: Some("DDR5".into()), count: 1 },
                ]),
            }

            "total exactly at the max is accepted" {
                (0..rpc::machine_discovery::MemoryDeviceGroup::MAX_REHYDRATE_COUNT)
                    .map(|_| MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) })
                    .collect::<Vec<_>>() => Yields(vec![MemoryDeviceGroup {
                        size_mb: Some(16384),
                        mem_type: Some("DDR5".into()),
                        count: rpc::machine_discovery::MemoryDeviceGroup::MAX_REHYDRATE_COUNT,
                    }]),
            }

            "total above the max is rejected" {
                (0..rpc::machine_discovery::MemoryDeviceGroup::MAX_REHYDRATE_COUNT + 1)
                    .map(|_| MemoryDevice { size_mb: Some(16384), mem_type: Some("DDR5".into()) })
                    .collect::<Vec<_>>() => Fails,
            }
        );
    }
}
