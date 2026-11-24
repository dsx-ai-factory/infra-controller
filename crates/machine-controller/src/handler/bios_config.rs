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

//! BIOS configuration: applying the platform settings, waiting for the BMC
//! work they start, and PollingBiosSetup escalation.

use bmc_backend::idrac_job;
use bmc_platform::{OperationStatus, PlatformError, PowerState, ResetType, VendorJobId};
use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::state_handler::state_handler_platform_error as platform_error;
use chrono::Utc;
use eyre::eyre;
use libredfish::Redfish;
use model::machine::{
    BiosConfigInfo, BiosConfigState, ManagedHostState, ManagedHostStateSnapshot,
    PowerState as ExpectedPowerState,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{
    ReachabilityParams, RebootStatus, handler_host_power_control, trigger_reboot_if_needed,
};
use crate::config::MachineStateControllerConfig;
use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};
use crate::platform_config::{self, PlatformBmc};
use crate::redfish::{power_state_name, reset_bmc};

/// Outcome of configure_host_bios function.
pub(super) enum BiosConfigOutcome {
    WaitingForBmc(String),
    WaitingForReboot(String),
    Applied(BiosConfigInfo),
}

/// Outcome of advancing the BIOS config job state machine (Dell: wait for BIOS PATCH job before boot order).
pub(super) enum BiosConfigJobAdvanceOutcome {
    Continue(BiosConfigInfo),
    /// Dell BIOS job completed; proceed to verify settings via PollingBiosSetup.
    Done,
    Failed {
        failure: String,
    },
    /// Same state, but wait (e.g. waiting for power down or BMC to come back).
    Wait(String),
    /// After successful power/BMC recovery from a failed BIOS job: re-run machine_setup (not PollingBiosSetup).
    RetryPlatformConfiguration {
        retry_count: u32,
    },
}

#[derive(Debug)]
pub(super) enum PollingBiosSetupOutcome {
    Verified,
    Wait(String),
    EnterRecovery(BiosConfigInfo),
    Failed { failure: String },
}

/// Outcome of entering HandleBiosJobFailure recovery, or failing once the budget is exhausted.
enum BiosRecoveryAttemptOutcome {
    Continue(BiosConfigInfo),
    Failed { failure: String },
}

impl From<BiosRecoveryAttemptOutcome> for BiosConfigJobAdvanceOutcome {
    fn from(outcome: BiosRecoveryAttemptOutcome) -> Self {
        match outcome {
            BiosRecoveryAttemptOutcome::Continue(info) => Self::Continue(info),
            BiosRecoveryAttemptOutcome::Failed { failure } => Self::Failed { failure },
        }
    }
}

const SERVICE_UNAVAILABLE: u16 = 503;

fn bmc_unavailable(error: &PlatformError) -> bool {
    matches!(
        error,
        PlatformError::Unreachable
            | PlatformError::Bmc {
                status: SERVICE_UNAVAILABLE,
                ..
            }
    )
}

pub(super) async fn configure_host_bios(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    boot_interface: Option<&BootInterfaceTarget>,
    retry_count: u32,
) -> Result<BiosConfigOutcome, StateHandlerError> {
    let bmc = PlatformBmc::connect(ctx.services, mh_snapshot, redfish_client).await?;
    let applied = platform_config::apply(&bmc, &ctx.services.site_config, boot_interface).await;
    let outcome = match applied {
        Err(e) if bmc_unavailable(&e) => {
            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                error = %e,
                "applying the platform settings failed because the BMC did not answer. retrying without restarting the host"
            );
            return Ok(BiosConfigOutcome::WaitingForBmc(format!(
                "applying the platform settings failed: {e}; waiting for the BMC to answer"
            )));
        }
        Err(e) => {
            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                error = %e,
                "applying the platform settings failed, potentially due to known race condition between UEFI POST and BMC. triggering force-restart if needed"
            );

            // if machine_setup failed, reboot to potentially work around
            // a known race between the DPU UEFI and the BMC, where if
            // the BMC is not up when DPU UEFI runs, then Attributes might
            // not come through. The fix is to force-restart the DPU to
            // re-POST.
            //
            // As of July 2024, Josh Price said there's an NBU FR to fix
            // this, but it wasn't target to a release yet.
            let reboot_status = if mh_snapshot
                .host_snapshot
                .status
                .last_reboot_requested
                .is_none()
            {
                handler_host_power_control(mh_snapshot, ctx, ResetType::ForceRestart).await?;

                RebootStatus {
                    increase_retry_count: true,
                    status: "Restarted host".to_string(),
                }
            } else {
                trigger_reboot_if_needed(
                    &mh_snapshot.host_snapshot,
                    mh_snapshot,
                    None,
                    reachability_params,
                    ctx,
                )
                .await?
            };
            return Ok(BiosConfigOutcome::WaitingForReboot(format!(
                "applying the platform settings failed: {e}; triggered host reboot: {reboot_status:#?}"
            )));
        }
        Ok(outcome) => outcome,
    };

    let bios_job_id = platform_config::vendor_job_id(&outcome).map(|job_id| job_id.to_string());
    let bios_config_state = if bios_job_id.is_some() {
        BiosConfigState::WaitForBiosJobScheduled
    } else {
        BiosConfigState::RebootHost
    };
    Ok(BiosConfigOutcome::Applied(BiosConfigInfo {
        bios_job_id,
        bios_config_state,
        retry_count,
    }))
}

/// Whether the platform settings are in effect on `mh_snapshot`'s host.
pub(super) async fn platform_settings_in_effect(
    services: &MachineStateHandlerServices,
    bmc: &PlatformBmc<'_>,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<bool, StateHandlerError> {
    platform_config::in_effect(bmc, &services.site_config, boot_interface)
        .await
        .map_err(|e| platform_error("is_bios_setup", e))
}

/// Advance one step of the BIOS config job wait state machine.
pub(super) async fn advance_bios_config_job(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    info: BiosConfigInfo,
) -> Result<BiosConfigJobAdvanceOutcome, StateHandlerError> {
    let machine_controller_config = &ctx.services.site_config.machine_state_controller;
    match info.bios_config_state {
        BiosConfigState::WaitForBiosJobScheduled => {
            let (job_id, job_status) =
                read_bios_job(ctx.services, redfish_client, mh_snapshot, &info).await?;
            let job_status = job_status.map_err(|e| platform_error("get_job_state", e))?;
            match job_status {
                OperationStatus::Failed { .. } | OperationStatus::NeedsIntervention { .. } => {
                    let failure = format!("BIOS job {job_id} failed with state {job_status:?}");
                    tracing::warn!(
                        reason = %failure,
                        "transitioning to HandleBiosJobFailure (power cycle + BMC reset)"
                    );
                    Ok(try_bios_recovery_attempt(
                        machine_controller_config,
                        info.retry_count,
                        info.bios_job_id,
                        failure,
                    )?
                    .into())
                }
                OperationStatus::Completed => {
                    tracing::info!(
                        %job_id,
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "BIOS job completed before scheduling was observed; skipping reboot",
                    );
                    Ok(BiosConfigJobAdvanceOutcome::Done)
                }
                OperationStatus::AwaitingReset => {
                    Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                        bios_job_id: info.bios_job_id,
                        bios_config_state: BiosConfigState::RebootHost,
                        retry_count: info.retry_count,
                    }))
                }
                OperationStatus::Running => Err(StateHandlerError::GenericError(eyre!(
                    "waiting for BIOS job {job_id} to be scheduled; current state: {job_status:?}"
                ))),
            }
        }
        BiosConfigState::RebootHost => {
            handler_host_power_control(mh_snapshot, ctx, ResetType::ForceRestart).await?;
            if info.bios_job_id.is_none() {
                return Ok(BiosConfigJobAdvanceOutcome::Done);
            }
            Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                bios_job_id: info.bios_job_id,
                bios_config_state: BiosConfigState::WaitForBiosJobCompletion,
                retry_count: info.retry_count,
            }))
        }
        BiosConfigState::WaitForBiosJobCompletion => {
            const JOB_QUERY_WAIT_MINUTES: i64 = 5;
            let (job_id, job_status) =
                read_bios_job(ctx.services, redfish_client, mh_snapshot, &info).await?;
            let job_status = match job_status {
                Ok(status) => status,
                Err(e) => {
                    let minutes_since_state_change = mh_snapshot
                        .host_snapshot
                        .state
                        .version
                        .since_state_change()
                        .num_minutes();
                    if minutes_since_state_change < JOB_QUERY_WAIT_MINUTES {
                        return Err(platform_error("get_job_state", e));
                    }
                    let failure = format!(
                        "BIOS config job {job_id} lookup failed after {minutes_since_state_change} min: {e}"
                    );
                    tracing::warn!(
                        reason = %failure,
                        "transitioning to HandleBiosJobFailure (power cycle + BMC reset)"
                    );
                    return Ok(try_bios_recovery_attempt(
                        machine_controller_config,
                        info.retry_count,
                        info.bios_job_id,
                        failure,
                    )?
                    .into());
                }
            };
            match job_status {
                OperationStatus::Completed => Ok(BiosConfigJobAdvanceOutcome::Done),
                OperationStatus::Failed { .. } | OperationStatus::NeedsIntervention { .. } => {
                    let failure =
                        format!("BIOS config job {job_id} failed with state {job_status:?}");
                    tracing::warn!(
                        reason = %failure,
                        "transitioning to HandleBiosJobFailure (power cycle + BMC reset)"
                    );
                    Ok(try_bios_recovery_attempt(
                        machine_controller_config,
                        info.retry_count,
                        info.bios_job_id,
                        failure,
                    )?
                    .into())
                }
                OperationStatus::Running | OperationStatus::AwaitingReset => {
                    Err(StateHandlerError::GenericError(eyre!(
                        "waiting for BIOS job {job_id} to complete; current state: {job_status:?}"
                    )))
                }
            }
        }
        BiosConfigState::HandleBiosJobFailure {
            failure,
            power_state,
        } => {
            let bmc = ctx.services.connect_bmc(&mh_snapshot.host_snapshot).await?;
            let current_power_state = crate::redfish::power_state(&bmc)
                .await
                .map_err(|e| platform_error("get_power_state", e))?;

            match power_state {
                ExpectedPowerState::Off => {
                    if current_power_state != Some(PowerState::Off) {
                        handler_host_power_control(mh_snapshot, ctx, ResetType::ForceOff).await?;
                        return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                            "HandleBiosJobFailure: waiting for power down; current power state: {current_power_state}; failure: {failure}",
                            current_power_state = power_state_name(current_power_state)
                        )));
                    }
                    tracing::info!(
                        reason = %failure,
                        "HandleBiosJobFailure: resetting BMC after BIOS job failure"
                    );
                    reset_bmc(&bmc)
                        .await
                        .map_err(|e| platform_error("bmc_reset", e))?;
                    Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                        bios_job_id: info.bios_job_id,
                        bios_config_state: BiosConfigState::HandleBiosJobFailure {
                            failure,
                            power_state: ExpectedPowerState::On,
                        },
                        retry_count: info.retry_count,
                    }))
                }
                ExpectedPowerState::On => {
                    if current_power_state != Some(PowerState::On) {
                        let basetime = mh_snapshot
                            .host_snapshot
                            .status
                            .last_reboot_requested
                            .as_ref()
                            .map(|x| x.time)
                            .unwrap_or(mh_snapshot.host_snapshot.state.version.timestamp());
                        let power_down_wait = ctx
                            .services
                            .site_config
                            .machine_state_controller
                            .power_down_wait;
                        if Utc::now().signed_duration_since(basetime) < power_down_wait {
                            return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                                "HandleBiosJobFailure: waiting for BMC to come back online; failure: {failure}"
                            )));
                        }
                        handler_host_power_control(mh_snapshot, ctx, ResetType::On).await?;
                        return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                            "HandleBiosJobFailure: powering on after BMC reset; failure: {failure}"
                        )));
                    }
                    tracing::info!(
                        retry_count = info.retry_count,
                        "HandleBiosJobFailure: BMC reset complete; re-running platform configuration (machine_setup) — power cycle does not apply BIOS attributes",
                    );
                    Ok(BiosConfigJobAdvanceOutcome::RetryPlatformConfiguration {
                        retry_count: info.retry_count,
                    })
                }
                _ => Err(StateHandlerError::GenericError(eyre!(
                    "HandleBiosJobFailure: unexpected power state {power_state:#?} for {}",
                    mh_snapshot.host_snapshot.id
                ))),
            }
        }
    }
}

/// Enter HandleBiosJobFailure recovery, or move to Failed when budget is exhausted.
fn try_bios_recovery_attempt(
    machine_controller_config: &MachineStateControllerConfig,
    retry_count: u32,
    bios_job_id: Option<String>,
    failure: String,
) -> Result<BiosRecoveryAttemptOutcome, StateHandlerError> {
    if retry_count >= machine_controller_config.max_bios_config_retries {
        tracing::warn!(
            retry_count,
            max_retries = machine_controller_config.max_bios_config_retries,
            reason = %failure,
            "BIOS recovery budget exhausted; moving host to Failed for manual remediation"
        );
        return Ok(BiosRecoveryAttemptOutcome::Failed {
            failure: format!(
                "{failure} (automated BIOS recovery exhausted after {} retries)",
                machine_controller_config.max_bios_config_retries
            ),
        });
    }
    Ok(BiosRecoveryAttemptOutcome::Continue(BiosConfigInfo {
        bios_job_id,
        bios_config_state: BiosConfigState::HandleBiosJobFailure {
            failure,
            power_state: ExpectedPowerState::Off,
        },
        retry_count: retry_count + 1,
    }))
}

/// The job a job-polling substate waits for, and where it stands.
async fn read_bios_job(
    services: &MachineStateHandlerServices,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    info: &BiosConfigInfo,
) -> Result<(VendorJobId, Result<OperationStatus, PlatformError>), StateHandlerError> {
    let job_id = info
        .bios_job_id
        .clone()
        .and_then(|job_id| VendorJobId::new(job_id).ok())
        .ok_or_else(|| {
            StateHandlerError::GenericError(eyre!(
                "{:?} requires bios_job_id for host {}",
                info.bios_config_state,
                mh_snapshot.host_snapshot.id
            ))
        })?;
    let status = vendor_job_status(services, redfish_client, mh_snapshot, &job_id).await?;
    Ok((job_id, status))
}

/// What the BMC of `mh_snapshot`'s host reports of the vendor job `job_id`.
pub(super) async fn vendor_job_status(
    services: &MachineStateHandlerServices,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    job_id: &VendorJobId,
) -> Result<Result<OperationStatus, PlatformError>, StateHandlerError> {
    let bmc = PlatformBmc::connect(services, mh_snapshot, redfish_client).await?;
    Ok(bmc.operation_status(&idrac_job(job_id.clone())).await)
}

pub(super) async fn advance_polling_bios_setup(
    services: &MachineStateHandlerServices,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    retry_count: u32,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<PollingBiosSetupOutcome, StateHandlerError> {
    let machine_controller_config = &services.site_config.machine_state_controller;
    let stuck_for = mh_snapshot.host_snapshot.state.version.since_state_change();
    let bmc = PlatformBmc::connect(services, mh_snapshot, redfish_client).await?;

    match platform_config::in_effect(&bmc, &services.site_config, boot_interface).await {
        Ok(true) => {
            tracing::info!("BIOS setup verified successfully");
            Ok(PollingBiosSetupOutcome::Verified)
        }
        Ok(false) => {
            if let Some(outcome) = escalate_stuck_polling_bios_setup(
                machine_controller_config,
                retry_count,
                stuck_for,
            )? {
                return Ok(outcome);
            }
            Ok(PollingBiosSetupOutcome::Wait(format!(
                "Polling BIOS setup status, waiting for settings to be applied (retry_count={retry_count})"
            )))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to check BIOS setup status, will retry"
            );
            Ok(PollingBiosSetupOutcome::Wait(format!(
                "Failed to check BIOS setup status: {e}. Will retry."
            )))
        }
    }
}

fn escalate_stuck_polling_bios_setup(
    machine_controller_config: &MachineStateControllerConfig,
    retry_count: u32,
    stuck_for: chrono::Duration,
) -> Result<Option<PollingBiosSetupOutcome>, StateHandlerError> {
    if stuck_for <= machine_controller_config.polling_bios_setup_stuck_threshold {
        return Ok(None);
    }

    tracing::warn!(
        ?stuck_for,
        retry_count,
        "PollingBiosSetup stuck; attempting HandleBiosJobFailure recovery (power-off + BMC reset + power-on + re-run machine_setup)"
    );

    let failure = format!(
        "PollingBiosSetup stuck for {} minutes (is_bios_setup returned false)",
        stuck_for.num_minutes()
    );

    Ok(Some(
        match try_bios_recovery_attempt(machine_controller_config, retry_count, None, failure)? {
            BiosRecoveryAttemptOutcome::Continue(info) => {
                PollingBiosSetupOutcome::EnterRecovery(info)
            }
            BiosRecoveryAttemptOutcome::Failed { failure } => {
                PollingBiosSetupOutcome::Failed { failure }
            }
        },
    ))
}

pub(super) async fn handle_bios_setup_failed_recovery(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
    explicit_target: Option<&BootInterfaceTarget>,
    recovered_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let boot_interface =
        super::resolve_boot_interface_for_step(ctx, mh_snapshot, explicit_target).await?;
    let boot_interface = boot_interface.into_target();
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;
    let bmc = PlatformBmc::connect(ctx.services, mh_snapshot, redfish_client.as_ref()).await?;
    match platform_config::in_effect(&bmc, &ctx.services.site_config, boot_interface.as_ref()).await
    {
        Ok(true) => {
            tracing::info!("BIOS setup verified after manual remediation; resuming state machine");
            Ok(StateHandlerOutcome::transition(recovered_state))
        }
        Ok(false) => Ok(StateHandlerOutcome::do_nothing()),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to check BIOS setup status, will retry"
            );
            Ok(StateHandlerOutcome::do_nothing())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalate_stuck_polling_bios_setup_not_triggered_before_threshold() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let result = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            0,
            chrono::Duration::minutes(10),
        )
        .unwrap();

        assert!(result.is_none());
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_enters_handle_bios_job_failure_when_stuck() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let info = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            0,
            chrono::Duration::minutes(16),
        )
        .unwrap()
        .expect("recovery should be triggered");
        let PollingBiosSetupOutcome::EnterRecovery(info) = info else {
            panic!("expected EnterRecovery");
        };
        assert_eq!(info.bios_job_id, None);
        assert_eq!(info.retry_count, 1);
        assert!(matches!(
            info.bios_config_state,
            BiosConfigState::HandleBiosJobFailure {
                power_state: ExpectedPowerState::Off,
                ..
            }
        ));
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_respects_shared_retry_budget() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let result = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            machine_controller_config.max_bios_config_retries,
            chrono::Duration::minutes(20),
        )
        .unwrap()
        .expect("expected Failed outcome");

        assert!(matches!(result, PollingBiosSetupOutcome::Failed { .. }));
    }

    #[test]
    fn try_bios_recovery_attempt_fails_when_budget_exhausted() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let result = try_bios_recovery_attempt(
            &machine_controller_config,
            machine_controller_config.max_bios_config_retries,
            Some("job-1".to_string()),
            "job failed".to_string(),
        )
        .unwrap();

        assert!(matches!(result, BiosRecoveryAttemptOutcome::Failed { .. }));
    }

    #[test]
    fn escalate_stuck_polling_bios_setup_allows_last_budgeted_attempt() {
        let machine_controller_config = MachineStateControllerConfig::default();
        let outcome = escalate_stuck_polling_bios_setup(
            &machine_controller_config,
            machine_controller_config.max_bios_config_retries - 1,
            chrono::Duration::minutes(20),
        )
        .unwrap()
        .expect("last budgeted recovery should be allowed");

        let PollingBiosSetupOutcome::EnterRecovery(info) = outcome else {
            panic!("expected EnterRecovery");
        };
        assert_eq!(
            info.retry_count,
            machine_controller_config.max_bios_config_retries
        );
    }
}
