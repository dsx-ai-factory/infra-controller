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
use bmc_platform::{DriverOutcome, PlatformError, Power};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::{Redfish, SystemPowerControl};
use nv_redfish::resource::{PowerState, ResetType};

use crate::BackendBmc;
use crate::convert::platform_error;

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
