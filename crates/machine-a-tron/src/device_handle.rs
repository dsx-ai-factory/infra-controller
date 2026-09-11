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
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use bmc_mock::HostMachineInfo;
use bmc_mock::injection::InjectionStore;
use carbide_uuid::machine::MachineId;
use uuid::Uuid;

use crate::api_client::ApiClient;
use crate::dpu_machine::DpuMachineHandle;
use crate::host_machine::MachineHandle;
use crate::power_shelf_simulator::PowerShelfHandle;
use crate::status::{DeviceKind, DeviceStatus, DeviceStatusConfig};
use crate::switch_simulator::SwitchHandle;
use crate::{Guid, InfinibandPortState, PersistedDevice};

#[derive(Debug, Clone)]
enum DeviceHandleInner {
    Machine(MachineHandle),
    Switch(SwitchHandle),
    PowerShelf(PowerShelfHandle),
}

/// Common handle for a running simulated physical-device actor.
#[derive(Debug, Clone)]
pub struct DeviceHandle(DeviceHandleInner);

impl DeviceHandle {
    pub(crate) fn machine(handle: MachineHandle) -> Self {
        Self(DeviceHandleInner::Machine(handle))
    }

    pub(crate) fn switch(handle: SwitchHandle) -> Self {
        Self(DeviceHandleInner::Switch(handle))
    }

    pub(crate) fn power_shelf(handle: PowerShelfHandle) -> Self {
        Self(DeviceHandleInner::PowerShelf(handle))
    }

    pub(crate) fn kind(&self) -> DeviceKind {
        match &self.0 {
            DeviceHandleInner::Machine(_) => DeviceKind::Machine,
            DeviceHandleInner::Switch(_) => DeviceKind::Switch,
            DeviceHandleInner::PowerShelf(_) => DeviceKind::PowerShelf,
        }
    }

    pub fn mat_id(&self) -> Uuid {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.mat_id(),
            DeviceHandleInner::Switch(handle) => handle.mat_id(),
            DeviceHandleInner::PowerShelf(handle) => handle.mat_id(),
        }
    }

    pub fn observed_machine_id(&self) -> Option<MachineId> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.observed_machine_id(),
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => None,
        }
    }

    pub async fn api_state(&self) -> eyre::Result<String> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.api_state().await,
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => {
                eyre::bail!("API machine state is unavailable for {}", self.kind())
            }
        }
    }

    pub async fn wait_until_machine_up_with_api_state(
        &self,
        state: &str,
        timeout: Duration,
    ) -> eyre::Result<()> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => {
                handle
                    .wait_until_machine_up_with_api_state(state, timeout)
                    .await
            }
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => {
                eyre::bail!("cannot wait for machine state on {}", self.kind())
            }
        }
    }

    pub fn pause(&self) -> eyre::Result<()> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.pause(),
            DeviceHandleInner::Switch(handle) => handle.pause(),
            DeviceHandleInner::PowerShelf(handle) => handle.pause(),
        }
    }

    pub fn resume(&self) -> eyre::Result<()> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.resume(),
            DeviceHandleInner::Switch(handle) => handle.resume(),
            DeviceHandleInner::PowerShelf(handle) => handle.resume(),
        }
    }

    pub fn host_info(&self) -> &HostMachineInfo {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.host_info(),
            DeviceHandleInner::Switch(handle) => handle.host_info(),
            DeviceHandleInner::PowerShelf(handle) => handle.host_info(),
        }
    }

    pub fn machine_config_section(&self) -> &str {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.machine_config_section(),
            DeviceHandleInner::Switch(handle) => handle.machine_config_section(),
            DeviceHandleInner::PowerShelf(handle) => handle.machine_config_section(),
        }
    }

    pub fn status(&self, config: &DeviceStatusConfig) -> DeviceStatus {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.status(config),
            DeviceHandleInner::Switch(handle) => handle.status(config),
            DeviceHandleInner::PowerShelf(handle) => handle.status(config),
        }
    }

    pub fn set_infiniband_port_state(
        &self,
        guid: Guid,
        state: InfinibandPortState,
    ) -> eyre::Result<()> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.set_infiniband_port_state(guid, state),
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => {
                eyre::bail!("cannot set InfiniBand port state on {}", self.kind())
            }
        }
    }

    pub fn persisted(&self) -> PersistedDevice {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.persisted(),
            DeviceHandleInner::Switch(handle) => handle.persisted(),
            DeviceHandleInner::PowerShelf(handle) => handle.persisted(),
        }
    }

    pub fn dpus(&self) -> &[DpuMachineHandle] {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.dpus(),
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => &[],
        }
    }

    pub(crate) async fn delete_machine_from_api(&self, api_client: ApiClient) -> eyre::Result<()> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.clone().delete_from_api(api_client).await,
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => {
                eyre::bail!("cannot delete {} through the machine API", self.kind())
            }
        }
    }

    pub(crate) fn bmc_injection_store(&self) -> Arc<InjectionStore> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.bmc_injection_store(),
            DeviceHandleInner::Switch(handle) => handle.bmc_injection_store(),
            DeviceHandleInner::PowerShelf(handle) => handle.bmc_injection_store(),
        }
    }

    pub fn abort(&self) {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.abort(),
            DeviceHandleInner::Switch(handle) => handle.abort(),
            DeviceHandleInner::PowerShelf(handle) => handle.abort(),
        }
    }

    pub async fn abort_and_wait(&self) -> eyre::Result<()> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.abort_and_wait().await,
            DeviceHandleInner::Switch(handle) => handle.abort_and_wait().await,
            DeviceHandleInner::PowerShelf(handle) => handle.abort_and_wait().await,
        }
    }

    pub fn bmc_ssh_host_pubkey(&self) -> Option<String> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.bmc_ssh_host_pubkey(),
            DeviceHandleInner::Switch(handle) => handle.bmc_ssh_host_pubkey(),
            DeviceHandleInner::PowerShelf(handle) => handle.bmc_ssh_host_pubkey(),
        }
    }

    pub fn bmc_ip(&self) -> Option<Ipv4Addr> {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.bmc_ip(),
            DeviceHandleInner::Switch(handle) => handle.bmc_ip(),
            DeviceHandleInner::PowerShelf(handle) => handle.bmc_ip(),
        }
    }

    /// The address of the device's host-side endpoint: a switch's NVOS
    /// management address once it has one. Hosts and power shelves have no
    /// host endpoint that RMS addresses.
    pub fn host_ip(&self) -> Option<Ipv4Addr> {
        match &self.0 {
            DeviceHandleInner::Switch(handle) => handle.nvos_ip(),
            DeviceHandleInner::Machine(_) | DeviceHandleInner::PowerShelf(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_control_test(dpus: Vec<DpuMachineHandle>, ipmi_port: Option<u16>) -> Self {
        Self::machine(MachineHandle::for_control_test(dpus, ipmi_port))
    }

    #[cfg(test)]
    pub(crate) fn with_control_test_ssh_endpoint(self, port: u16) -> Self {
        match self.0 {
            DeviceHandleInner::Machine(handle) => {
                Self::machine(handle.with_control_test_ssh_endpoint(port))
            }
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => {
                unreachable!("control-test SSH endpoint is only configured for machines")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_control_test_bmc_ip(&self, ip: Option<Ipv4Addr>) {
        match &self.0 {
            DeviceHandleInner::Machine(handle) => handle.set_control_test_bmc_ip(ip),
            DeviceHandleInner::Switch(_) | DeviceHandleInner::PowerShelf(_) => {
                unreachable!("control-test BMC addresses are only set on machines")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_control_test_in_section(machine_config_section: &str) -> Self {
        Self::machine(MachineHandle::for_control_test_in_section(
            Vec::new(),
            None,
            machine_config_section,
        ))
    }
}
