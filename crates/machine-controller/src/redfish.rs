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

use bmc_backend::{BackendBmc, idrac_job};
use bmc_platform::{
    BootProgress, BootProgressTypes, OperationStatus, PlatformError, PowerState, ResetType,
    VendorJobId,
};
use carbide_redfish::state_handler::state_handler_platform_error as platform_error;
use carbide_uuid::machine::MachineIdSubtypeTrait;
use chrono::{DateTime, Utc};
use model::machine::{Machine, MachineLastRebootRequestedMode};
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};
use crate::write_ops::MachineWriteOp;

/// `PowerControlError` distinguishes preparation failures from possibly applied power changes.
/// A queued caller can restore its earlier attempt only when no mutation was attempted.
#[derive(Debug)]
pub(crate) enum PowerControlError<E> {
    NotSent(E),
    Uncertain(E),
}

impl<E> PowerControlError<E> {
    pub(crate) fn into_inner(self) -> E {
        match self {
            Self::NotSent(error) | Self::Uncertain(error) => error,
        }
    }

    pub(crate) fn map<F>(self, map: impl FnOnce(E) -> F) -> PowerControlError<F> {
        match self {
            Self::NotSent(error) => PowerControlError::NotSent(map(error)),
            Self::Uncertain(error) => PowerControlError::Uncertain(map(error)),
        }
    }
}

/// Reads the power state of `bmc`'s system, `None` when it reports a state
/// NICo does not classify.
pub async fn power_state(bmc: &BackendBmc) -> Result<Option<PowerState>, PlatformError> {
    bmc.power()?.state().await
}

/// Requests `reset_type` from `bmc`'s system.
pub async fn set_power(bmc: &BackendBmc, reset_type: ResetType) -> Result<(), PlatformError> {
    bmc.power()?.set(reset_type).await.map(drop)
}

/// Restarts the BMC itself.
pub async fn reset_bmc(bmc: &BackendBmc) -> Result<(), PlatformError> {
    bmc.bmc_control()?.reset().await.map(drop)
}

/// Resets the BMC to its factory defaults.
pub async fn reset_bmc_to_factory_defaults(bmc: &BackendBmc) -> Result<(), PlatformError> {
    bmc.bmc_control()?
        .reset_to_factory_defaults()
        .await
        .map(drop)
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
    async move {
        host_power_control_with_location(bmc, machine, action, ctx, trigger_location, None)
            .await
            .map(drop)
            .map_err(PowerControlError::into_inner)
    }
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
/// Returns false when a restart was skipped because the host was powered off.
pub(crate) async fn host_power_control_with_location(
    bmc: &BackendBmc,
    machine: &Machine<impl MachineIdSubtypeTrait>,
    action: ResetType,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    trigger_location: &std::panic::Location<'_>,
    requested_at: Option<DateTime<Utc>>,
) -> Result<bool, PowerControlError<PlatformError>> {
    let power = bmc
        .power()
        .map_err(|error| PowerControlError::NotSent(error.into()))?;
    let action = if action == ResetType::FullPowerCycle
        && !power
            .ac_power_cycle_supported()
            .await
            .map_err(PowerControlError::NotSent)?
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
        let power_state = power.state().await.map_err(PowerControlError::NotSent)?;
        tracing::info!(
            machine_id = machine.id.to_string(),
            ?power_state,
            "Host Power State"
        );
        if power_state == Some(PowerState::Off) {
            tracing::info!(
                machine_id = machine.id.to_string(),
                action = "Manual intervention required to initiate power-on",
                "Host Power Action"
            );
            return Ok(false);
        } else {
            power.set(action).await.map_err(|error| match error {
                PlatformError::NotSent { source } => PowerControlError::NotSent(*source),
                error => PowerControlError::Uncertain(error),
            })?;
        }
    } else if action == ResetType::FullPowerCycle {
        let power_state = power.state().await.map_err(PowerControlError::NotSent)?;
        if power_state != Some(PowerState::Off) {
            tracing::warn!(
                machine_id = machine.id.to_string(),
                ?power_state,
                "ACPowercycle requires chassis to be Off, forcing off first"
            );
            power
                .set(ResetType::ForceOff)
                .await
                .map_err(PowerControlError::Uncertain)?;
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
                result.map_err(PowerControlError::Uncertain)?;
            }
        }
    } else {
        power
            .set(action)
            .await
            .map_err(PowerControlError::Uncertain)?;
    }

    if record_after_success {
        ctx.pending_db_writes
            .push(MachineWriteOp::UpdateRebootRequestedTime {
                machine_id: machine.id.into(),
                mode: reboot_requested_mode(action),
                time: requested_at,
            });
    }

    Ok(true)
}

// did_dpu_finish_booting returns true if the DPU has come up from the last reboot and the OS is running. It will return false if the DPU has not come up from the last reboot or is stuck booting.
// the function will return the BootProgress structure to the caller if it returns true.
pub async fn did_dpu_finish_booting(
    bmc: &BackendBmc,
) -> Result<(bool, Option<BootProgress>), PlatformError> {
    // Boxed so callers' futures stay within the query depth limit.
    let boot_progress = Box::pin(async { bmc.power()?.boot_progress().await }).await?;
    let os_running = boot_progress
        .as_ref()
        .is_some_and(|progress| progress.last_state == Some(BootProgressTypes::OsRunning));
    Ok((os_running, boot_progress))
}

/// Connects to `machine`'s BMC and polls the iDRAC vendor job `job_id`.
pub(crate) async fn poll_idrac_job(
    services: &MachineStateHandlerServices,
    machine: &Machine<impl MachineIdSubtypeTrait>,
    job_id: &str,
) -> Result<OperationStatus, StateHandlerError> {
    let bmc = services.connect_bmc(machine).await?;
    let vendor_job_id = VendorJobId::new(job_id.to_string())
        .map_err(|error| StateHandlerError::GenericError(eyre::eyre!("invalid job id: {error}")))?;
    bmc.operation_status(&idrac_job(vendor_job_id))
        .await
        .map_err(|error| platform_error("operation_status", error))
}
