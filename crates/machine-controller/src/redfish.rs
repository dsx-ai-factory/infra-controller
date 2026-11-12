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

use bmc_backend::BackendBmc;
use bmc_platform::{PlatformError, PowerState, ResetType};
use carbide_uuid::machine::MachineIdSubtypeTrait;
use chrono::{DateTime, Utc};
use libredfish::model::BootProgress;
use libredfish::{Redfish, RedfishError};
use model::machine::{Machine, MachineLastRebootRequestedMode};
use state_controller::state_handler::StateHandlerContext;

use crate::context::MachineStateHandlerContextObjects;
use crate::write_ops::MachineWriteOp;

/// Reads the power state of `bmc`'s system, `None` when it reports a state
/// NICo does not classify.
pub async fn power_state(bmc: &BackendBmc) -> Result<Option<PowerState>, PlatformError> {
    bmc.power()?.state().await
}

/// Requests `reset_type` from `bmc`'s system.
pub async fn set_power(bmc: &BackendBmc, reset_type: ResetType) -> Result<(), PlatformError> {
    bmc.power()?.set(reset_type).await.map(drop)
}

/// Names `state` for logs and wait reasons, `Unknown` when unclassified.
pub fn power_state_name(state: Option<PowerState>) -> String {
    state.map_or_else(|| "Unknown".to_string(), |state| format!("{state:?}"))
}

/// The reboot mode recorded for a requested reset.
pub(crate) fn reboot_requested_mode(reset_type: ResetType) -> MachineLastRebootRequestedMode {
    match reset_type {
        ResetType::On | ResetType::ForceOn => MachineLastRebootRequestedMode::PowerOn,
        ResetType::ForceOff | ResetType::GracefulShutdown => {
            MachineLastRebootRequestedMode::PowerOff
        }
        // NICo requests only the resets above and these restarts.
        _ => MachineLastRebootRequestedMode::Reboot,
    }
}

/// Control host power and queue request tracking through the shared Redfish path.
#[track_caller]
pub fn host_power_control(
    bmc: &BackendBmc,
    machine: &Machine<impl MachineIdSubtypeTrait>,
    action: ResetType,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> impl Future<Output = Result<(), PlatformError>> {
    let trigger_location = std::panic::Location::caller();
    host_power_control_with_location(bmc, machine, action, ctx, trigger_location, None)
}

/// Advance lockdown recovery only after the BMC accepts the required restart.
/// An unreadable, powered-off, or transitioning host stays at its retry boundary.
#[track_caller]
pub(crate) fn restart_host_for_lockdown(
    bmc: &BackendBmc,
    machine: &Machine<impl MachineIdSubtypeTrait>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> impl Future<Output = Result<bool, PlatformError>> {
    let trigger_location = std::panic::Location::caller();
    async move {
        let power = bmc.power()?;
        if power.state().await? != Some(PowerState::On) {
            return Ok(false);
        }

        let action = ResetType::ForceRestart;
        let requested_at = Utc::now();
        tracing::info!(
            machine_id = machine.id.to_string(),
            ?action,
            trigger_location = %trigger_location,
            "Host Power Control"
        );
        power.set(action).await?;
        ctx.pending_db_writes
            .push(MachineWriteOp::UpdateRebootRequestedTime {
                machine_id: machine.id.into(),
                mode: reboot_requested_mode(action),
                time: requested_at,
            });
        Ok(true)
    }
}

/// Control host power and queue its accepted request. `requested_at` lets a
/// durable workflow use its database-clock intent; other callers use Core time.
pub async fn host_power_control_with_location(
    bmc: &BackendBmc,
    machine: &Machine<impl MachineIdSubtypeTrait>,
    action: ResetType,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    trigger_location: &std::panic::Location<'_>,
    requested_at: Option<DateTime<Utc>>,
) -> Result<(), PlatformError> {
    let power = bmc.power()?;
    let action = if action == ResetType::FullPowerCycle && !power.ac_power_cycle_supported().await?
    {
        // Not supported here, so just turn off
        ResetType::ForceOff
    } else {
        action
    };
    // Always log to ensure we can see that carbide is doing the power controlling
    tracing::info!(
        machine_id = machine.id.to_string(),
        ?action,
        trigger_location = %trigger_location,
        "Host Power Control"
    );
    let requested_at = requested_at.unwrap_or_else(Utc::now);
    // Record restart and power-on requests only after the BMC accepts them.
    let record_after_success = matches!(
        action,
        ResetType::ForceRestart | ResetType::GracefulRestart | ResetType::On
    );
    if !record_after_success {
        ctx.pending_db_writes
            .push(MachineWriteOp::UpdateRebootRequestedTime {
                machine_id: machine.id.into(),
                mode: reboot_requested_mode(action),
                time: requested_at,
            });
    }

    if (action == ResetType::GracefulRestart) || (action == ResetType::ForceRestart) {
        let power_state = power.state().await?;
        tracing::info!(
            machine_id = machine.id.to_string(),
            ?power_state,
            "Host Power State"
        );
        if power_state == Some(PowerState::Off) {
            tracing::info!(
                machine_id = machine.id.to_string(),
                action = "Manual intervention required to initiate power-on".to_string(),
                "Host Power Action"
            );
            return Ok(());
        } else {
            power.set(action).await?;
        }
    } else if action == ResetType::FullPowerCycle {
        let power_state = power.state().await?;
        if power_state != Some(PowerState::Off) {
            tracing::warn!(
                machine_id = machine.id.to_string(),
                ?power_state,
                "ACPowercycle requires chassis to be Off, forcing off first"
            );
            power.set(ResetType::ForceOff).await?;
        }
        match power.set(action).await {
            // The BMC has no AC power cycle action. Treat it like a BMC without
            // AC power cycle support: the host stays off.
            Err(PlatformError::Unsupported) => {
                tracing::warn!(
                    machine_id = machine.id.to_string(),
                    "ACPowercycle is not supported, leaving host powered off"
                );
                ctx.pending_db_writes
                    .push(MachineWriteOp::UpdateRebootRequestedTime {
                        machine_id: machine.id.into(),
                        mode: reboot_requested_mode(ResetType::ForceOff),
                        time: requested_at,
                    });
            }
            result => {
                result?;
            }
        }
    } else {
        power.set(action).await?;
    }

    if record_after_success {
        ctx.pending_db_writes
            .push(MachineWriteOp::UpdateRebootRequestedTime {
                machine_id: machine.id.into(),
                mode: reboot_requested_mode(action),
                time: requested_at,
            });
    }

    Ok(())
}

const LAST_OEM_STATE_OS_IS_RUNNING: &str = "OsIsRunning";

// did_dpu_finish_booting returns true if the DPU has come up from the last reboot and the OS is running. It will return false if the DPU has not come up from the last reboot or is stuck booting.
// the function will return the BootProgress structure to the caller if it returns true.
pub async fn did_dpu_finish_booting(
    dpu_redfish_client: &dyn Redfish,
) -> Result<(bool, Option<BootProgress>), RedfishError> {
    let system = dpu_redfish_client.get_system().await?;
    match system.boot_progress.clone() {
        Some(boot_progress) => {
            let is_dpu_up = match boot_progress
                .last_state
                .unwrap_or(libredfish::model::BootProgressTypes::None)
            {
                libredfish::model::BootProgressTypes::OSRunning => true,
                _ => {
                    boot_progress.oem_last_state.unwrap_or_default() == LAST_OEM_STATE_OS_IS_RUNNING
                }
            };

            Ok((is_dpu_up, system.boot_progress))
        }
        None => Ok((false, None)),
    }
}
