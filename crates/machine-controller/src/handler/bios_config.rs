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

//! BIOS configuration: machine_setup, Dell job wait/recovery, and PollingBiosSetup escalation.

use carbide_redfish::boot_interface::BootInterfaceTarget;
use carbide_redfish::libredfish::error::state_handler_redfish_error as redfish_error;
use chrono::Utc;
use eyre::eyre;
use libredfish::{Redfish, SystemPowerControl};
use model::machine::{
    BiosConfigInfo, BiosConfigState, ManagedHostState, ManagedHostStateSnapshot, PowerState,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{
    ReachabilityParams, RebootStatus, call_machine_setup_and_handle_no_dpu_error,
    handler_host_power_control, trigger_reboot_if_needed,
};
use crate::config::MachineStateControllerConfig;
use crate::context::MachineStateHandlerContextObjects;

/// Outcome of configure_host_bios function.
pub(super) enum BiosConfigOutcome {
    Done,
    WaitingForReboot(String),
    /// Dell BIOS PATCH returned a job ID; wait for it to complete before boot order.
    WaitingForBiosJob(BiosConfigInfo),
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

pub(super) async fn configure_host_bios(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    reachability_params: &ReachabilityParams,
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    boot_interface: Option<&BootInterfaceTarget>,
    retry_count: u32,
) -> Result<BiosConfigOutcome, StateHandlerError> {
    let bios_job_id = match call_machine_setup_and_handle_no_dpu_error(
        redfish_client,
        boot_interface,
        mh_snapshot.host_snapshot.associated_dpu_machine_ids().len(),
        &ctx.services.site_config,
    )
    .await
    {
        Err(e) => {
            tracing::warn!(
                machine_id = %mh_snapshot.host_snapshot.id,
                error = %e,
                "redfish machine_setup failed, potentially due to known race condition between UEFI POST and BMC. triggering force-restart if needed"
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
                handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart)
                    .await?;

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
                "redfish machine_setup failed: {e}; triggered host reboot: {reboot_status:#?}"
            )));
        }
        Ok(jid) => jid,
    };

    if let Some(job_id) = bios_job_id {
        return Ok(BiosConfigOutcome::WaitingForBiosJob(BiosConfigInfo {
            bios_job_id: Some(job_id),
            bios_config_state: BiosConfigState::WaitForBiosJobScheduled,
            retry_count,
        }));
    }

    // No job to wait for (non-Dell or vendor that doesn't return job); reboot to apply and continue.
    handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart).await?;
    Ok(BiosConfigOutcome::Done)
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
            let job_id = info.bios_job_id.as_ref().ok_or_else(|| {
                StateHandlerError::GenericError(eyre!(
                    "WaitForBiosJobScheduled requires bios_job_id for host {}",
                    mh_snapshot.host_snapshot.id
                ))
            })?;
            let job_state = redfish_client
                .get_job_state(job_id)
                .await
                .map_err(|e| redfish_error("get_job_state", e))?;
            if job_state.is_error_state() {
                let failure = format!("BIOS job {} failed with state {job_state:#?}", job_id);
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
            if matches!(job_state, libredfish::JobState::Completed) {
                tracing::info!(
                    %job_id,
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "BIOS job completed before scheduling was observed; skipping reboot",
                );
                return Ok(BiosConfigJobAdvanceOutcome::Done);
            }
            if !matches!(job_state, libredfish::JobState::Scheduled) {
                return Err(StateHandlerError::GenericError(eyre!(
                    "waiting for BIOS job {:#?} to be scheduled; current state: {job_state:#?}",
                    job_id
                )));
            }
            Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                bios_job_id: info.bios_job_id,
                bios_config_state: BiosConfigState::RebootHost,
                retry_count: info.retry_count,
            }))
        }
        BiosConfigState::RebootHost => {
            handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart).await?;
            Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                bios_job_id: info.bios_job_id,
                bios_config_state: BiosConfigState::WaitForBiosJobCompletion,
                retry_count: info.retry_count,
            }))
        }
        BiosConfigState::WaitForBiosJobCompletion => {
            const JOB_QUERY_WAIT_MINUTES: i64 = 5;
            let job_id = info.bios_job_id.as_ref().ok_or_else(|| {
                StateHandlerError::GenericError(eyre!(
                    "WaitForBiosJobCompletion requires bios_job_id for host {}",
                    mh_snapshot.host_snapshot.id
                ))
            })?;
            let job_state = match redfish_client.get_job_state(job_id).await {
                Ok(s) => s,
                Err(e) => {
                    let minutes_since_state_change = mh_snapshot
                        .host_snapshot
                        .state
                        .version
                        .since_state_change()
                        .num_minutes();
                    if minutes_since_state_change < JOB_QUERY_WAIT_MINUTES {
                        return Err(redfish_error("get_job_state", e));
                    }
                    let failure = format!(
                        "BIOS config job {} lookup failed after {} min: {}",
                        job_id, minutes_since_state_change, e
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
            match job_state {
                libredfish::JobState::Completed => Ok(BiosConfigJobAdvanceOutcome::Done),
                _ if job_state.is_error_state() => {
                    let failure = format!(
                        "BIOS config job {} failed with state {job_state:#?}",
                        job_id
                    );
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
                _ => Err(StateHandlerError::GenericError(eyre!(
                    "waiting for BIOS job {:#?} to complete; current state: {job_state:#?}",
                    job_id
                ))),
            }
        }
        BiosConfigState::HandleBiosJobFailure {
            failure,
            power_state,
        } => {
            let current_power_state = redfish_client
                .get_power_state()
                .await
                .map_err(|e| redfish_error("get_power_state", e))?;

            match power_state {
                PowerState::Off => {
                    if current_power_state != libredfish::PowerState::Off {
                        handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceOff)
                            .await?;
                        return Ok(BiosConfigJobAdvanceOutcome::Wait(format!(
                            "HandleBiosJobFailure: waiting for power down; current power state: {current_power_state}; failure: {failure}"
                        )));
                    }
                    tracing::info!(
                        reason = %failure,
                        "HandleBiosJobFailure: resetting BMC after BIOS job failure"
                    );
                    redfish_client
                        .bmc_reset(None)
                        .await
                        .map_err(|e| redfish_error("bmc_reset", e))?;
                    Ok(BiosConfigJobAdvanceOutcome::Continue(BiosConfigInfo {
                        bios_job_id: info.bios_job_id,
                        bios_config_state: BiosConfigState::HandleBiosJobFailure {
                            failure,
                            power_state: PowerState::On,
                        },
                        retry_count: info.retry_count,
                    }))
                }
                PowerState::On => {
                    if current_power_state != libredfish::PowerState::On {
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
                        handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::On)
                            .await?;
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
            power_state: PowerState::Off,
        },
        retry_count: retry_count + 1,
    }))
}

pub(super) async fn advance_polling_bios_setup(
    redfish_client: &dyn Redfish,
    mh_snapshot: &ManagedHostStateSnapshot,
    retry_count: u32,
    machine_controller_config: &MachineStateControllerConfig,
    boot_interface: Option<&BootInterfaceTarget>,
) -> Result<PollingBiosSetupOutcome, StateHandlerError> {
    let stuck_for = mh_snapshot.host_snapshot.state.version.since_state_change();

    let is_bios_setup_result = match boot_interface {
        Some(target) => {
            target
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
        }
        None => redfish_client.is_bios_setup(None).await,
    };
    match is_bios_setup_result {
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
    let is_bios_setup_result = match &boot_interface {
        Some(target) => {
            target
                .run(|bi| redfish_client.is_bios_setup(Some(bi)))
                .await
        }
        None => redfish_client.is_bios_setup(None).await,
    };
    match is_bios_setup_result {
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
                power_state: PowerState::Off,
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
