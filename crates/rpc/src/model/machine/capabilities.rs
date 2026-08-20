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

use model::machine::capabilities::{
    MachineCapabilitiesSet, MachineCapabilityCpu, MachineCapabilityDeviceType,
    MachineCapabilityDpu, MachineCapabilityGpu, MachineCapabilityInfiniband,
    MachineCapabilityMemory, MachineCapabilityNetwork, MachineCapabilityStorage,
    MachineCapabilityType,
};

use crate::errors::RpcDataConversionError;
use crate::forge as rpc;

impl From<MachineCapabilityType> for rpc::MachineCapabilityType {
    fn from(t: MachineCapabilityType) -> Self {
        match t {
            MachineCapabilityType::Cpu => rpc::MachineCapabilityType::CapTypeCpu,
            MachineCapabilityType::Gpu => rpc::MachineCapabilityType::CapTypeGpu,
            MachineCapabilityType::Memory => rpc::MachineCapabilityType::CapTypeMemory,
            MachineCapabilityType::Storage => rpc::MachineCapabilityType::CapTypeStorage,
            MachineCapabilityType::Network => rpc::MachineCapabilityType::CapTypeNetwork,
            MachineCapabilityType::Infiniband => rpc::MachineCapabilityType::CapTypeInfiniband,
            MachineCapabilityType::Dpu => rpc::MachineCapabilityType::CapTypeDpu,
        }
    }
}

impl TryFrom<rpc::MachineCapabilityType> for MachineCapabilityType {
    type Error = RpcDataConversionError;

    fn try_from(t: rpc::MachineCapabilityType) -> Result<Self, Self::Error> {
        match t {
            rpc::MachineCapabilityType::CapTypeInvalid => Err(
                RpcDataConversionError::InvalidArgument(t.as_str_name().to_string()),
            ),
            rpc::MachineCapabilityType::CapTypeCpu => Ok(MachineCapabilityType::Cpu),
            rpc::MachineCapabilityType::CapTypeGpu => Ok(MachineCapabilityType::Gpu),
            rpc::MachineCapabilityType::CapTypeMemory => Ok(MachineCapabilityType::Memory),
            rpc::MachineCapabilityType::CapTypeStorage => Ok(MachineCapabilityType::Storage),
            rpc::MachineCapabilityType::CapTypeNetwork => Ok(MachineCapabilityType::Network),
            rpc::MachineCapabilityType::CapTypeInfiniband => Ok(MachineCapabilityType::Infiniband),
            rpc::MachineCapabilityType::CapTypeDpu => Ok(MachineCapabilityType::Dpu),
        }
    }
}

impl From<MachineCapabilityCpu> for rpc::MachineCapabilityAttributesCpu {
    fn from(cap: MachineCapabilityCpu) -> Self {
        rpc::MachineCapabilityAttributesCpu {
            name: cap.name,
            count: cap.count,
            vendor: cap.vendor,
            cores: cap.cores,
            threads: cap.threads,
        }
    }
}

impl From<MachineCapabilityGpu> for rpc::MachineCapabilityAttributesGpu {
    fn from(cap: MachineCapabilityGpu) -> Self {
        rpc::MachineCapabilityAttributesGpu {
            name: cap.name,
            frequency: cap.frequency,
            vendor: cap.vendor,
            count: cap.count,
            capacity: cap.memory_capacity,
            cores: cap.cores,
            threads: cap.threads,
            device_type: cap
                .device_type
                .map(|dt| rpc::MachineCapabilityDeviceType::from(dt).into()),
        }
    }
}

impl From<MachineCapabilityMemory> for rpc::MachineCapabilityAttributesMemory {
    fn from(cap: MachineCapabilityMemory) -> Self {
        rpc::MachineCapabilityAttributesMemory {
            name: cap.name,
            count: cap.count,
            vendor: cap.vendor,
            capacity: cap.capacity,
        }
    }
}

impl From<MachineCapabilityStorage> for rpc::MachineCapabilityAttributesStorage {
    fn from(cap: MachineCapabilityStorage) -> Self {
        rpc::MachineCapabilityAttributesStorage {
            name: cap.name,
            count: cap.count,
            vendor: cap.vendor,
            capacity: cap.capacity,
        }
    }
}

impl From<MachineCapabilityNetwork> for rpc::MachineCapabilityAttributesNetwork {
    fn from(cap: MachineCapabilityNetwork) -> Self {
        rpc::MachineCapabilityAttributesNetwork {
            name: cap.name,
            count: cap.count,
            vendor: cap.vendor,
            device_type: cap
                .device_type
                .map(|dt| rpc::MachineCapabilityDeviceType::from(dt).into()),
        }
    }
}

impl From<MachineCapabilityInfiniband> for rpc::MachineCapabilityAttributesInfiniband {
    fn from(cap: MachineCapabilityInfiniband) -> Self {
        rpc::MachineCapabilityAttributesInfiniband {
            name: cap.name,
            vendor: Some(cap.vendor),
            count: cap.count,
            inactive_devices: cap.inactive_devices,
        }
    }
}

impl From<MachineCapabilityDpu> for rpc::MachineCapabilityAttributesDpu {
    fn from(cap: MachineCapabilityDpu) -> Self {
        rpc::MachineCapabilityAttributesDpu {
            name: cap.name,
            count: cap.count,
            hardware_revision: cap.hardware_revision,
        }
    }
}

impl From<MachineCapabilitiesSet> for rpc::MachineCapabilitiesSet {
    fn from(cap_set: MachineCapabilitiesSet) -> Self {
        rpc::MachineCapabilitiesSet {
            cpu: cap_set.cpu.into_iter().map(|cap| cap.into()).collect(),
            gpu: cap_set.gpu.into_iter().map(|cap| cap.into()).collect(),
            memory: cap_set.memory.into_iter().map(|cap| cap.into()).collect(),
            storage: cap_set.storage.into_iter().map(|cap| cap.into()).collect(),
            network: cap_set.network.into_iter().map(|cap| cap.into()).collect(),
            infiniband: cap_set
                .infiniband
                .into_iter()
                .map(|cap| cap.into())
                .collect(),
            dpu: cap_set.dpu.into_iter().map(|cap| cap.into()).collect(),
        }
    }
}

impl From<MachineCapabilityDeviceType> for rpc::MachineCapabilityDeviceType {
    fn from(t: MachineCapabilityDeviceType) -> Self {
        match t {
            MachineCapabilityDeviceType::Unknown => rpc::MachineCapabilityDeviceType::Unknown,
            MachineCapabilityDeviceType::Dpu => rpc::MachineCapabilityDeviceType::Dpu,
            MachineCapabilityDeviceType::NvLink => rpc::MachineCapabilityDeviceType::Nvlink,
            MachineCapabilityDeviceType::Spx => rpc::MachineCapabilityDeviceType::Spx,
        }
    }
}

impl TryFrom<rpc::MachineCapabilityDeviceType> for MachineCapabilityDeviceType {
    type Error = RpcDataConversionError;

    fn try_from(t: rpc::MachineCapabilityDeviceType) -> Result<Self, Self::Error> {
        match t {
            rpc::MachineCapabilityDeviceType::Unknown => Ok(MachineCapabilityDeviceType::Unknown),
            rpc::MachineCapabilityDeviceType::Dpu => Ok(MachineCapabilityDeviceType::Dpu),
            rpc::MachineCapabilityDeviceType::Nvlink => Ok(MachineCapabilityDeviceType::NvLink),
            rpc::MachineCapabilityDeviceType::Spx => Ok(MachineCapabilityDeviceType::Spx),
        }
    }
}

/* ********************************** */
/*              Tests                 */
/* ********************************** */
