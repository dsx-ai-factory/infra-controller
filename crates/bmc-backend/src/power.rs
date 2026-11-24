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

use bmc_drivers::CatalogError;
use bmc_platform::{BootProgress, BootProgressTypes, DriverOutcome, PlatformError, Power};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::model::{
    BootProgress as LibredfishBootProgress, BootProgressTypes as LibredfishBootProgressTypes,
};
use libredfish::{Redfish, SystemPowerControl};
use nv_redfish::resource::{PowerState, ResetType};

use crate::BackendBmc;
use crate::convert::platform_error;

/// The OEM boot state of a BlueField whose Arm OS is running.
const BLUEFIELD_OEM_OS_IS_RUNNING: &str = "OsIsRunning";

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::power`].
    pub fn power(&self) -> Result<BackendPower<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendPower::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.power().map(BackendPower::Drivers),
        }
    }
}

/// The power operations of a [`BackendBmc`].
pub enum BackendPower<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn Power<RedfishBmc>, RedfishBmc>),
}

impl BackendPower<'_> {
    /// See [`Power::state`].
    pub async fn state(&self) -> Result<Option<PowerState>, PlatformError> {
        match self {
            Self::Libredfish(client) => client
                .get_power_state()
                .await
                .map(power_state)
                .map_err(platform_error),
            Self::Drivers(power) => power.state().await,
        }
    }

    /// See [`Power::boot_progress`]. libredfish has no drivers, so a BlueField's
    /// OEM running state is reported as `OsRunning` here for every platform.
    pub async fn boot_progress(&self) -> Result<Option<BootProgress>, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let system = client.get_system().await.map_err(platform_error)?;
                Ok(system.boot_progress.map(boot_progress))
            }
            Self::Drivers(power) => power.boot_progress().await,
        }
    }

    /// See [`Power::ac_power_cycle_supported`].
    pub async fn ac_power_cycle_supported(&self) -> Result<bool, PlatformError> {
        match self {
            Self::Libredfish(client) => Ok(client.ac_powercycle_supported_by_power()),
            Self::Drivers(power) => power.ac_power_cycle_supported().await,
        }
    }

    /// See [`Power::set`].
    pub async fn set(&self, reset_type: ResetType) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let action = system_power_control(reset_type).ok_or(PlatformError::Unsupported)?;
                client.power(action).await.map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(power) => power.set(reset_type).await,
        }
    }

    /// See [`Power::chassis_reset`].
    pub async fn chassis_reset(
        &self,
        chassis_id: &str,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let action = system_power_control(reset_type).ok_or(PlatformError::Unsupported)?;
                client
                    .chassis_reset(chassis_id, action)
                    .await
                    .map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(power) => power.chassis_reset(chassis_id, reset_type).await,
        }
    }
}

/// `Unknown` has no platform equivalent. `Reset` is a restart in progress, so
/// callers wait it out as they do `PoweringOn`.
fn power_state(state: libredfish::PowerState) -> Option<PowerState> {
    match state {
        libredfish::PowerState::Off => Some(PowerState::Off),
        libredfish::PowerState::On => Some(PowerState::On),
        libredfish::PowerState::PoweringOff => Some(PowerState::PoweringOff),
        libredfish::PowerState::PoweringOn => Some(PowerState::PoweringOn),
        libredfish::PowerState::Paused => Some(PowerState::Paused),
        libredfish::PowerState::Reset => Some(PowerState::PoweringOn),
        libredfish::PowerState::Unknown => None,
    }
}

fn boot_progress(progress: LibredfishBootProgress) -> BootProgress {
    let last_state = if progress.oem_last_state.as_deref() == Some(BLUEFIELD_OEM_OS_IS_RUNNING) {
        Some(BootProgressTypes::OsRunning)
    } else {
        progress.last_state.map(boot_progress_type)
    };
    BootProgress {
        last_state,
        oem_last_state: progress.oem_last_state,
    }
}

fn boot_progress_type(state: LibredfishBootProgressTypes) -> BootProgressTypes {
    match state {
        LibredfishBootProgressTypes::None => BootProgressTypes::None,
        LibredfishBootProgressTypes::PrimaryProcessorInitializationStarted => {
            BootProgressTypes::PrimaryProcessorInitializationStarted
        }
        LibredfishBootProgressTypes::BusInitializationStarted => {
            BootProgressTypes::BusInitializationStarted
        }
        LibredfishBootProgressTypes::MemoryInitializationStarted => {
            BootProgressTypes::MemoryInitializationStarted
        }
        LibredfishBootProgressTypes::SecondaryProcessorInitializationStarted => {
            BootProgressTypes::SecondaryProcessorInitializationStarted
        }
        LibredfishBootProgressTypes::PCIResourceConfigStarted => {
            BootProgressTypes::PciResourceConfigStarted
        }
        LibredfishBootProgressTypes::SystemHardwareInitializationComplete => {
            BootProgressTypes::SystemHardwareInitializationComplete
        }
        LibredfishBootProgressTypes::SetupEntered => BootProgressTypes::SetupEntered,
        LibredfishBootProgressTypes::OSBootStarted => BootProgressTypes::OsBootStarted,
        LibredfishBootProgressTypes::OSRunning => BootProgressTypes::OsRunning,
        LibredfishBootProgressTypes::OEM => BootProgressTypes::Oem,
    }
}

fn system_power_control(reset_type: ResetType) -> Option<SystemPowerControl> {
    match reset_type {
        ResetType::On => Some(SystemPowerControl::On),
        ResetType::ForceOff => Some(SystemPowerControl::ForceOff),
        ResetType::GracefulShutdown => Some(SystemPowerControl::GracefulShutdown),
        ResetType::GracefulRestart => Some(SystemPowerControl::GracefulRestart),
        ResetType::ForceRestart => Some(SystemPowerControl::ForceRestart),
        ResetType::PowerCycle => Some(SystemPowerControl::PowerCycle),
        ResetType::FullPowerCycle => Some(SystemPowerControl::ACPowercycle),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn libredfish_power_states_map_to_what_callers_act_on() {
        value_scenarios!(power_state:
            "states callers act on" {
                libredfish::PowerState::Off => Some(PowerState::Off),
                libredfish::PowerState::On => Some(PowerState::On),
                libredfish::PowerState::PoweringOff => Some(PowerState::PoweringOff),
                libredfish::PowerState::PoweringOn => Some(PowerState::PoweringOn),
                libredfish::PowerState::Paused => Some(PowerState::Paused),
            }
            "states callers wait out" {
                libredfish::PowerState::Reset => Some(PowerState::PoweringOn),
                libredfish::PowerState::Unknown => None,
            }
        );
    }

    #[test]
    fn libredfish_boot_stages_keep_their_redfish_names() {
        value_scenarios!(boot_progress_type:
            "renamed in nv-redfish" {
                LibredfishBootProgressTypes::PCIResourceConfigStarted
                    => BootProgressTypes::PciResourceConfigStarted,
                LibredfishBootProgressTypes::OSRunning => BootProgressTypes::OsRunning,
                LibredfishBootProgressTypes::OEM => BootProgressTypes::Oem,
            }
        );
    }

    #[test]
    fn a_bluefield_oem_running_state_reports_as_os_running() {
        let progress = |last_state, oem_last_state: Option<&str>| LibredfishBootProgress {
            last_state,
            last_state_time: None,
            oem_last_state: oem_last_state.map(str::to_string),
        };
        value_scenarios!(run = |progress: LibredfishBootProgress| boot_progress(progress).last_state;
            "the OS runs" {
                progress(Some(LibredfishBootProgressTypes::OEM), Some("OsIsRunning"))
                    => Some(BootProgressTypes::OsRunning),
            }
            "reported as is" {
                progress(Some(LibredfishBootProgressTypes::OEM), Some("UEFI"))
                    => Some(BootProgressTypes::Oem),
                progress(None, None) => None,
            }
        );
    }

    #[test]
    fn reset_types_map_to_the_libredfish_actions_callers_request() {
        value_scenarios!(system_power_control:
            "supported" {
                ResetType::On => Some(SystemPowerControl::On),
                ResetType::ForceOff => Some(SystemPowerControl::ForceOff),
                ResetType::GracefulShutdown => Some(SystemPowerControl::GracefulShutdown),
                ResetType::GracefulRestart => Some(SystemPowerControl::GracefulRestart),
                ResetType::ForceRestart => Some(SystemPowerControl::ForceRestart),
                ResetType::PowerCycle => Some(SystemPowerControl::PowerCycle),
                ResetType::FullPowerCycle => Some(SystemPowerControl::ACPowercycle),
            }
            "unsupported" {
                ResetType::ForceOn => None,
                ResetType::Nmi => None,
            }
        );
    }
}
