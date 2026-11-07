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

//! Describes hardware that is discovered by Forge

use base64::prelude::*;
use carbide_network::{MELLANOX_SF_VF_MAC_ADDRESS_IN, MELLANOX_SF_VF_MAC_ADDRESS_OUT};
use carbide_utils::arch::CpuArchitecture;
use carbide_utils::try_convert_vec;
use mac_address::MacAddress;
use model::hardware_info::{
    BlockDevice, CpuInfo, DmiData, DpuData, Gpu, GpuPlatformInfo, HardwareInfo,
    InfinibandInterface, LldpSwitchData, MachineInventory, MachineInventorySoftwareComponent,
    MachineNvLinkInfo, MemoryDevice, NetworkInterface, NvLinkGpu, NvmeDevice, PciDeviceProperties,
    TpmDescription, TpmEkCertificate, condense_groups, condense_memory_devices,
};

use crate as rpc;
use crate::errors::RpcDataConversionError;

impl From<rpc::machine_discovery::TpmDescription> for TpmDescription {
    fn from(value: rpc::machine_discovery::TpmDescription) -> Self {
        TpmDescription {
            vendor: value.vendor.trim_matches('\0').to_string(),
            firmware_version: value.firmware_version.trim_matches('\0').to_string(),
            tpm_spec: value.tpm_spec.trim_matches('\0').to_string(),
        }
    }
}

impl From<TpmDescription> for rpc::machine_discovery::TpmDescription {
    fn from(value: TpmDescription) -> Self {
        rpc::machine_discovery::TpmDescription {
            vendor: value.vendor,
            firmware_version: value.firmware_version,
            tpm_spec: value.tpm_spec,
        }
    }
}

// These defines conversions functions from the RPC data model into the internal
// data model (which might also be used in the database).
// It might actually be nicer to have those closer to the rpc crate to avoid
// polluting the internal data model with API concerns, but since this is a
// separate crate we can't have it there (unless we also make the model a
// separate crate).
//

impl TryFrom<rpc::machine_discovery::CpuInfo> for CpuInfo {
    type Error = RpcDataConversionError;

    fn try_from(cpu_info: rpc::machine_discovery::CpuInfo) -> Result<Self, Self::Error> {
        Ok(Self {
            model: cpu_info.model,
            vendor: cpu_info.vendor,
            sockets: cpu_info.sockets,
            cores: cpu_info.cores,
            threads: cpu_info.threads,
        })
    }
}

impl TryFrom<CpuInfo> for rpc::machine_discovery::CpuInfo {
    type Error = RpcDataConversionError;

    fn try_from(cpu_info: CpuInfo) -> Result<Self, Self::Error> {
        Ok(Self {
            model: cpu_info.model,
            vendor: cpu_info.vendor,
            sockets: cpu_info.sockets,
            cores: cpu_info.cores,
            threads: cpu_info.threads,
        })
    }
}

impl TryFrom<rpc::machine_discovery::BlockDevice> for BlockDevice {
    type Error = RpcDataConversionError;

    fn try_from(dev: rpc::machine_discovery::BlockDevice) -> Result<Self, Self::Error> {
        Ok(Self {
            model: dev.model,
            revision: dev.revision,
            serial: dev.serial,
            device_type: dev.device_type,
        })
    }
}

impl TryFrom<BlockDevice> for rpc::machine_discovery::BlockDevice {
    type Error = RpcDataConversionError;

    fn try_from(dev: BlockDevice) -> Result<Self, Self::Error> {
        Ok(Self {
            model: dev.model,
            revision: dev.revision,
            serial: dev.serial,
            device_type: dev.device_type,
        })
    }
}

impl TryFrom<rpc::machine_discovery::NvmeDevice> for NvmeDevice {
    type Error = RpcDataConversionError;

    fn try_from(dev: rpc::machine_discovery::NvmeDevice) -> Result<Self, Self::Error> {
        Ok(Self {
            model: dev.model,
            firmware_rev: dev.firmware_rev,
            serial: dev.serial,
            size_mb: dev.size_mb,
            pci_path: dev.pci_path,
        })
    }
}

impl TryFrom<NvmeDevice> for rpc::machine_discovery::NvmeDevice {
    type Error = RpcDataConversionError;

    fn try_from(dev: NvmeDevice) -> Result<Self, Self::Error> {
        Ok(Self {
            model: dev.model,
            firmware_rev: dev.firmware_rev,
            serial: dev.serial,
            size_mb: dev.size_mb,
            pci_path: dev.pci_path,
        })
    }
}

impl TryFrom<rpc::machine_discovery::DmiData> for DmiData {
    type Error = RpcDataConversionError;

    fn try_from(data: rpc::machine_discovery::DmiData) -> Result<Self, Self::Error> {
        Ok(Self {
            board_name: data.board_name,
            board_version: data.board_version,
            bios_version: data.bios_version,
            bios_date: data.bios_date,
            product_serial: data.product_serial,
            board_serial: data.board_serial,
            chassis_serial: data.chassis_serial,
            product_name: data.product_name,
            sys_vendor: data.sys_vendor,
        })
    }
}

impl TryFrom<DmiData> for rpc::machine_discovery::DmiData {
    type Error = RpcDataConversionError;

    fn try_from(data: DmiData) -> Result<Self, Self::Error> {
        Ok(Self {
            board_name: data.board_name,
            board_version: data.board_version,
            bios_version: data.bios_version,
            bios_date: data.bios_date,
            product_serial: data.product_serial,
            board_serial: data.board_serial,
            chassis_serial: data.chassis_serial,
            product_name: data.product_name,
            sys_vendor: data.sys_vendor,
        })
    }
}

impl TryFrom<rpc::machine_discovery::LldpSwitchData> for LldpSwitchData {
    type Error = RpcDataConversionError;

    fn try_from(data: rpc::machine_discovery::LldpSwitchData) -> Result<Self, Self::Error> {
        // Reads the legacy combined `id`/`remote_port` proto fields (deprecated).
        #[allow(deprecated)]
        Ok(Self {
            name: data.name,
            id: data.id,
            description: data.description,
            local_port: data.local_port,
            ip_address: data
                .ip_address
                .into_iter()
                .map(|ip| {
                    ip.parse()
                        .map_err(|_| RpcDataConversionError::InvalidIpAddress(ip))
                })
                .collect::<Result<Vec<_>, _>>()?,
            remote_port: data.remote_port,
        })
    }
}

impl TryFrom<LldpSwitchData> for rpc::machine_discovery::LldpSwitchData {
    type Error = RpcDataConversionError;

    fn try_from(data: LldpSwitchData) -> Result<Self, Self::Error> {
        // The api-model type carries only the legacy combined `id`/`remote_port`;
        // the split *_type/*_value proto fields default to empty on this path.
        #[allow(deprecated)]
        Ok(Self {
            name: data.name,
            id: data.id,
            description: data.description,
            local_port: data.local_port,
            ip_address: data
                .ip_address
                .into_iter()
                .map(|ip| ip.to_string())
                .collect(),
            remote_port: data.remote_port,
            ..Default::default()
        })
    }
}

impl TryFrom<rpc::machine_discovery::DpuData> for DpuData {
    type Error = RpcDataConversionError;

    fn try_from(data: rpc::machine_discovery::DpuData) -> Result<Self, Self::Error> {
        Ok(Self {
            part_number: data.part_number,
            part_description: data.part_description,
            product_version: data.product_version,
            factory_mac_address: data.factory_mac_address,
            firmware_version: data.firmware_version,
            firmware_date: data.firmware_date,
            switches: try_convert_vec(data.switches)?,
        })
    }
}

impl TryFrom<DpuData> for rpc::machine_discovery::DpuData {
    type Error = RpcDataConversionError;

    fn try_from(data: DpuData) -> Result<Self, Self::Error> {
        Ok(Self {
            part_number: data.part_number,
            part_description: data.part_description,
            product_version: data.product_version,
            factory_mac_address: data.factory_mac_address,
            firmware_version: data.firmware_version,
            firmware_date: data.firmware_date,
            switches: try_convert_vec(data.switches)?,
        })
    }
}

impl TryFrom<rpc::machine_discovery::NetworkInterface> for NetworkInterface {
    type Error = RpcDataConversionError;

    fn try_from(iface: rpc::machine_discovery::NetworkInterface) -> Result<Self, Self::Error> {
        let pci_properties = match iface.pci_properties.map(PciDeviceProperties::try_from) {
            Some(Err(e)) => return Err(e),
            Some(Ok(props)) => Some(props),
            None => None,
        };

        // Do what deserialize_ch_64 does in this case.
        let mac_string = if iface.mac_address == MELLANOX_SF_VF_MAC_ADDRESS_IN {
            MELLANOX_SF_VF_MAC_ADDRESS_OUT.to_string()
        } else {
            iface.mac_address
        };

        let mac_address: MacAddress = mac_string
            .parse()
            .map_err(|_| RpcDataConversionError::InvalidMacAddress(mac_string.clone()))?;

        Ok(Self {
            mac_address,
            pci_properties,
        })
    }
}

impl TryFrom<NetworkInterface> for rpc::machine_discovery::NetworkInterface {
    type Error = RpcDataConversionError;

    fn try_from(iface: NetworkInterface) -> Result<Self, Self::Error> {
        let pci_properties = match iface
            .pci_properties
            .map(rpc::machine_discovery::PciDeviceProperties::try_from)
        {
            Some(Err(e)) => return Err(e),
            Some(Ok(props)) => Some(props),
            None => None,
        };

        Ok(Self {
            mac_address: iface.mac_address.to_string(),
            pci_properties,
        })
    }
}

impl TryFrom<rpc::machine_discovery::InfinibandInterface> for InfinibandInterface {
    type Error = RpcDataConversionError;

    fn try_from(ibface: rpc::machine_discovery::InfinibandInterface) -> Result<Self, Self::Error> {
        let pci_properties = match ibface.pci_properties.map(PciDeviceProperties::try_from) {
            Some(Err(e)) => return Err(e),
            Some(Ok(props)) => Some(props),
            None => None,
        };

        Ok(Self {
            guid: ibface.guid,
            pci_properties,
        })
    }
}

impl TryFrom<InfinibandInterface> for rpc::machine_discovery::InfinibandInterface {
    type Error = RpcDataConversionError;

    fn try_from(ibface: InfinibandInterface) -> Result<Self, Self::Error> {
        let pci_properties = match ibface
            .pci_properties
            .map(rpc::machine_discovery::PciDeviceProperties::try_from)
        {
            Some(Err(e)) => return Err(e),
            Some(Ok(props)) => Some(props),
            None => None,
        };

        Ok(Self {
            guid: ibface.guid,
            pci_properties,
        })
    }
}

impl TryFrom<rpc::machine_discovery::PciDeviceProperties> for PciDeviceProperties {
    type Error = RpcDataConversionError;

    fn try_from(props: rpc::machine_discovery::PciDeviceProperties) -> Result<Self, Self::Error> {
        Ok(Self {
            vendor: props.vendor,
            device: props.device,
            path: props.path,
            numa_node: props.numa_node,
            description: props.description,
            slot: props.slot,
        })
    }
}

impl TryFrom<PciDeviceProperties> for rpc::machine_discovery::PciDeviceProperties {
    type Error = RpcDataConversionError;

    fn try_from(props: PciDeviceProperties) -> Result<Self, Self::Error> {
        Ok(Self {
            vendor: props.vendor,
            device: props.device,
            path: props.path,
            numa_node: props.numa_node,
            description: props.description,
            slot: props.slot,
        })
    }
}

impl TryFrom<GpuPlatformInfo> for rpc::machine_discovery::GpuPlatformInfo {
    type Error = RpcDataConversionError;

    fn try_from(info: GpuPlatformInfo) -> Result<Self, Self::Error> {
        Ok(Self {
            chassis_serial: info.chassis_serial,
            slot_number: info.slot_number,
            tray_index: info.tray_index,
            host_id: info.host_id,
            module_id: info.module_id,
            fabric_guid: info.fabric_guid,
        })
    }
}

impl TryFrom<rpc::machine_discovery::GpuPlatformInfo> for GpuPlatformInfo {
    type Error = RpcDataConversionError;

    fn try_from(info: rpc::machine_discovery::GpuPlatformInfo) -> Result<Self, Self::Error> {
        Ok(Self {
            chassis_serial: info.chassis_serial,
            slot_number: info.slot_number,
            tray_index: info.tray_index,
            host_id: info.host_id,
            module_id: info.module_id,
            fabric_guid: info.fabric_guid,
        })
    }
}

impl TryFrom<Gpu> for rpc::machine_discovery::Gpu {
    type Error = RpcDataConversionError;

    fn try_from(gpu: Gpu) -> Result<Self, Self::Error> {
        let platform_info = match gpu
            .platform_info
            .map(rpc::machine_discovery::GpuPlatformInfo::try_from)
        {
            Some(Err(e)) => return Err(e),
            Some(Ok(info)) => Some(info),
            None => None,
        };

        Ok(Self {
            name: gpu.name,
            serial: gpu.serial,
            driver_version: gpu.driver_version,
            vbios_version: gpu.vbios_version,
            inforom_version: gpu.inforom_version,
            total_memory: gpu.total_memory,
            frequency: gpu.frequency,
            pci_bus_id: gpu.pci_bus_id,
            platform_info,
        })
    }
}

impl TryFrom<rpc::machine_discovery::Gpu> for Gpu {
    type Error = RpcDataConversionError;

    fn try_from(gpu: rpc::machine_discovery::Gpu) -> Result<Self, Self::Error> {
        let platform_info = match gpu.platform_info.map(GpuPlatformInfo::try_from) {
            Some(Err(e)) => return Err(e),
            Some(Ok(info)) => Some(info),
            None => None,
        };

        Ok(Self {
            name: gpu.name,
            serial: gpu.serial,
            driver_version: gpu.driver_version,
            vbios_version: gpu.vbios_version,
            inforom_version: gpu.inforom_version,
            total_memory: gpu.total_memory,
            frequency: gpu.frequency,
            pci_bus_id: gpu.pci_bus_id,
            platform_info,
        })
    }
}

impl From<rpc::machine_discovery::MemoryDevice> for MemoryDevice {
    fn from(value: rpc::machine_discovery::MemoryDevice) -> Self {
        MemoryDevice {
            size_mb: value.size_mb,
            mem_type: value.mem_type,
        }
    }
}

impl TryFrom<rpc::machine_discovery::MemoryDeviceGroup>
    for model::hardware_info::MemoryDeviceGroup
{
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::machine_discovery::MemoryDeviceGroup) -> Result<Self, Self::Error> {
        if value.count > model::hardware_info::MAX_MEMORY_DEVICE_COUNT {
            return Err(RpcDataConversionError::InvalidValue(
                "memory_device_groups[].count".to_string(),
                value.count.to_string(),
            ));
        }
        Ok(Self {
            size_mb: value.size_mb,
            mem_type: value.mem_type,
            count: value.count,
        })
    }
}

impl From<model::hardware_info::MemoryDeviceGroup> for rpc::machine_discovery::MemoryDeviceGroup {
    fn from(value: model::hardware_info::MemoryDeviceGroup) -> Self {
        Self {
            size_mb: value.size_mb,
            mem_type: value.mem_type,
            count: value.count,
        }
    }
}

impl TryFrom<rpc::machine_discovery::DiscoveryInfo> for HardwareInfo {
    type Error = RpcDataConversionError;

    fn try_from(info: rpc::machine_discovery::DiscoveryInfo) -> Result<Self, Self::Error> {
        let memory_groups_are_authoritative = info.memory_groups_are_authoritative();
        let tpm_ek_certificate = info
            .tpm_ek_certificate
            .map(|base64| {
                BASE64_STANDARD
                    .decode(base64)
                    .map_err(|_| RpcDataConversionError::InvalidBase64Data("tpm_ek_certificate"))
            })
            .transpose()?;

        let machine_arch = match info.machine_arch {
            // new
            Some(arch) => rpc::utils::cpu_architecture_from_rpc(arch),
            // old
            None => {
                tracing::warn!("DiscoveryInfo missing machine_arch.");
                info.machine_type.parse().unwrap_or_else(|e| {
                    // Unfortunately we don't have the machine_id here.
                    tracing::error!(error = %e, "Error parsing grpc DiscoveryInfo");
                    CpuArchitecture::Unknown
                })
            }
        };

        let cpu_info: Vec<CpuInfo> = try_convert_vec(info.cpu_info)?;

        Ok(Self {
            network_interfaces: try_convert_vec(info.network_interfaces)?,
            infiniband_interfaces: try_convert_vec(info.infiniband_interfaces)?,
            cpu_info,
            block_devices: try_convert_vec(info.block_devices)?,
            machine_type: machine_arch,
            nvme_devices: try_convert_vec(info.nvme_devices)?,
            dmi_data: info.dmi_data.map(DmiData::try_from).transpose()?,
            tpm_ek_certificate: tpm_ek_certificate.map(TpmEkCertificate::from),
            dpu_info: info.dpu_info.map(DpuData::try_from).transpose()?,
            gpus: try_convert_vec(info.gpus)?,
            memory_devices: if memory_groups_are_authoritative {
                let groups = info
                    .memory_device_groups
                    .into_iter()
                    .map(model::hardware_info::MemoryDeviceGroup::try_from)
                    .collect::<Result<Vec<_>, _>>()?;
                condense_groups(groups).map_err(|e| {
                    RpcDataConversionError::InvalidValue(
                        "memory_device_groups".to_string(),
                        e.to_string(),
                    )
                })?
            } else {
                #[allow(deprecated)]
                condense_memory_devices(info.memory_devices.into_iter().map(MemoryDevice::from))
                    .map_err(|e| {
                        RpcDataConversionError::InvalidValue(
                            "memory_devices".to_string(),
                            e.to_string(),
                        )
                    })?
            },
            tpm_description: info.tpm_description.map(std::convert::Into::into),
        })
    }
}

impl TryFrom<HardwareInfo> for rpc::machine_discovery::DiscoveryInfo {
    type Error = RpcDataConversionError;

    fn try_from(info: HardwareInfo) -> Result<Self, Self::Error> {
        Ok(Self {
            network_interfaces: try_convert_vec(info.network_interfaces)?,
            infiniband_interfaces: try_convert_vec(info.infiniband_interfaces)?,
            cpu_info: try_convert_vec(info.cpu_info)?,
            block_devices: try_convert_vec(info.block_devices)?,
            machine_type: info.machine_type.to_string(),
            machine_arch: Some(rpc::utils::cpu_architecture_to_rpc(info.machine_type)),
            nvme_devices: try_convert_vec(info.nvme_devices)?,
            dmi_data: info
                .dmi_data
                .map(rpc::machine_discovery::DmiData::try_from)
                .transpose()?,
            tpm_ek_certificate: info
                .tpm_ek_certificate
                .map(|cert| BASE64_STANDARD.encode(cert.into_bytes())),
            dpu_info: info
                .dpu_info
                .map(rpc::machine_discovery::DpuData::try_from)
                .transpose()?,
            gpus: try_convert_vec(info.gpus)?,
            #[allow(deprecated)]
            memory_devices: vec![],
            memory_device_groups: info
                .memory_devices
                .into_iter()
                .map(rpc::machine_discovery::MemoryDeviceGroup::from)
                .collect(),
            tpm_description: info.tpm_description.map(std::convert::Into::into),
            attest_key_info: None,
        })
    }
}

impl TryFrom<crate::forge::MachineInventory> for MachineInventory {
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::MachineInventory) -> Result<Self, Self::Error> {
        Ok(MachineInventory {
            components: value
                .components
                .into_iter()
                .map(MachineInventorySoftwareComponent::try_from)
                .collect::<Result<_, _>>()?,
        })
    }
}

impl TryFrom<crate::forge::MachineInventorySoftwareComponent>
    for MachineInventorySoftwareComponent
{
    type Error = RpcDataConversionError;

    fn try_from(value: rpc::forge::MachineInventorySoftwareComponent) -> Result<Self, Self::Error> {
        Ok(MachineInventorySoftwareComponent {
            name: value.name,
            version: value.version,
            url: value.url,
        })
    }
}

impl From<MachineInventory> for rpc::forge::MachineInventory {
    fn from(value: MachineInventory) -> Self {
        rpc::forge::MachineInventory {
            components: value
                .components
                .into_iter()
                .map(|c| rpc::forge::MachineInventorySoftwareComponent {
                    name: c.name,
                    version: c.version,
                    url: c.url,
                })
                .collect(),
        }
    }
}

impl From<MachineNvLinkInfo> for rpc::forge::MachineNvLinkInfo {
    fn from(value: MachineNvLinkInfo) -> Self {
        rpc::forge::MachineNvLinkInfo {
            domain_uuid: Some(value.domain_uuid),
            gpus: value
                .gpus
                .into_iter()
                .map(rpc::forge::NvLinkGpu::from)
                .collect(),
            chassis_serial: value.chassis_serial,
        }
    }
}

impl From<NvLinkGpu> for rpc::forge::NvLinkGpu {
    fn from(value: NvLinkGpu) -> Self {
        rpc::forge::NvLinkGpu {
            tray_index: value.tray_index,
            slot_id: value.slot_id,
            device_id: value.device_id,
            guid: value.guid,
        }
    }
}

impl TryFrom<rpc::forge::MachineNvLinkInfo> for MachineNvLinkInfo {
    type Error = rpc::errors::RpcDataConversionError;

    fn try_from(value: rpc::forge::MachineNvLinkInfo) -> Result<Self, Self::Error> {
        Ok(MachineNvLinkInfo {
            domain_uuid: value.domain_uuid.ok_or(
                rpc::errors::RpcDataConversionError::MissingArgument("domain_uuid"),
            )?,
            chassis_serial: value.chassis_serial,
            gpus: value.gpus.into_iter().map(NvLinkGpu::from).collect(),
        })
    }
}

impl From<rpc::forge::NvLinkGpu> for NvLinkGpu {
    fn from(value: rpc::forge::NvLinkGpu) -> Self {
        NvLinkGpu {
            tray_index: value.tray_index,
            slot_id: value.slot_id,
            device_id: value.device_id,
            guid: value.guid,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Case, Check, check_cases, check_values};
    use carbide_uuid::nvlink::NvLinkDomainId;
    use model::hardware_info::MemoryDeviceGroup;

    use super::*;

    struct ArchitectureInput {
        machine_arch: Option<i32>,
        legacy_machine_type: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct LldpProjection {
        name: String,
        id: String,
        description: String,
        local_port: String,
        ip_addresses: Vec<String>,
        remote_port: String,
    }

    #[derive(Debug, PartialEq)]
    struct PciProjection {
        vendor: String,
        device: String,
        path: String,
        numa_node: i32,
        description: Option<String>,
        slot: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct CpuProjection {
        model: String,
        vendor: String,
        sockets: u32,
        cores: u32,
        threads: u32,
    }

    #[derive(Debug, PartialEq)]
    struct BlockDeviceProjection {
        model: String,
        revision: String,
        serial: String,
        device_type: String,
    }

    #[derive(Debug, PartialEq)]
    struct NvmeDeviceProjection {
        model: String,
        firmware_rev: String,
        serial: String,
        size_mb: Option<u32>,
        pci_path: Option<String>,
    }

    #[derive(Debug, PartialEq)]
    struct DmiProjection {
        board_name: String,
        board_version: String,
        bios_version: String,
        bios_date: String,
        product_serial: String,
        board_serial: String,
        chassis_serial: String,
        product_name: String,
        sys_vendor: String,
    }

    #[derive(Debug, PartialEq)]
    struct DpuProjection {
        part_number: String,
        part_description: String,
        product_version: String,
        factory_mac_address: String,
        firmware_version: String,
        firmware_date: String,
    }

    #[derive(Debug, PartialEq)]
    struct GpuPlatformProjection {
        chassis_serial: String,
        slot_number: u32,
        tray_index: u32,
        host_id: u32,
        module_id: u32,
        fabric_guid: String,
    }

    #[derive(Debug, PartialEq)]
    struct GpuProjection {
        name: String,
        serial: String,
        driver_version: String,
        vbios_version: String,
        inforom_version: String,
        total_memory: String,
        frequency: String,
        pci_bus_id: String,
        platform_info: Option<GpuPlatformProjection>,
    }

    #[derive(Debug, PartialEq)]
    struct HardwareProjection {
        machine_type: CpuArchitecture,
        network_mac_addresses: Vec<String>,
        network_slots: Vec<Option<String>>,
        infiniband_guids: Vec<String>,
        infiniband_pci_properties: Vec<Option<PciProjection>>,
        cpu_info: Vec<CpuProjection>,
        block_devices: Vec<BlockDeviceProjection>,
        nvme_devices: Vec<NvmeDeviceProjection>,
        dmi_data: Option<DmiProjection>,
        tpm_ek_certificate: Option<Vec<u8>>,
        dpu_info: Option<DpuProjection>,
        dpu_switch_names: Vec<String>,
        dpu_switch_addresses: Vec<Vec<String>>,
        gpus: Vec<GpuProjection>,
        memory_sizes: Vec<Option<u32>>,
        memory_types: Vec<Option<String>>,
        tpm_description: Option<TpmDescription>,
    }

    #[derive(Debug, PartialEq)]
    struct RpcDiscoveryProjection {
        machine_type: String,
        machine_arch: Option<i32>,
        network_mac_addresses: Vec<String>,
        network_slots: Vec<Option<String>>,
        infiniband_guids: Vec<String>,
        infiniband_pci_properties: Vec<Option<PciProjection>>,
        cpu_info: Vec<CpuProjection>,
        block_devices: Vec<BlockDeviceProjection>,
        nvme_devices: Vec<NvmeDeviceProjection>,
        dmi_data: Option<DmiProjection>,
        tpm_ek_certificate: Option<String>,
        dpu_info: Option<DpuProjection>,
        dpu_switch_names: Vec<String>,
        dpu_switch_addresses: Vec<Vec<String>>,
        gpus: Vec<GpuProjection>,
        memory_sizes: Vec<Option<u32>>,
        memory_types: Vec<Option<String>>,
        tpm_description: Option<rpc::machine_discovery::TpmDescription>,
    }

    impl From<CpuInfo> for CpuProjection {
        fn from(value: CpuInfo) -> Self {
            Self {
                model: value.model,
                vendor: value.vendor,
                sockets: value.sockets,
                cores: value.cores,
                threads: value.threads,
            }
        }
    }

    impl From<&PciDeviceProperties> for PciProjection {
        fn from(value: &PciDeviceProperties) -> Self {
            Self {
                vendor: value.vendor.clone(),
                device: value.device.clone(),
                path: value.path.clone(),
                numa_node: value.numa_node,
                description: value.description.clone(),
                slot: value.slot.clone(),
            }
        }
    }

    impl From<&rpc::machine_discovery::PciDeviceProperties> for PciProjection {
        fn from(value: &rpc::machine_discovery::PciDeviceProperties) -> Self {
            Self {
                vendor: value.vendor.clone(),
                device: value.device.clone(),
                path: value.path.clone(),
                numa_node: value.numa_node,
                description: value.description.clone(),
                slot: value.slot.clone(),
            }
        }
    }

    impl From<rpc::machine_discovery::CpuInfo> for CpuProjection {
        fn from(value: rpc::machine_discovery::CpuInfo) -> Self {
            Self {
                model: value.model,
                vendor: value.vendor,
                sockets: value.sockets,
                cores: value.cores,
                threads: value.threads,
            }
        }
    }

    impl From<BlockDevice> for BlockDeviceProjection {
        fn from(value: BlockDevice) -> Self {
            Self {
                model: value.model,
                revision: value.revision,
                serial: value.serial,
                device_type: value.device_type,
            }
        }
    }

    impl From<rpc::machine_discovery::BlockDevice> for BlockDeviceProjection {
        fn from(value: rpc::machine_discovery::BlockDevice) -> Self {
            Self {
                model: value.model,
                revision: value.revision,
                serial: value.serial,
                device_type: value.device_type,
            }
        }
    }

    impl From<NvmeDevice> for NvmeDeviceProjection {
        fn from(value: NvmeDevice) -> Self {
            Self {
                model: value.model,
                firmware_rev: value.firmware_rev,
                serial: value.serial,
                size_mb: value.size_mb,
                pci_path: value.pci_path,
            }
        }
    }

    impl From<rpc::machine_discovery::NvmeDevice> for NvmeDeviceProjection {
        fn from(value: rpc::machine_discovery::NvmeDevice) -> Self {
            Self {
                model: value.model,
                firmware_rev: value.firmware_rev,
                serial: value.serial,
                size_mb: value.size_mb,
                pci_path: value.pci_path,
            }
        }
    }

    impl From<DmiData> for DmiProjection {
        fn from(value: DmiData) -> Self {
            Self {
                board_name: value.board_name,
                board_version: value.board_version,
                bios_version: value.bios_version,
                bios_date: value.bios_date,
                product_serial: value.product_serial,
                board_serial: value.board_serial,
                chassis_serial: value.chassis_serial,
                product_name: value.product_name,
                sys_vendor: value.sys_vendor,
            }
        }
    }

    impl From<rpc::machine_discovery::DmiData> for DmiProjection {
        fn from(value: rpc::machine_discovery::DmiData) -> Self {
            Self {
                board_name: value.board_name,
                board_version: value.board_version,
                bios_version: value.bios_version,
                bios_date: value.bios_date,
                product_serial: value.product_serial,
                board_serial: value.board_serial,
                chassis_serial: value.chassis_serial,
                product_name: value.product_name,
                sys_vendor: value.sys_vendor,
            }
        }
    }

    impl From<&DpuData> for DpuProjection {
        fn from(value: &DpuData) -> Self {
            Self {
                part_number: value.part_number.clone(),
                part_description: value.part_description.clone(),
                product_version: value.product_version.clone(),
                factory_mac_address: value.factory_mac_address.clone(),
                firmware_version: value.firmware_version.clone(),
                firmware_date: value.firmware_date.clone(),
            }
        }
    }

    impl From<&rpc::machine_discovery::DpuData> for DpuProjection {
        fn from(value: &rpc::machine_discovery::DpuData) -> Self {
            Self {
                part_number: value.part_number.clone(),
                part_description: value.part_description.clone(),
                product_version: value.product_version.clone(),
                factory_mac_address: value.factory_mac_address.clone(),
                firmware_version: value.firmware_version.clone(),
                firmware_date: value.firmware_date.clone(),
            }
        }
    }

    impl From<GpuPlatformInfo> for GpuPlatformProjection {
        fn from(value: GpuPlatformInfo) -> Self {
            Self {
                chassis_serial: value.chassis_serial,
                slot_number: value.slot_number,
                tray_index: value.tray_index,
                host_id: value.host_id,
                module_id: value.module_id,
                fabric_guid: value.fabric_guid,
            }
        }
    }

    impl From<rpc::machine_discovery::GpuPlatformInfo> for GpuPlatformProjection {
        fn from(value: rpc::machine_discovery::GpuPlatformInfo) -> Self {
            Self {
                chassis_serial: value.chassis_serial,
                slot_number: value.slot_number,
                tray_index: value.tray_index,
                host_id: value.host_id,
                module_id: value.module_id,
                fabric_guid: value.fabric_guid,
            }
        }
    }

    impl From<Gpu> for GpuProjection {
        fn from(value: Gpu) -> Self {
            Self {
                name: value.name,
                serial: value.serial,
                driver_version: value.driver_version,
                vbios_version: value.vbios_version,
                inforom_version: value.inforom_version,
                total_memory: value.total_memory,
                frequency: value.frequency,
                pci_bus_id: value.pci_bus_id,
                platform_info: value.platform_info.map(GpuPlatformProjection::from),
            }
        }
    }

    impl From<rpc::machine_discovery::Gpu> for GpuProjection {
        fn from(value: rpc::machine_discovery::Gpu) -> Self {
            Self {
                name: value.name,
                serial: value.serial,
                driver_version: value.driver_version,
                vbios_version: value.vbios_version,
                inforom_version: value.inforom_version,
                total_memory: value.total_memory,
                frequency: value.frequency,
                pci_bus_id: value.pci_bus_id,
                platform_info: value.platform_info.map(GpuPlatformProjection::from),
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct InventoryComponentProjection {
        name: String,
        version: String,
        url: String,
    }

    #[derive(Debug, PartialEq)]
    struct InventoryProjection {
        components: Vec<InventoryComponentProjection>,
    }

    #[derive(Debug, PartialEq)]
    struct NvLinkGpuProjection {
        tray_index: i32,
        slot_id: i32,
        device_id: i32,
        guid: u64,
    }

    #[derive(Debug, PartialEq)]
    struct NvLinkProjection {
        domain_uuid: Option<NvLinkDomainId>,
        chassis_serial: String,
        gpus: Vec<NvLinkGpuProjection>,
    }

    fn rpc_pci(marker: &str) -> rpc::machine_discovery::PciDeviceProperties {
        rpc::machine_discovery::PciDeviceProperties {
            vendor: format!("vendor-{marker}"),
            device: format!("device-{marker}"),
            path: format!("/pci/{marker}"),
            numa_node: 3,
            description: Some(format!("description-{marker}")),
            slot: Some(format!("slot-{marker}")),
        }
    }

    fn model_pci(marker: &str) -> PciDeviceProperties {
        PciDeviceProperties {
            vendor: format!("vendor-{marker}"),
            device: format!("device-{marker}"),
            path: format!("/pci/{marker}"),
            numa_node: 3,
            description: Some(format!("description-{marker}")),
            slot: Some(format!("slot-{marker}")),
        }
    }

    fn rpc_lldp(marker: &str, ip_addresses: &[&str]) -> rpc::machine_discovery::LldpSwitchData {
        rpc::machine_discovery::LldpSwitchData {
            name: format!("switch-{marker}"),
            // The model still represents the legacy combined chassis identifier.
            #[allow(deprecated)]
            id: format!("id={marker}"),
            description: format!("description-{marker}"),
            local_port: format!("local-{marker}"),
            ip_address: ip_addresses
                .iter()
                .map(|address| address.to_string())
                .collect(),
            // The model still represents the legacy combined remote-port identifier.
            #[allow(deprecated)]
            remote_port: format!("remote={marker}"),
            ..Default::default()
        }
    }

    fn model_lldp(marker: &str, ip_addresses: &[IpAddr]) -> LldpSwitchData {
        LldpSwitchData {
            name: format!("switch-{marker}"),
            id: format!("id={marker}"),
            description: format!("description-{marker}"),
            local_port: format!("local-{marker}"),
            ip_address: ip_addresses.to_vec(),
            remote_port: format!("remote={marker}"),
        }
    }

    fn project_model_lldp(data: LldpSwitchData) -> LldpProjection {
        LldpProjection {
            name: data.name,
            id: data.id,
            description: data.description,
            local_port: data.local_port,
            ip_addresses: data
                .ip_address
                .into_iter()
                .map(|address| address.to_string())
                .collect(),
            remote_port: data.remote_port,
        }
    }

    fn project_rpc_lldp(data: rpc::machine_discovery::LldpSwitchData) -> LldpProjection {
        LldpProjection {
            name: data.name,
            // The model still represents the legacy combined chassis identifier.
            #[allow(deprecated)]
            id: data.id,
            description: data.description,
            local_port: data.local_port,
            ip_addresses: data.ip_address,
            // The model still represents the legacy combined remote-port identifier.
            #[allow(deprecated)]
            remote_port: data.remote_port,
        }
    }

    fn project_hardware(info: HardwareInfo) -> HardwareProjection {
        HardwareProjection {
            machine_type: info.machine_type,
            network_mac_addresses: info
                .network_interfaces
                .iter()
                .map(|interface| interface.mac_address.to_string())
                .collect(),
            network_slots: info
                .network_interfaces
                .iter()
                .map(|interface| {
                    interface
                        .pci_properties
                        .as_ref()
                        .and_then(|properties| properties.slot.clone())
                })
                .collect(),
            infiniband_guids: info
                .infiniband_interfaces
                .iter()
                .map(|interface| interface.guid.clone())
                .collect(),
            infiniband_pci_properties: info
                .infiniband_interfaces
                .iter()
                .map(|interface| interface.pci_properties.as_ref().map(PciProjection::from))
                .collect(),
            cpu_info: info.cpu_info.into_iter().map(CpuProjection::from).collect(),
            block_devices: info
                .block_devices
                .into_iter()
                .map(BlockDeviceProjection::from)
                .collect(),
            nvme_devices: info
                .nvme_devices
                .into_iter()
                .map(NvmeDeviceProjection::from)
                .collect(),
            dmi_data: info.dmi_data.map(DmiProjection::from),
            tpm_ek_certificate: info
                .tpm_ek_certificate
                .as_ref()
                .map(|certificate| certificate.as_bytes().to_vec()),
            dpu_info: info.dpu_info.as_ref().map(DpuProjection::from),
            dpu_switch_names: info
                .dpu_info
                .as_ref()
                .map(|data| {
                    data.switches
                        .iter()
                        .map(|switch| switch.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            dpu_switch_addresses: info
                .dpu_info
                .as_ref()
                .map(|data| {
                    data.switches
                        .iter()
                        .map(|switch| {
                            switch
                                .ip_address
                                .iter()
                                .map(|address| address.to_string())
                                .collect()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            gpus: info.gpus.into_iter().map(GpuProjection::from).collect(),
            memory_sizes: info
                .memory_devices
                .iter()
                .flat_map(|g| g.rehydrate())
                .map(|memory| memory.size_mb)
                .collect(),
            memory_types: info
                .memory_devices
                .iter()
                .flat_map(|g| g.rehydrate())
                .map(|memory| memory.mem_type)
                .collect(),
            tpm_description: info.tpm_description,
        }
    }

    fn project_rpc_discovery(
        info: rpc::machine_discovery::DiscoveryInfo,
    ) -> RpcDiscoveryProjection {
        RpcDiscoveryProjection {
            machine_type: info.machine_type,
            machine_arch: info.machine_arch,
            network_mac_addresses: info
                .network_interfaces
                .iter()
                .map(|interface| interface.mac_address.clone())
                .collect(),
            network_slots: info
                .network_interfaces
                .iter()
                .map(|interface| {
                    interface
                        .pci_properties
                        .as_ref()
                        .and_then(|properties| properties.slot.clone())
                })
                .collect(),
            infiniband_guids: info
                .infiniband_interfaces
                .iter()
                .map(|interface| interface.guid.clone())
                .collect(),
            infiniband_pci_properties: info
                .infiniband_interfaces
                .iter()
                .map(|interface| interface.pci_properties.as_ref().map(PciProjection::from))
                .collect(),
            cpu_info: info.cpu_info.into_iter().map(CpuProjection::from).collect(),
            block_devices: info
                .block_devices
                .into_iter()
                .map(BlockDeviceProjection::from)
                .collect(),
            nvme_devices: info
                .nvme_devices
                .into_iter()
                .map(NvmeDeviceProjection::from)
                .collect(),
            dmi_data: info.dmi_data.map(DmiProjection::from),
            tpm_ek_certificate: info.tpm_ek_certificate,
            dpu_info: info.dpu_info.as_ref().map(DpuProjection::from),
            dpu_switch_names: info
                .dpu_info
                .as_ref()
                .map(|data| {
                    data.switches
                        .iter()
                        .map(|switch| switch.name.clone())
                        .collect()
                })
                .unwrap_or_default(),
            dpu_switch_addresses: info
                .dpu_info
                .as_ref()
                .map(|data| {
                    data.switches
                        .iter()
                        .map(|switch| switch.ip_address.clone())
                        .collect()
                })
                .unwrap_or_default(),
            gpus: info.gpus.into_iter().map(GpuProjection::from).collect(),
            memory_sizes: info
                .memory_device_groups
                .iter()
                .flat_map(|g| std::iter::repeat_n(g.size_mb, g.count as usize))
                .collect(),
            memory_types: info
                .memory_device_groups
                .iter()
                .flat_map(|g| std::iter::repeat_n(g.mem_type.clone(), g.count as usize))
                .collect(),
            tpm_description: info.tpm_description,
        }
    }

    fn populated_rpc_discovery() -> rpc::machine_discovery::DiscoveryInfo {
        rpc::machine_discovery::DiscoveryInfo {
            network_interfaces: vec![rpc::machine_discovery::NetworkInterface {
                mac_address: "10:20:30:40:50:60".to_string(),
                pci_properties: Some(rpc_pci("network")),
            }],
            infiniband_interfaces: vec![
                rpc::machine_discovery::InfinibandInterface {
                    guid: "ib-guid-without-pci".to_string(),
                    pci_properties: None,
                },
                rpc::machine_discovery::InfinibandInterface {
                    guid: "ib-guid-with-pci".to_string(),
                    pci_properties: Some(rpc_pci("infiniband")),
                },
            ],
            cpu_info: vec![rpc::machine_discovery::CpuInfo {
                model: "Grace CPU".to_string(),
                vendor: "NVIDIA".to_string(),
                sockets: 2,
                cores: 72,
                threads: 144,
            }],
            block_devices: vec![rpc::machine_discovery::BlockDevice {
                model: "Block Model".to_string(),
                revision: "1.2".to_string(),
                serial: "block-serial".to_string(),
                device_type: "disk".to_string(),
            }],
            machine_type: "legacy-ignored".to_string(),
            machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Aarch64 as i32),
            nvme_devices: vec![rpc::machine_discovery::NvmeDevice {
                model: "NVMe Model".to_string(),
                firmware_rev: "nvme-fw".to_string(),
                serial: "nvme-serial".to_string(),
                size_mb: Some(3_840_000),
                pci_path: Some("/devices/pci0000:00/0000:00:01.0/nvme/nvme0".to_string()),
            }],
            dmi_data: Some(rpc::machine_discovery::DmiData {
                board_name: "Board".to_string(),
                board_version: "B1".to_string(),
                bios_version: "BIOS-1".to_string(),
                bios_date: "2026-07-23".to_string(),
                product_serial: "product-serial".to_string(),
                board_serial: "board-serial".to_string(),
                chassis_serial: "chassis-serial".to_string(),
                product_name: "Product".to_string(),
                sys_vendor: "NVIDIA".to_string(),
            }),
            tpm_ek_certificate: Some("AAEC/w==".to_string()),
            dpu_info: Some(rpc::machine_discovery::DpuData {
                part_number: "900-9D3B6-00CV-A00".to_string(),
                part_description: "BlueField".to_string(),
                product_version: "A1".to_string(),
                factory_mac_address: "10:20:30:40:50:61".to_string(),
                firmware_version: "32.43.1014".to_string(),
                firmware_date: "2026-07-23".to_string(),
                switches: vec![rpc_lldp("nested", &["192.0.2.30", "2001:db8::30"])],
            }),
            gpus: vec![
                rpc::machine_discovery::Gpu {
                    name: "GPU without platform".to_string(),
                    serial: "gpu-serial-0".to_string(),
                    driver_version: "driver-0".to_string(),
                    vbios_version: "vbios-0".to_string(),
                    inforom_version: "inforom-0".to_string(),
                    total_memory: "96 GiB".to_string(),
                    frequency: "1 GHz".to_string(),
                    pci_bus_id: "0000:01:00.0".to_string(),
                    platform_info: None,
                },
                rpc::machine_discovery::Gpu {
                    name: "GPU with platform".to_string(),
                    serial: "gpu-serial-1".to_string(),
                    driver_version: "driver-1".to_string(),
                    vbios_version: "vbios-1".to_string(),
                    inforom_version: "inforom-1".to_string(),
                    total_memory: "192 GiB".to_string(),
                    frequency: "2 GHz".to_string(),
                    pci_bus_id: "0000:02:00.0".to_string(),
                    platform_info: Some(rpc::machine_discovery::GpuPlatformInfo {
                        chassis_serial: "gpu-chassis".to_string(),
                        slot_number: 4,
                        tray_index: 2,
                        host_id: 1,
                        module_id: 8,
                        fabric_guid: "fabric-guid".to_string(),
                    }),
                },
            ],
            #[allow(deprecated)]
            memory_devices: vec![],
            memory_device_groups: vec![
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(131_072),
                    mem_type: Some("DDR5".to_string()),
                    count: 1,
                },
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: None,
                    mem_type: None,
                    count: 1,
                },
            ],
            tpm_description: Some(rpc::machine_discovery::TpmDescription {
                vendor: "\0NVIDIA\0".to_string(),
                firmware_version: "\x001.2.3\0".to_string(),
                tpm_spec: "\x002.0\0".to_string(),
            }),
            attest_key_info: None,
        }
    }

    fn populated_hardware_info() -> HardwareInfo {
        HardwareInfo {
            network_interfaces: vec![NetworkInterface {
                mac_address: MacAddress::new([0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
                pci_properties: Some(model_pci("network")),
            }],
            infiniband_interfaces: vec![
                InfinibandInterface {
                    guid: "ib-guid-without-pci".to_string(),
                    pci_properties: None,
                },
                InfinibandInterface {
                    guid: "ib-guid-with-pci".to_string(),
                    pci_properties: Some(model_pci("infiniband")),
                },
            ],
            cpu_info: vec![CpuInfo {
                model: "Grace CPU".to_string(),
                vendor: "NVIDIA".to_string(),
                sockets: 2,
                cores: 72,
                threads: 144,
            }],
            block_devices: vec![BlockDevice {
                model: "Block Model".to_string(),
                revision: "1.2".to_string(),
                serial: "block-serial".to_string(),
                device_type: "disk".to_string(),
            }],
            machine_type: CpuArchitecture::Aarch64,
            nvme_devices: vec![NvmeDevice {
                model: "NVMe Model".to_string(),
                firmware_rev: "nvme-fw".to_string(),
                serial: "nvme-serial".to_string(),
                size_mb: Some(3_840_000),
                pci_path: Some("/devices/pci0000:00/0000:00:01.0/nvme/nvme0".to_string()),
            }],
            dmi_data: Some(DmiData {
                board_name: "Board".to_string(),
                board_version: "B1".to_string(),
                bios_version: "BIOS-1".to_string(),
                bios_date: "2026-07-23".to_string(),
                product_serial: "product-serial".to_string(),
                board_serial: "board-serial".to_string(),
                chassis_serial: "chassis-serial".to_string(),
                product_name: "Product".to_string(),
                sys_vendor: "NVIDIA".to_string(),
            }),
            tpm_ek_certificate: Some(TpmEkCertificate::from(vec![0, 1, 2, 255])),
            dpu_info: Some(DpuData {
                part_number: "900-9D3B6-00CV-A00".to_string(),
                part_description: "BlueField".to_string(),
                product_version: "A1".to_string(),
                factory_mac_address: "10:20:30:40:50:61".to_string(),
                firmware_version: "32.43.1014".to_string(),
                firmware_date: "2026-07-23".to_string(),
                switches: vec![model_lldp(
                    "nested",
                    &[
                        "192.0.2.30".parse().expect("valid IPv4 address"),
                        "2001:db8::30".parse().expect("valid IPv6 address"),
                    ],
                )],
            }),
            gpus: vec![
                Gpu {
                    name: "GPU without platform".to_string(),
                    serial: "gpu-serial-0".to_string(),
                    driver_version: "driver-0".to_string(),
                    vbios_version: "vbios-0".to_string(),
                    inforom_version: "inforom-0".to_string(),
                    total_memory: "96 GiB".to_string(),
                    frequency: "1 GHz".to_string(),
                    pci_bus_id: "0000:01:00.0".to_string(),
                    platform_info: None,
                },
                Gpu {
                    name: "GPU with platform".to_string(),
                    serial: "gpu-serial-1".to_string(),
                    driver_version: "driver-1".to_string(),
                    vbios_version: "vbios-1".to_string(),
                    inforom_version: "inforom-1".to_string(),
                    total_memory: "192 GiB".to_string(),
                    frequency: "2 GHz".to_string(),
                    pci_bus_id: "0000:02:00.0".to_string(),
                    platform_info: Some(GpuPlatformInfo {
                        chassis_serial: "gpu-chassis".to_string(),
                        slot_number: 4,
                        tray_index: 2,
                        host_id: 1,
                        module_id: 8,
                        fabric_guid: "fabric-guid".to_string(),
                    }),
                },
            ],
            memory_devices: vec![
                MemoryDeviceGroup {
                    size_mb: Some(131_072),
                    mem_type: Some("DDR5".to_string()),
                    count: 1,
                },
                MemoryDeviceGroup {
                    size_mb: None,
                    mem_type: None,
                    count: 1,
                },
            ],
            tpm_description: Some(TpmDescription {
                vendor: "NVIDIA".to_string(),
                firmware_version: "1.2.3".to_string(),
                tpm_spec: "2.0".to_string(),
            }),
        }
    }

    fn populated_cpu_projection() -> Vec<CpuProjection> {
        vec![CpuProjection {
            model: "Grace CPU".to_string(),
            vendor: "NVIDIA".to_string(),
            sockets: 2,
            cores: 72,
            threads: 144,
        }]
    }

    fn populated_block_device_projection() -> Vec<BlockDeviceProjection> {
        vec![BlockDeviceProjection {
            model: "Block Model".to_string(),
            revision: "1.2".to_string(),
            serial: "block-serial".to_string(),
            device_type: "disk".to_string(),
        }]
    }

    fn populated_nvme_device_projection() -> Vec<NvmeDeviceProjection> {
        vec![NvmeDeviceProjection {
            model: "NVMe Model".to_string(),
            firmware_rev: "nvme-fw".to_string(),
            serial: "nvme-serial".to_string(),
            size_mb: Some(3_840_000),
            pci_path: Some("/devices/pci0000:00/0000:00:01.0/nvme/nvme0".to_string()),
        }]
    }

    fn populated_dmi_projection() -> Option<DmiProjection> {
        Some(DmiProjection {
            board_name: "Board".to_string(),
            board_version: "B1".to_string(),
            bios_version: "BIOS-1".to_string(),
            bios_date: "2026-07-23".to_string(),
            product_serial: "product-serial".to_string(),
            board_serial: "board-serial".to_string(),
            chassis_serial: "chassis-serial".to_string(),
            product_name: "Product".to_string(),
            sys_vendor: "NVIDIA".to_string(),
        })
    }

    fn populated_dpu_projection() -> Option<DpuProjection> {
        Some(DpuProjection {
            part_number: "900-9D3B6-00CV-A00".to_string(),
            part_description: "BlueField".to_string(),
            product_version: "A1".to_string(),
            factory_mac_address: "10:20:30:40:50:61".to_string(),
            firmware_version: "32.43.1014".to_string(),
            firmware_date: "2026-07-23".to_string(),
        })
    }

    fn populated_gpu_projection() -> Vec<GpuProjection> {
        vec![
            GpuProjection {
                name: "GPU without platform".to_string(),
                serial: "gpu-serial-0".to_string(),
                driver_version: "driver-0".to_string(),
                vbios_version: "vbios-0".to_string(),
                inforom_version: "inforom-0".to_string(),
                total_memory: "96 GiB".to_string(),
                frequency: "1 GHz".to_string(),
                pci_bus_id: "0000:01:00.0".to_string(),
                platform_info: None,
            },
            GpuProjection {
                name: "GPU with platform".to_string(),
                serial: "gpu-serial-1".to_string(),
                driver_version: "driver-1".to_string(),
                vbios_version: "vbios-1".to_string(),
                inforom_version: "inforom-1".to_string(),
                total_memory: "192 GiB".to_string(),
                frequency: "2 GHz".to_string(),
                pci_bus_id: "0000:02:00.0".to_string(),
                platform_info: Some(GpuPlatformProjection {
                    chassis_serial: "gpu-chassis".to_string(),
                    slot_number: 4,
                    tray_index: 2,
                    host_id: 1,
                    module_id: 8,
                    fabric_guid: "fabric-guid".to_string(),
                }),
            },
        ]
    }

    fn empty_hardware_projection() -> HardwareProjection {
        HardwareProjection {
            machine_type: CpuArchitecture::Unknown,
            network_mac_addresses: vec![],
            network_slots: vec![],
            infiniband_guids: vec![],
            infiniband_pci_properties: vec![],
            cpu_info: vec![],
            block_devices: vec![],
            nvme_devices: vec![],
            dmi_data: None,
            tpm_ek_certificate: None,
            dpu_info: None,
            dpu_switch_names: vec![],
            dpu_switch_addresses: vec![],
            gpus: vec![],
            memory_sizes: vec![],
            memory_types: vec![],
            tpm_description: None,
        }
    }

    fn populated_hardware_projection() -> HardwareProjection {
        HardwareProjection {
            machine_type: CpuArchitecture::Aarch64,
            network_mac_addresses: vec!["10:20:30:40:50:60".to_string()],
            network_slots: vec![Some("slot-network".to_string())],
            infiniband_guids: vec![
                "ib-guid-without-pci".to_string(),
                "ib-guid-with-pci".to_string(),
            ],
            infiniband_pci_properties: vec![
                None,
                Some(PciProjection {
                    vendor: "vendor-infiniband".to_string(),
                    device: "device-infiniband".to_string(),
                    path: "/pci/infiniband".to_string(),
                    numa_node: 3,
                    description: Some("description-infiniband".to_string()),
                    slot: Some("slot-infiniband".to_string()),
                }),
            ],
            cpu_info: populated_cpu_projection(),
            block_devices: populated_block_device_projection(),
            nvme_devices: populated_nvme_device_projection(),
            dmi_data: populated_dmi_projection(),
            tpm_ek_certificate: Some(vec![0, 1, 2, 255]),
            dpu_info: populated_dpu_projection(),
            dpu_switch_names: vec!["switch-nested".to_string()],
            dpu_switch_addresses: vec![vec!["192.0.2.30".to_string(), "2001:db8::30".to_string()]],
            gpus: populated_gpu_projection(),
            memory_sizes: vec![Some(131_072), None],
            memory_types: vec![Some("DDR5".to_string()), None],
            tpm_description: Some(TpmDescription {
                vendor: "NVIDIA".to_string(),
                firmware_version: "1.2.3".to_string(),
                tpm_spec: "2.0".to_string(),
            }),
        }
    }

    fn empty_rpc_discovery_projection() -> RpcDiscoveryProjection {
        RpcDiscoveryProjection {
            machine_type: String::new(),
            machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Unknown as i32),
            network_mac_addresses: vec![],
            network_slots: vec![],
            infiniband_guids: vec![],
            infiniband_pci_properties: vec![],
            cpu_info: vec![],
            block_devices: vec![],
            nvme_devices: vec![],
            dmi_data: None,
            tpm_ek_certificate: None,
            dpu_info: None,
            dpu_switch_names: vec![],
            dpu_switch_addresses: vec![],
            gpus: vec![],
            memory_sizes: vec![],
            memory_types: vec![],
            tpm_description: None,
        }
    }

    fn populated_rpc_discovery_projection() -> RpcDiscoveryProjection {
        RpcDiscoveryProjection {
            machine_type: "aarch64".to_string(),
            machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Aarch64 as i32),
            network_mac_addresses: vec!["10:20:30:40:50:60".to_string()],
            network_slots: vec![Some("slot-network".to_string())],
            infiniband_guids: vec![
                "ib-guid-without-pci".to_string(),
                "ib-guid-with-pci".to_string(),
            ],
            infiniband_pci_properties: vec![
                None,
                Some(PciProjection {
                    vendor: "vendor-infiniband".to_string(),
                    device: "device-infiniband".to_string(),
                    path: "/pci/infiniband".to_string(),
                    numa_node: 3,
                    description: Some("description-infiniband".to_string()),
                    slot: Some("slot-infiniband".to_string()),
                }),
            ],
            cpu_info: populated_cpu_projection(),
            block_devices: populated_block_device_projection(),
            nvme_devices: populated_nvme_device_projection(),
            dmi_data: populated_dmi_projection(),
            tpm_ek_certificate: Some("AAEC/w==".to_string()),
            dpu_info: populated_dpu_projection(),
            dpu_switch_names: vec!["switch-nested".to_string()],
            dpu_switch_addresses: vec![vec!["192.0.2.30".to_string(), "2001:db8::30".to_string()]],
            gpus: populated_gpu_projection(),
            memory_sizes: vec![Some(131_072), None],
            memory_types: vec![Some("DDR5".to_string()), None],
            tpm_description: Some(rpc::machine_discovery::TpmDescription {
                vendor: "NVIDIA".to_string(),
                firmware_version: "1.2.3".to_string(),
                tpm_spec: "2.0".to_string(),
            }),
        }
    }

    fn project_model_inventory(inventory: MachineInventory) -> InventoryProjection {
        InventoryProjection {
            components: inventory
                .components
                .into_iter()
                .map(|component| InventoryComponentProjection {
                    name: component.name,
                    version: component.version,
                    url: component.url,
                })
                .collect(),
        }
    }

    fn project_rpc_inventory(inventory: rpc::forge::MachineInventory) -> InventoryProjection {
        InventoryProjection {
            components: inventory
                .components
                .into_iter()
                .map(|component| InventoryComponentProjection {
                    name: component.name,
                    version: component.version,
                    url: component.url,
                })
                .collect(),
        }
    }

    fn nvlink_domain() -> NvLinkDomainId {
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
            .parse()
            .expect("valid NVLink domain ID")
    }

    fn project_model_nvlink(info: MachineNvLinkInfo) -> NvLinkProjection {
        NvLinkProjection {
            domain_uuid: Some(info.domain_uuid),
            chassis_serial: info.chassis_serial,
            gpus: info
                .gpus
                .into_iter()
                .map(|gpu| NvLinkGpuProjection {
                    tray_index: gpu.tray_index,
                    slot_id: gpu.slot_id,
                    device_id: gpu.device_id,
                    guid: gpu.guid,
                })
                .collect(),
        }
    }

    fn project_rpc_nvlink(info: rpc::forge::MachineNvLinkInfo) -> NvLinkProjection {
        NvLinkProjection {
            domain_uuid: info.domain_uuid,
            chassis_serial: info.chassis_serial,
            gpus: info
                .gpus
                .into_iter()
                .map(|gpu| NvLinkGpuProjection {
                    tray_index: gpu.tray_index,
                    slot_id: gpu.slot_id,
                    device_id: gpu.device_id,
                    guid: gpu.guid,
                })
                .collect(),
        }
    }

    #[test]
    fn architecture_precedence_cases() {
        check_values(
            [
                Check {
                    scenario: "modern Aarch64 architecture",
                    input: ArchitectureInput {
                        machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Aarch64 as i32),
                        legacy_machine_type: "",
                    },
                    expect: CpuArchitecture::Aarch64,
                },
                Check {
                    scenario: "modern x86 architecture",
                    input: ArchitectureInput {
                        machine_arch: Some(rpc::machine_discovery::CpuArchitecture::X8664 as i32),
                        legacy_machine_type: "",
                    },
                    expect: CpuArchitecture::X86_64,
                },
                Check {
                    scenario: "modern architecture takes precedence over legacy value",
                    input: ArchitectureInput {
                        machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Aarch64 as i32),
                        legacy_machine_type: "x86_64",
                    },
                    expect: CpuArchitecture::Aarch64,
                },
                Check {
                    scenario: "unknown modern numeric value takes precedence over legacy value",
                    input: ArchitectureInput {
                        machine_arch: Some(99),
                        legacy_machine_type: "aarch64",
                    },
                    expect: CpuArchitecture::Unknown,
                },
                Check {
                    scenario: "legacy Aarch64 architecture",
                    input: ArchitectureInput {
                        machine_arch: None,
                        legacy_machine_type: "aarch64",
                    },
                    expect: CpuArchitecture::Aarch64,
                },
                Check {
                    scenario: "legacy x86 architecture",
                    input: ArchitectureInput {
                        machine_arch: None,
                        legacy_machine_type: "x86_64",
                    },
                    expect: CpuArchitecture::X86_64,
                },
                Check {
                    scenario: "invalid legacy architecture becomes unknown",
                    input: ArchitectureInput {
                        machine_arch: None,
                        legacy_machine_type: "riscv64",
                    },
                    expect: CpuArchitecture::Unknown,
                },
            ],
            |input| {
                HardwareInfo::try_from(rpc::machine_discovery::DiscoveryInfo {
                    machine_arch: input.machine_arch,
                    machine_type: input.legacy_machine_type.to_string(),
                    ..Default::default()
                })
                .expect("architecture-only discovery info converts")
                .machine_type
            },
        );
    }

    #[test]
    fn network_interface_inbound_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "ordinary MAC without PCI properties",
                    input: rpc::machine_discovery::NetworkInterface {
                        mac_address: "10:20:30:40:50:60".to_string(),
                        pci_properties: None,
                    },
                    expect: Yields(NetworkInterface {
                        mac_address: MacAddress::new([0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
                        pci_properties: None,
                    }),
                },
                Case {
                    scenario: "Mellanox sentinel is rewritten and PCI properties are preserved",
                    input: rpc::machine_discovery::NetworkInterface {
                        mac_address: MELLANOX_SF_VF_MAC_ADDRESS_IN.to_string(),
                        pci_properties: Some(rpc_pci("network")),
                    },
                    expect: Yields(NetworkInterface {
                        mac_address: MacAddress::new([0, 0, 0, 0, 0, 0x64]),
                        pci_properties: Some(model_pci("network")),
                    }),
                },
                Case {
                    scenario: "malformed MAC is rejected",
                    input: rpc::machine_discovery::NetworkInterface {
                        mac_address: "not-a-mac".to_string(),
                        pci_properties: None,
                    },
                    expect: Fails,
                },
            ],
            |value| NetworkInterface::try_from(value).map_err(drop),
        );
    }

    #[test]
    fn network_interface_outbound_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "model interface without PCI properties converts back to RPC",
                    input: NetworkInterface {
                        mac_address: MacAddress::new([0x10, 0x20, 0x30, 0x40, 0x50, 0x60]),
                        pci_properties: None,
                    },
                    expect: Yields(rpc::machine_discovery::NetworkInterface {
                        mac_address: "10:20:30:40:50:60".to_string(),
                        pci_properties: None,
                    }),
                },
                Case {
                    scenario: "model PCI properties convert back to RPC",
                    input: NetworkInterface {
                        mac_address: MacAddress::new([0x10, 0x20, 0x30, 0x40, 0x50, 0x61]),
                        pci_properties: Some(model_pci("outbound")),
                    },
                    expect: Yields(rpc::machine_discovery::NetworkInterface {
                        mac_address: "10:20:30:40:50:61".to_string(),
                        pci_properties: Some(rpc_pci("outbound")),
                    }),
                },
            ],
            |value| rpc::machine_discovery::NetworkInterface::try_from(value).map_err(drop),
        );
    }

    #[test]
    fn lldp_inbound_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "IPv4 management address",
                    input: rpc_lldp("ipv4", &["192.0.2.10"]),
                    expect: Yields(LldpProjection {
                        name: "switch-ipv4".to_string(),
                        id: "id=ipv4".to_string(),
                        description: "description-ipv4".to_string(),
                        local_port: "local-ipv4".to_string(),
                        ip_addresses: vec!["192.0.2.10".to_string()],
                        remote_port: "remote=ipv4".to_string(),
                    }),
                },
                Case {
                    scenario: "IPv6 management address",
                    input: rpc_lldp("ipv6", &["2001:db8::10"]),
                    expect: Yields(LldpProjection {
                        name: "switch-ipv6".to_string(),
                        id: "id=ipv6".to_string(),
                        description: "description-ipv6".to_string(),
                        local_port: "local-ipv6".to_string(),
                        ip_addresses: vec!["2001:db8::10".to_string()],
                        remote_port: "remote=ipv6".to_string(),
                    }),
                },
                Case {
                    scenario: "invalid management address is rejected",
                    input: rpc_lldp("invalid", &["not-an-ip"]),
                    expect: Fails,
                },
            ],
            |value| {
                LldpSwitchData::try_from(value)
                    .map(project_model_lldp)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn lldp_outbound_conversion_cases() {
        check_cases(
            [Case {
                scenario: "model management addresses convert back to strings",
                input: model_lldp(
                    "reverse",
                    &[
                        "192.0.2.20".parse().expect("valid IPv4 address"),
                        "2001:db8::20".parse().expect("valid IPv6 address"),
                    ],
                ),
                expect: Yields(LldpProjection {
                    name: "switch-reverse".to_string(),
                    id: "id=reverse".to_string(),
                    description: "description-reverse".to_string(),
                    local_port: "local-reverse".to_string(),
                    ip_addresses: vec!["192.0.2.20".to_string(), "2001:db8::20".to_string()],
                    remote_port: "remote=reverse".to_string(),
                }),
            }],
            |value| {
                rpc::machine_discovery::LldpSwitchData::try_from(value)
                    .map(project_rpc_lldp)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn discovery_info_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "sparse discovery info",
                    input: rpc::machine_discovery::DiscoveryInfo::default(),
                    expect: Yields(empty_hardware_projection()),
                },
                Case {
                    scenario: "populated discovery info projects nested hardware data",
                    input: populated_rpc_discovery(),
                    expect: Yields(populated_hardware_projection()),
                },
                Case {
                    scenario: "old flat memory_devices are condensed on read",
                    input: rpc::machine_discovery::DiscoveryInfo {
                        #[allow(deprecated)]
                        memory_devices: vec![
                            rpc::machine_discovery::MemoryDevice {
                                size_mb: Some(65_536),
                                mem_type: Some("DDR4".to_string()),
                            },
                            rpc::machine_discovery::MemoryDevice {
                                size_mb: Some(65_536),
                                mem_type: Some("DDR4".to_string()),
                            },
                        ],
                        machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Unknown as i32),
                        ..Default::default()
                    },
                    expect: Yields(HardwareProjection {
                        memory_sizes: vec![Some(65_536), Some(65_536)],
                        memory_types: vec![Some("DDR4".to_string()), Some("DDR4".to_string())],
                        ..empty_hardware_projection()
                    }),
                },
                Case {
                    scenario: "invalid TPM certificate base64 is rejected",
                    input: rpc::machine_discovery::DiscoveryInfo {
                        tpm_ek_certificate: Some("not-base64".to_string()),
                        ..Default::default()
                    },
                    expect: Fails,
                },
                Case {
                    scenario: "nested invalid LLDP address is propagated",
                    input: rpc::machine_discovery::DiscoveryInfo {
                        dpu_info: Some(rpc::machine_discovery::DpuData {
                            switches: vec![rpc_lldp("nested-invalid", &["not-an-ip"])],
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    expect: Fails,
                },
            ],
            |info| {
                HardwareInfo::try_from(info)
                    .map(project_hardware)
                    .map_err(drop)
            },
        );
    }

    // Verifies that MemoryDeviceGroups survive a round trip through the rpc DiscoveryInfo proto.
    #[test]
    fn memory_device_groups_round_trip() {
        let original = vec![
            MemoryDeviceGroup {
                size_mb: Some(65_536),
                mem_type: Some("DDR5".to_string()),
                count: 8,
            },
            MemoryDeviceGroup {
                size_mb: Some(32_768),
                mem_type: Some("DDR4".to_string()),
                count: 4,
            },
            MemoryDeviceGroup {
                size_mb: None,
                mem_type: None,
                count: 2,
            },
        ];

        let hardware_info = HardwareInfo {
            memory_devices: original.clone(),
            ..Default::default()
        };

        let discovery_info = rpc::machine_discovery::DiscoveryInfo::try_from(hardware_info)
            .expect("serialization succeeds");
        let recovered = HardwareInfo::try_from(discovery_info)
            .expect("deserialization succeeds")
            .memory_devices;

        assert_eq!(recovered, original);
    }

    // Verifies that an empty `memory_device_groups` list survives a JSON round trip.
    // `memory_device_groups` is `skip_serializing_if = "Vec::is_empty"`, so an empty list is
    // omitted from the JSON entirely; without `#[serde(default)]` on that field, deserializing
    // that JSON would fail with "missing field `memory_device_groups`".
    #[test]
    fn memory_device_groups_empty_json_round_trip() {
        let discovery_info = rpc::machine_discovery::DiscoveryInfo {
            memory_device_groups: vec![],
            ..Default::default()
        };

        let json = serde_json::to_string(&discovery_info).expect("serialization succeeds");
        assert!(
            !json.contains("memory_device_groups"),
            "empty memory_device_groups should be omitted from JSON: {json}"
        );

        let recovered: rpc::machine_discovery::DiscoveryInfo =
            serde_json::from_str(&json).expect("deserialization succeeds");
        assert_eq!(recovered.memory_device_groups, Vec::new());
    }

    // Verifies that the internal `count` field is set correctly when reading both
    // old and new proto formats — not just that rehydration produces the right flat list.
    #[test]
    fn memory_device_group_count_after_conversion() {
        check_values(
            [
                Check {
                    scenario: "new format: count is preserved from memory_device_groups",
                    input: rpc::machine_discovery::DiscoveryInfo {
                        memory_device_groups: vec![rpc::machine_discovery::MemoryDeviceGroup {
                            size_mb: Some(65_536),
                            mem_type: Some("DDR4".to_string()),
                            count: 8,
                        }],
                        machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Unknown as i32),
                        ..Default::default()
                    },
                    expect: vec![MemoryDeviceGroup {
                        size_mb: Some(65_536),
                        mem_type: Some("DDR4".to_string()),
                        count: 8,
                    }],
                },
                Check {
                    scenario: "old format: identical flat entries are condensed into count > 1",
                    input: rpc::machine_discovery::DiscoveryInfo {
                        #[allow(deprecated)]
                        memory_devices: vec![
                            rpc::machine_discovery::MemoryDevice {
                                size_mb: Some(65_536),
                                mem_type: Some("DDR4".to_string()),
                            },
                            rpc::machine_discovery::MemoryDevice {
                                size_mb: Some(65_536),
                                mem_type: Some("DDR4".to_string()),
                            },
                        ],
                        machine_arch: Some(rpc::machine_discovery::CpuArchitecture::Unknown as i32),
                        ..Default::default()
                    },
                    expect: vec![MemoryDeviceGroup {
                        size_mb: Some(65_536),
                        mem_type: Some("DDR4".to_string()),
                        count: 2,
                    }],
                },
            ],
            |info| {
                HardwareInfo::try_from(info)
                    .expect("conversion succeeds")
                    .memory_devices
            },
        );
    }

    #[test]
    fn discovery_info_reverse_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "sparse hardware info",
                    input: HardwareInfo::default(),
                    expect: Yields(empty_rpc_discovery_projection()),
                },
                Case {
                    scenario: "populated hardware info projects nested RPC data",
                    input: populated_hardware_info(),
                    expect: Yields(populated_rpc_discovery_projection()),
                },
            ],
            |info| {
                rpc::machine_discovery::DiscoveryInfo::try_from(info)
                    .map(project_rpc_discovery)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn machine_inventory_inbound_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "empty RPC inventory",
                    input: rpc::forge::MachineInventory { components: vec![] },
                    expect: Yields(InventoryProjection { components: vec![] }),
                },
                Case {
                    scenario: "populated RPC inventory preserves component order",
                    input: rpc::forge::MachineInventory {
                        components: vec![
                            rpc::forge::MachineInventorySoftwareComponent {
                                name: "firmware".to_string(),
                                version: "1.2.3".to_string(),
                                url: "https://example.com/firmware".to_string(),
                            },
                            rpc::forge::MachineInventorySoftwareComponent {
                                name: "driver".to_string(),
                                version: "4.5.6".to_string(),
                                url: "https://example.com/driver".to_string(),
                            },
                        ],
                    },
                    expect: Yields(InventoryProjection {
                        components: vec![
                            InventoryComponentProjection {
                                name: "firmware".to_string(),
                                version: "1.2.3".to_string(),
                                url: "https://example.com/firmware".to_string(),
                            },
                            InventoryComponentProjection {
                                name: "driver".to_string(),
                                version: "4.5.6".to_string(),
                                url: "https://example.com/driver".to_string(),
                            },
                        ],
                    }),
                },
            ],
            |value| {
                MachineInventory::try_from(value)
                    .map(project_model_inventory)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn machine_inventory_outbound_conversion_cases() {
        check_values(
            [Check {
                scenario: "model inventory converts back to RPC",
                input: MachineInventory {
                    components: vec![
                        MachineInventorySoftwareComponent {
                            name: "agent".to_string(),
                            version: "7.8.9".to_string(),
                            url: "https://example.com/agent".to_string(),
                        },
                        MachineInventorySoftwareComponent {
                            name: "runtime".to_string(),
                            version: "10.11.12".to_string(),
                            url: "https://example.com/runtime".to_string(),
                        },
                    ],
                },
                expect: InventoryProjection {
                    components: vec![
                        InventoryComponentProjection {
                            name: "agent".to_string(),
                            version: "7.8.9".to_string(),
                            url: "https://example.com/agent".to_string(),
                        },
                        InventoryComponentProjection {
                            name: "runtime".to_string(),
                            version: "10.11.12".to_string(),
                            url: "https://example.com/runtime".to_string(),
                        },
                    ],
                },
            }],
            |value| project_rpc_inventory(rpc::forge::MachineInventory::from(value)),
        );
    }

    #[test]
    fn nvlink_inbound_conversion_cases() {
        check_cases(
            [
                Case {
                    scenario: "RPC NVLink domain without GPUs",
                    input: rpc::forge::MachineNvLinkInfo {
                        domain_uuid: Some(nvlink_domain()),
                        chassis_serial: "chassis-empty".to_string(),
                        gpus: vec![],
                    },
                    expect: Yields(NvLinkProjection {
                        domain_uuid: Some(nvlink_domain()),
                        chassis_serial: "chassis-empty".to_string(),
                        gpus: vec![],
                    }),
                },
                Case {
                    scenario: "RPC NVLink domain preserves populated GPU order",
                    input: rpc::forge::MachineNvLinkInfo {
                        domain_uuid: Some(nvlink_domain()),
                        chassis_serial: "chassis-populated".to_string(),
                        gpus: vec![
                            rpc::forge::NvLinkGpu {
                                tray_index: 0,
                                slot_id: 1,
                                device_id: 2,
                                guid: 100,
                            },
                            rpc::forge::NvLinkGpu {
                                tray_index: 3,
                                slot_id: 4,
                                device_id: 5,
                                guid: 200,
                            },
                        ],
                    },
                    expect: Yields(NvLinkProjection {
                        domain_uuid: Some(nvlink_domain()),
                        chassis_serial: "chassis-populated".to_string(),
                        gpus: vec![
                            NvLinkGpuProjection {
                                tray_index: 0,
                                slot_id: 1,
                                device_id: 2,
                                guid: 100,
                            },
                            NvLinkGpuProjection {
                                tray_index: 3,
                                slot_id: 4,
                                device_id: 5,
                                guid: 200,
                            },
                        ],
                    }),
                },
                Case {
                    scenario: "missing NVLink domain is rejected",
                    input: rpc::forge::MachineNvLinkInfo {
                        domain_uuid: None,
                        chassis_serial: "missing-domain".to_string(),
                        gpus: vec![],
                    },
                    expect: Fails,
                },
            ],
            |value| {
                MachineNvLinkInfo::try_from(value)
                    .map(project_model_nvlink)
                    .map_err(drop)
            },
        );
    }

    #[test]
    fn nvlink_outbound_conversion_cases() {
        check_values(
            [Check {
                scenario: "model NVLink domain converts back to RPC",
                input: MachineNvLinkInfo {
                    domain_uuid: nvlink_domain(),
                    chassis_serial: "chassis-reverse".to_string(),
                    gpus: vec![
                        NvLinkGpu {
                            tray_index: 6,
                            slot_id: 7,
                            device_id: 8,
                            guid: 300,
                        },
                        NvLinkGpu {
                            tray_index: 9,
                            slot_id: 10,
                            device_id: 11,
                            guid: 400,
                        },
                    ],
                },
                expect: NvLinkProjection {
                    domain_uuid: Some(nvlink_domain()),
                    chassis_serial: "chassis-reverse".to_string(),
                    gpus: vec![
                        NvLinkGpuProjection {
                            tray_index: 6,
                            slot_id: 7,
                            device_id: 8,
                            guid: 300,
                        },
                        NvLinkGpuProjection {
                            tray_index: 9,
                            slot_id: 10,
                            device_id: 11,
                            guid: 400,
                        },
                    ],
                },
            }],
            |value| project_rpc_nvlink(rpc::forge::MachineNvLinkInfo::from(value)),
        );
    }

    // Zero-count groups in the proto `memory_device_groups` field are dropped and do
    // not appear in the converted `HardwareInfo.memory_devices`.
    #[test]
    fn discovery_info_conversion_drops_zero_count_memory_groups() {
        let info = rpc::machine_discovery::DiscoveryInfo {
            memory_device_groups: vec![
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                    count: 4,
                },
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(8192),
                    mem_type: Some("DDR4".to_string()),
                    count: 0,
                },
            ],
            ..Default::default()
        };
        let hw = HardwareInfo::try_from(info).unwrap();
        assert_eq!(
            hw.memory_devices,
            vec![MemoryDeviceGroup {
                size_mb: Some(16384),
                mem_type: Some("DDR5".to_string()),
                count: 4,
            }]
        );
    }

    // When every group in `memory_device_groups` has count == 0, the field is
    // logically empty.  Per the wire contract, readers must fall back to the
    // legacy `memory_devices` field instead of treating the all-zero groups as
    // authoritative and discarding the legacy data.
    #[test]
    fn discovery_info_conversion_falls_back_to_legacy_when_all_groups_are_zero_count() {
        #[allow(deprecated)]
        let info = rpc::machine_discovery::DiscoveryInfo {
            memory_device_groups: vec![rpc::machine_discovery::MemoryDeviceGroup {
                size_mb: Some(8192),
                mem_type: Some("DDR4".to_string()),
                count: 0,
            }],
            memory_devices: vec![
                rpc::machine_discovery::MemoryDevice {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                },
                rpc::machine_discovery::MemoryDevice {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                },
            ],
            ..Default::default()
        };
        let hw = HardwareInfo::try_from(info).unwrap();
        assert_eq!(
            hw.memory_devices,
            vec![MemoryDeviceGroup {
                size_mb: Some(16384),
                mem_type: Some("DDR5".to_string()),
                count: 2,
            }]
        );
    }

    // Groups with the same `(size_mb, mem_type)` separated by a dropped
    // zero-count group must merge into a single group, matching the invariant
    // enforced on every other ingestion path (legacy flat format, JSON
    // deserialization).
    #[test]
    fn discovery_info_conversion_merges_groups_across_dropped_zero_count_group() {
        let info = rpc::machine_discovery::DiscoveryInfo {
            memory_device_groups: vec![
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                    count: 4,
                },
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(8192),
                    mem_type: Some("DDR4".to_string()),
                    count: 0,
                },
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                    count: 2,
                },
            ],
            ..Default::default()
        };
        let hw = HardwareInfo::try_from(info).unwrap();
        assert_eq!(
            hw.memory_devices,
            vec![MemoryDeviceGroup {
                size_mb: Some(16384),
                mem_type: Some("DDR5".to_string()),
                count: 6,
            }]
        );
    }

    // A `count` above `MAX_MEMORY_DEVICE_COUNT` is rejected, otherwise
    // it would let `rehydrate` allocate an unbounded number of `MemoryDevice`s.
    #[test]
    fn discovery_info_conversion_rejects_excessive_memory_group_count() {
        let info = rpc::machine_discovery::DiscoveryInfo {
            memory_device_groups: vec![rpc::machine_discovery::MemoryDeviceGroup {
                size_mb: Some(16384),
                mem_type: Some("DDR5".to_string()),
                count: model::hardware_info::MAX_MEMORY_DEVICE_COUNT + 1,
            }],
            ..Default::default()
        };
        assert!(HardwareInfo::try_from(info).is_err());
    }

    // Distinct groups, each individually within `MAX_MEMORY_DEVICE_COUNT`, must
    // be rejected if their total count across the whole list exceeds the max.
    #[test]
    fn discovery_info_conversion_rejects_distinct_groups_summing_above_max() {
        let above_half_max = model::hardware_info::MAX_MEMORY_DEVICE_COUNT / 2 + 1;
        let info = rpc::machine_discovery::DiscoveryInfo {
            memory_device_groups: vec![
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                    count: above_half_max,
                },
                rpc::machine_discovery::MemoryDeviceGroup {
                    size_mb: Some(8192),
                    mem_type: Some("DDR4".to_string()),
                    count: above_half_max,
                },
            ],
            ..Default::default()
        };
        assert!(HardwareInfo::try_from(info).is_err());
    }

    // The legacy flat `memory_devices` field (used when `memory_device_groups` is empty) must
    // also be rejected once condensing it would exceed `MAX_MEMORY_DEVICE_COUNT`.
    #[test]
    fn discovery_info_conversion_rejects_excessive_legacy_memory_device_count() {
        #[allow(deprecated)]
        let info = rpc::machine_discovery::DiscoveryInfo {
            memory_devices: (0..model::hardware_info::MAX_MEMORY_DEVICE_COUNT + 1)
                .map(|_| rpc::machine_discovery::MemoryDevice {
                    size_mb: Some(16384),
                    mem_type: Some("DDR5".to_string()),
                })
                .collect(),
            ..Default::default()
        };
        assert!(HardwareInfo::try_from(info).is_err());
    }
}
