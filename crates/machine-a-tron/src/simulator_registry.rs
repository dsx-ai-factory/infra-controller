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
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use bmc_mock::injection::InjectionStore;
use carbide_uuid::machine::MachineId;
use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use uuid::Uuid;

use crate::DeviceHandle;
use crate::device_simulator::{DeviceSimulator, SimulatorLifecycle};
use crate::rack::{
    RackInstance, RackMemberRef, RackMemberStatus, RackRegistration, RackStatus,
    RacksStatusResponse,
};
use crate::status::DeviceStatusConfig;

#[derive(Debug, Clone)]
pub struct SimulatorRegistry {
    inner: Arc<SimulatorRegistryInner>,
}

#[derive(Debug)]
struct SimulatorRegistryInner {
    devices: Vec<DeviceSimulator>,
    by_mat_id: HashMap<Uuid, usize>,
    /// Devices by BMC MAC, which is how an RMS power request names them.
    by_bmc_mac: HashMap<MacAddress, usize>,
    racks: BTreeMap<RackId, RackInstance>,
}

pub(crate) struct SimulatorRegistryBuilder {
    devices: Option<Vec<DeviceSimulator>>,
    rack_registrations: Vec<RackRegistration>,
}

impl SimulatorRegistry {
    pub(crate) fn builder() -> SimulatorRegistryBuilder {
        SimulatorRegistryBuilder {
            devices: None,
            rack_registrations: Vec::new(),
        }
    }

    pub fn try_from_handles(handles: Vec<DeviceHandle>) -> eyre::Result<Self> {
        Self::try_from_simulators(
            handles
                .into_iter()
                .map(DeviceSimulator::from_handle)
                .collect(),
        )
    }

    pub fn try_from_simulators(devices: Vec<DeviceSimulator>) -> eyre::Result<Self> {
        Self::builder().devices(devices).build()
    }

    fn try_from_builder(builder: SimulatorRegistryBuilder) -> eyre::Result<Self> {
        let SimulatorRegistryBuilder {
            devices,
            rack_registrations,
        } = builder;
        let devices =
            devices.ok_or_else(|| eyre::eyre!("simulator registry devices were not configured"))?;
        let mut by_mat_id = HashMap::with_capacity(devices.len());
        let mut by_bmc_mac = HashMap::with_capacity(devices.len());

        for (index, device) in devices.iter().enumerate() {
            if by_mat_id.insert(device.mat_id(), index).is_some() {
                for device in &devices {
                    device.abort();
                }
                eyre::bail!("duplicate simulator identity: {}", device.mat_id());
            }
            // The first device keeps a MAC two devices share; the BMC listener
            // can serve only one of them anyway.
            by_bmc_mac
                .entry(device.handle().host_info().bmc_mac_address)
                .or_insert(index);
        }

        let by_config_section = devices
            .iter()
            .enumerate()
            .map(|(index, device)| (device.handle().machine_config_section(), index))
            .collect::<HashMap<_, _>>();
        let mut racks = BTreeMap::new();
        for registration in rack_registrations {
            let members = registration
                .members
                .into_iter()
                .map(|member| {
                    let device_index = by_config_section
                        .get(member.machine_config_section.as_str())
                        .copied()
                        .ok_or_else(|| {
                            eyre::eyre!(
                                "rack {} unit {} ({}) has no simulator",
                                registration.rack_id,
                                member.placement.position(),
                                member.hardware_type
                            )
                        })?;
                    Ok(RackMemberRef {
                        placement: member.placement,
                        device_index,
                    })
                })
                .collect::<eyre::Result<Vec<_>>>()?;
            let rack_id = registration.rack_id.clone();
            let instance = RackInstance {
                rack_id: registration.rack_id,
                rack_type: registration.rack_type,
                version: registration.version,
                members,
            };
            eyre::ensure!(
                racks.insert(rack_id.clone(), instance).is_none(),
                "duplicate rack instance {rack_id}"
            );
        }

        Ok(Self {
            inner: Arc::new(SimulatorRegistryInner {
                devices,
                by_mat_id,
                by_bmc_mac,
                racks,
            }),
        })
    }

    pub fn devices(&self) -> &[DeviceSimulator] {
        &self.inner.devices
    }

    pub fn get(&self, mat_id: Uuid) -> Option<&DeviceSimulator> {
        self.inner
            .by_mat_id
            .get(&mat_id)
            .map(|index| &self.inner.devices[*index])
    }

    pub(crate) fn find_by_bmc_mac(&self, bmc_mac: MacAddress) -> Option<&DeviceSimulator> {
        self.inner
            .by_bmc_mac
            .get(&bmc_mac)
            .map(|index| &self.inner.devices[*index])
    }

    pub fn provisionable_handles(&self) -> Vec<DeviceHandle> {
        self.inner
            .devices
            .iter()
            .filter_map(DeviceSimulator::machine)
            .map(|simulator| simulator.handle().clone())
            .collect()
    }

    pub fn racks_status(&self, status_config: &DeviceStatusConfig) -> RacksStatusResponse {
        RacksStatusResponse {
            racks: self
                .inner
                .racks
                .values()
                .map(|rack| self.status_for_rack(rack, status_config))
                .collect(),
        }
    }

    pub fn rack_status(
        &self,
        rack_id: &RackId,
        status_config: &DeviceStatusConfig,
    ) -> Option<RackStatus> {
        self.inner
            .racks
            .get(rack_id)
            .map(|rack| self.status_for_rack(rack, status_config))
    }

    fn status_for_rack(
        &self,
        rack: &RackInstance,
        status_config: &DeviceStatusConfig,
    ) -> RackStatus {
        RackStatus {
            rack_id: rack.rack_id.to_string(),
            rack_type: rack.rack_type,
            version: rack.version,
            members: rack
                .members
                .iter()
                .map(|member| RackMemberStatus {
                    position: member.placement.position(),
                    device: self.inner.devices[member.device_index].status(status_config),
                })
                .collect(),
        }
    }

    pub fn find_injection_store(&self, id: &str) -> Option<Arc<InjectionStore>> {
        if let Ok(mat_id) = Uuid::parse_str(id) {
            if let Some(device) = self.get(mat_id) {
                return Some(device.bmc_injection_store());
            }
            for device in self.devices() {
                let Some(machine) = device.machine() else {
                    continue;
                };
                if let Some(dpu) = machine
                    .handle()
                    .dpus()
                    .iter()
                    .find(|dpu| dpu.mat_id() == mat_id)
                {
                    return Some(dpu.bmc_injection_store());
                }
            }
        }

        let machine_id = id.parse::<MachineId>().ok()?;
        for device in self.devices() {
            let Some(machine) = device.machine() else {
                continue;
            };
            if machine.handle().observed_machine_id().as_ref() == Some(&machine_id) {
                return Some(machine.bmc_injection_store());
            }
            if let Some(dpu) = machine
                .handle()
                .dpus()
                .iter()
                .find(|dpu| dpu.observed_machine_id().as_ref() == Some(&machine_id))
            {
                return Some(dpu.bmc_injection_store());
            }
        }
        None
    }
}

impl SimulatorRegistryBuilder {
    pub(crate) fn devices(mut self, devices: Vec<DeviceSimulator>) -> Self {
        self.devices = Some(devices);
        self
    }

    pub(crate) fn racks(mut self, rack_registrations: Vec<RackRegistration>) -> Self {
        self.rack_registrations = rack_registrations;
        self
    }

    pub(crate) fn build(self) -> eyre::Result<SimulatorRegistry> {
        SimulatorRegistry::try_from_builder(self)
    }
}

#[cfg(test)]
mod tests {
    use super::SimulatorRegistry;

    #[test]
    fn builder_requires_devices() {
        let error = SimulatorRegistry::builder().build().unwrap_err();

        assert_eq!(
            error.to_string(),
            "simulator registry devices were not configured"
        );
    }
}
