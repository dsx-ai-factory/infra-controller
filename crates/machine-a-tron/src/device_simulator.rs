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
use std::sync::Arc;

use bmc_mock::injection::InjectionStore;
use uuid::Uuid;

use crate::PersistedDevice;
use crate::api_client::ApiClient;
use crate::device_handle::DeviceHandle;
use crate::host_machine::MachineHandle;
use crate::power_shelf_simulator::PowerShelfHandle;
use crate::status::{DeviceKind, DeviceStatus, DeviceStatusConfig};
use crate::switch_simulator::SwitchHandle;

/// The common lifecycle exposed by every simulated physical device.
pub trait SimulatorLifecycle {
    fn handle(&self) -> &DeviceHandle;
    fn kind(&self) -> DeviceKind;

    fn mat_id(&self) -> Uuid {
        self.handle().mat_id()
    }

    fn resume(&self) -> eyre::Result<()> {
        self.handle().resume()
    }

    fn abort(&self) {
        self.handle().abort();
    }

    fn persisted(&self) -> PersistedDevice {
        self.handle().persisted()
    }

    fn status(&self, config: &DeviceStatusConfig) -> DeviceStatus {
        self.handle().status(config)
    }

    fn bmc_injection_store(&self) -> Arc<InjectionStore> {
        self.handle().bmc_injection_store()
    }
}

#[derive(Debug, Clone)]
pub struct MachineSimulator(DeviceHandle);

impl MachineSimulator {
    pub(crate) fn new(handle: MachineHandle) -> Self {
        Self(DeviceHandle::machine(handle))
    }
}

impl SimulatorLifecycle for MachineSimulator {
    fn handle(&self) -> &DeviceHandle {
        &self.0
    }

    fn kind(&self) -> DeviceKind {
        DeviceKind::Machine
    }
}

#[derive(Debug, Clone)]
pub struct SwitchSimulator(DeviceHandle);

impl SwitchSimulator {
    pub(crate) fn new(handle: SwitchHandle) -> Self {
        Self(DeviceHandle::switch(handle))
    }
}

impl SimulatorLifecycle for SwitchSimulator {
    fn handle(&self) -> &DeviceHandle {
        &self.0
    }

    fn kind(&self) -> DeviceKind {
        DeviceKind::Switch
    }
}

#[derive(Debug, Clone)]
pub struct PowerShelfSimulator(DeviceHandle);

impl PowerShelfSimulator {
    pub(crate) fn new(handle: PowerShelfHandle) -> Self {
        Self(DeviceHandle::power_shelf(handle))
    }
}

impl SimulatorLifecycle for PowerShelfSimulator {
    fn handle(&self) -> &DeviceHandle {
        &self.0
    }

    fn kind(&self) -> DeviceKind {
        DeviceKind::PowerShelf
    }
}

#[derive(Debug, Clone)]
pub enum DeviceSimulator {
    Machine(MachineSimulator),
    Switch(SwitchSimulator),
    PowerShelf(PowerShelfSimulator),
}

impl DeviceSimulator {
    pub(crate) fn from_handle(handle: DeviceHandle) -> Self {
        match handle.kind() {
            DeviceKind::Machine => Self::Machine(MachineSimulator(handle)),
            DeviceKind::Switch => Self::Switch(SwitchSimulator(handle)),
            DeviceKind::PowerShelf => Self::PowerShelf(PowerShelfSimulator(handle)),
            DeviceKind::Dpu => unreachable!("a top-level simulator cannot represent a DPU"),
        }
    }

    pub fn machine(&self) -> Option<&MachineSimulator> {
        match self {
            Self::Machine(simulator) => Some(simulator),
            Self::Switch(_) | Self::PowerShelf(_) => None,
        }
    }

    pub async fn delete_from_api(&self, api_client: ApiClient) -> eyre::Result<()> {
        match self {
            Self::Machine(simulator) => {
                simulator.handle().delete_machine_from_api(api_client).await
            }
            Self::Switch(simulator) => {
                let bmc_mac = simulator.handle().host_info().bmc_mac_address.to_string();
                if api_client
                    .force_delete_switch_by_bmc(bmc_mac.clone())
                    .await?
                    .is_none()
                {
                    tracing::info!(
                        bmc_mac_address = %bmc_mac,
                        "Not deleting switch because it has not been discovered"
                    );
                }
                Ok(())
            }
            Self::PowerShelf(simulator) => {
                let bmc_mac = simulator.handle().host_info().bmc_mac_address.to_string();
                if api_client
                    .force_delete_power_shelf_by_bmc(bmc_mac.clone())
                    .await?
                    .is_none()
                {
                    tracing::info!(
                        bmc_mac_address = %bmc_mac,
                        "Not deleting power shelf because it has not been discovered"
                    );
                }
                Ok(())
            }
        }
    }

    pub async fn shutdown(&self) -> eyre::Result<()> {
        self.handle().abort_and_wait().await
    }
}

impl SimulatorLifecycle for DeviceSimulator {
    fn handle(&self) -> &DeviceHandle {
        match self {
            Self::Machine(simulator) => &simulator.0,
            Self::Switch(simulator) => &simulator.0,
            Self::PowerShelf(simulator) => &simulator.0,
        }
    }

    fn kind(&self) -> DeviceKind {
        match self {
            Self::Machine(_) => DeviceKind::Machine,
            Self::Switch(_) => DeviceKind::Switch,
            Self::PowerShelf(_) => DeviceKind::PowerShelf,
        }
    }
}
