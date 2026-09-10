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
use carbide_uuid::machine_validation::MachineValidationId;
use chrono::Utc;
use libredfish::{EnabledDisabled, RedfishError, SystemPowerControl};
use model::machine::{
    FailureCause, FailureDetails, FailureSource, MachineState, MachineValidatingState,
    ManagedHostState, ManagedHostStateSnapshot, StateMachineArea, UnlockHostState, ValidationState,
};
use model::machine_validation::{MachineValidationState, MachineValidationStatus};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{HostHandlerParams, is_machine_validation_requested, machine_validation_completed};
use crate::context::{MachineStateHandlerContextObjects, MachineStateHandlerServices};
use crate::handler::host_boot_config::{
    HostBootConfigCheckOutcome, HostBootConfigDecision, HostBootConfigDpuFreshness,
    HostBootConfigOutcome, HostBootConfigStage, check_host_boot_config, decide_host_boot_config,
    initial_set_boot_order_info, inspect_host_boot_config, run_host_boot_config_stage,
};
use crate::handler::{
    RequiredBootInterface, handler_host_power_control, host_power_control, rebooted, redfish_error,
    require_boot_interface, resolve_boot_interface_for_step, trigger_reboot_if_needed, wait,
};

/// The validation flavor of the managed-host state, so the repair arms can
/// transition between validation substates without repeating the wrapper.
fn validating(machine_validation: MachineValidatingState) -> ManagedHostState {
    ManagedHostState::Validation {
        validation_state: ValidationState::MachineValidation { machine_validation },
    }
}

/// Handles a shared boot-configuration stage for machine validation.
///
/// Called by `handle_machine_validation_state` for its BIOS configuration,
/// vendor-job, polling, and boot-order repair substates. This adapter preserves
/// the validation ID, maps continued stages back into `MachineValidatingState`,
/// continues to `LockAfterBootRepair` on completion, and closes the validation
/// run before reporting a terminal boot-configuration failure.
async fn handle_validation_boot_config_stage(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &ManagedHostStateSnapshot,
    validation_id: MachineValidationId,
    stage: HostBootConfigStage,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;

    match run_host_boot_config_stage(
        ctx,
        &host_handler_params.reachability_params,
        redfish_client.as_ref(),
        mh_snapshot,
        None,
        stage,
    )
    .await?
    {
        HostBootConfigOutcome::Continue(stage) => {
            let machine_validation = match stage {
                HostBootConfigStage::ConfigureBios { retry_count } => {
                    MachineValidatingState::ConfigureBootBios {
                        validation_id,
                        retry_count,
                    }
                }
                HostBootConfigStage::WaitingForBiosJob { bios_config_info } => {
                    MachineValidatingState::WaitingForBootBiosJob {
                        validation_id,
                        bios_config_info,
                    }
                }
                HostBootConfigStage::PollingBiosSetup { retry_count } => {
                    MachineValidatingState::PollingBootBiosSetup {
                        validation_id,
                        retry_count,
                    }
                }
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info,
                } => MachineValidatingState::RepairBootConfig {
                    validation_id,
                    set_boot_order_info,
                },
            };

            Ok(StateHandlerOutcome::transition(validating(
                machine_validation,
            )))
        }
        HostBootConfigOutcome::Complete => Ok(StateHandlerOutcome::transition(validating(
            MachineValidatingState::LockAfterBootRepair { validation_id },
        ))),
        HostBootConfigOutcome::Wait(reason) => Ok(StateHandlerOutcome::wait(reason)),
        HostBootConfigOutcome::Failed { failure } => {
            let machine_id = mh_snapshot.host_snapshot.id;
            let mut txn = ctx.services.db_pool.begin().await?;
            let completed = db::machine_validation::mark_machine_validation_complete(
                txn.as_mut(),
                &machine_id,
                &validation_id,
                MachineValidationStatus {
                    state: MachineValidationState::Failed,
                    ..MachineValidationStatus::default()
                },
            )
            .await?;

            if !completed {
                tracing::info!(
                    %machine_id,
                    machine_validation_id = %validation_id,
                    "Machine validation boot-config failure observed after the run was already terminal"
                );
            }

            Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::BiosSetupFailed { err: failure },
                    failed_at: Utc::now(),
                    source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
                },
                machine_id: machine_id.into(),
                retry_count: 0,
            })
            .with_txn(txn))
        }
    }
}

async fn skip_machine_validation(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    validation_id: &MachineValidationId,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!("Skipping Machine Validation");
    let machine_id = mh_snapshot.host_snapshot.id;
    let mut txn = ctx.services.db_pool.begin().await?;
    let completed = db::machine_validation::mark_machine_validation_complete(
        txn.as_mut(),
        &machine_id,
        validation_id,
        MachineValidationStatus {
            state: MachineValidationState::Skipped,
            ..MachineValidationStatus::default()
        },
    )
    .await?;
    if !completed {
        tracing::info!(
            %machine_id,
            machine_validation_id = %validation_id,
            "machine validation completion ignored because run is no longer active"
        );
        return Ok(StateHandlerOutcome::do_nothing().with_txn(txn));
    }
    let machine_validation = db::machine_validation::find_by_id(txn.as_mut(), validation_id)
        .await
        .map_err(|err| StateHandlerError::GenericError(err.into()))?;

    *ctx.metrics
        .last_machine_validation_list
        .entry((
            machine_validation.machine_id.to_string(),
            machine_validation.context.unwrap_or_default(),
        ))
        .or_default() = 0_i32;

    Ok(StateHandlerOutcome::transition(ManagedHostState::HostInit {
        machine_state: MachineState::Discovered {
            skip_reboot_wait: true,
        },
    })
    .with_txn(txn))
}

pub(crate) async fn handle_machine_validation_state(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    machine_validation: &MachineValidatingState,
    host_handler_params: &HostHandlerParams,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match machine_validation {
        MachineValidatingState::RebootHost { validation_id } => {
            if !host_handler_params.machine_validation_config.enabled {
                return skip_machine_validation(ctx, validation_id, mh_snapshot).await;
            }
            // Handle reboot host case
            handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart).await?;
            let machine_validation =
                db::machine_validation::find_by_id(&mut ctx.services.db_reader, validation_id)
                    .await
                    .map_err(|err| StateHandlerError::GenericError(err.into()))?;

            let next_state = ManagedHostState::Validation {
                validation_state: ValidationState::MachineValidation {
                    machine_validation: MachineValidatingState::MachineValidating {
                        context: machine_validation.context.unwrap_or_default(),
                        id: *validation_id,
                        completed: 1,
                        total: 1,
                        is_enabled: host_handler_params.machine_validation_config.enabled,
                    },
                },
            };
            Ok(StateHandlerOutcome::transition(next_state))
        }
        MachineValidatingState::MachineValidating {
            context,
            id,
            completed,
            total,
            is_enabled,
        } => {
            tracing::trace!(
                machine_validation_context = %context,
                machine_validation_id = %id,
                completed_validation_count = completed,
                total_validation_count = total,
                validation_enabled = is_enabled,
                "machine validation progress",
            );
            if !rebooted(&mh_snapshot.host_snapshot) {
                // Ensure the boot config is still what it should be while
                // waiting. If it reads reverted -- changed externally, a BIOS
                // quirk, or the boot NIC dropping off the BMC's inventory
                // during the reboot's POST -- the host can't boot into the
                // validation environment, and further reboots can't restore a
                // setting that is gone. Correct it instead of pacing forever.
                let boot_interface_resolution =
                    resolve_boot_interface_for_step(ctx, mh_snapshot, None).await?;
                if let RequiredBootInterface::Ready(boot_interface) = require_boot_interface(
                    &mh_snapshot.host_snapshot.id,
                    boot_interface_resolution,
                    "verifying the boot config during validation",
                    |message| message,
                )? {
                    let redfish_client = ctx
                        .services
                        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                        .await?;
                    match inspect_host_boot_config(
                        redfish_client.as_ref(),
                        mh_snapshot,
                        &boot_interface,
                    )
                    .await
                    {
                        Ok(inspection)
                            if matches!(
                                decide_host_boot_config(inspection),
                                HostBootConfigDecision::ConfigureBios
                                    | HostBootConfigDecision::SetBootOrder
                            ) =>
                        {
                            tracing::warn!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                "Boot config reads reverted while waiting for the validation reboot; correcting it via boot repair",
                            );
                            return Ok(StateHandlerOutcome::transition(validating(
                                MachineValidatingState::PrepareBootRepair { validation_id: *id },
                            )));
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                error = %e,
                                "Could not verify the boot config while waiting for the validation reboot; will retry",
                            );
                        }
                    }
                }
                let status = trigger_reboot_if_needed(
                    &mh_snapshot.host_snapshot,
                    mh_snapshot,
                    None,
                    &host_handler_params.reachability_params,
                    ctx,
                )
                .await?;
                return Ok(StateHandlerOutcome::wait(status.status));
            }
            if !host_handler_params.machine_validation_config.enabled {
                return skip_machine_validation(ctx, id, mh_snapshot).await;
            }
            // Host validation completed
            if machine_validation_completed(&mh_snapshot.host_snapshot) {
                if mh_snapshot.host_snapshot.status.failure_details.cause == FailureCause::NoError {
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "machine validation completed"
                    );
                    let machine_validation =
                        db::machine_validation::find_by_id(&mut ctx.services.db_reader, id)
                            .await
                            .map_err(|err| StateHandlerError::GenericError(err.into()))?;
                    let status = machine_validation.status.clone().unwrap_or_default();
                    *ctx.metrics
                        .last_machine_validation_list
                        .entry((
                            machine_validation.machine_id.to_string(),
                            machine_validation.context.clone().unwrap_or_default(),
                        ))
                        .or_default() = status.total - status.completed;
                    handler_host_power_control(mh_snapshot, ctx, SystemPowerControl::ForceRestart)
                        .await?;
                    return Ok(StateHandlerOutcome::transition(
                        ManagedHostState::HostInit {
                            machine_state: MachineState::Discovered {
                                skip_reboot_wait: false,
                            },
                        },
                    ));
                } else {
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "machine validation failed"
                    );
                    return Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
                        details: mh_snapshot.host_snapshot.status.failure_details.clone(),
                        machine_id: mh_snapshot.host_snapshot.id.into(),
                        retry_count: 0,
                    }));
                }
            }
            Ok(StateHandlerOutcome::do_nothing())
        }
        MachineValidatingState::PrepareBootRepair { validation_id } => {
            // Boot repair writes BIOS settings, and lockdown was enabled
            // earlier in HostInit -- writes bounce off a locked BMC. Check and
            // unlock first, mirroring host boot repair.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            let next = match redfish_client.lockdown_status().await {
                Err(RedfishError::NotSupported(_)) => {
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "BMC vendor does not support checking lockdown status during validation boot repair",
                    );
                    MachineValidatingState::CheckBootConfigForRepair {
                        validation_id: *validation_id,
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        error = %e,
                        "Failed to fetch lockdown status during validation boot repair",
                    );
                    return Ok(StateHandlerOutcome::wait(format!(
                        "Failed to fetch lockdown status: {e}"
                    )));
                }
                Ok(lockdown_status) if !lockdown_status.is_fully_disabled() => {
                    tracing::info!(
                        machine_id = %mh_snapshot.host_snapshot.id,
                        "Lockdown is enabled during validation boot repair; disabling before boot config writes",
                    );
                    MachineValidatingState::UnlockForBootRepair {
                        validation_id: *validation_id,
                        unlock_host_state: UnlockHostState::DisableLockdown,
                    }
                }
                Ok(_) => MachineValidatingState::CheckBootConfigForRepair {
                    validation_id: *validation_id,
                },
            };

            Ok(StateHandlerOutcome::transition(validating(next)))
        }
        MachineValidatingState::UnlockForBootRepair {
            validation_id,
            unlock_host_state,
        } => {
            // Mirror host boot repair's unlock choreography.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            let next = match unlock_host_state {
                UnlockHostState::DisableLockdown => {
                    // Tolerate a vendor that reports lockdown status but does
                    // not support setting it, symmetric with the re-lock step.
                    match redfish_client.lockdown_bmc(EnabledDisabled::Disabled).await {
                        Ok(()) => {}
                        Err(RedfishError::NotSupported(_)) => {
                            tracing::info!(
                                machine_id = %mh_snapshot.host_snapshot.id,
                                "BMC vendor does not support disabling lockdown during validation boot repair",
                            );
                        }
                        Err(e) => return Err(redfish_error("lockdown_bmc", e)),
                    }

                    let vendor = mh_snapshot.host_snapshot.bmc_vendor();
                    if vendor.is_supermicro() {
                        tracing::info!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            %vendor,
                            "BMC lockdown disabled; rebooting host so Redfish reflects actual boot state",
                        );
                        MachineValidatingState::UnlockForBootRepair {
                            validation_id: *validation_id,
                            unlock_host_state: UnlockHostState::RebootHost,
                        }
                    } else {
                        MachineValidatingState::CheckBootConfigForRepair {
                            validation_id: *validation_id,
                        }
                    }
                }
                UnlockHostState::RebootHost => {
                    host_power_control(
                        redfish_client.as_ref(),
                        &mh_snapshot.host_snapshot,
                        SystemPowerControl::ForceRestart,
                        ctx,
                    )
                    .await
                    .map_err(|e| {
                        StateHandlerError::GenericError(eyre::eyre!(
                            "failed to ForceRestart host after disabling BMC lockdown: {}",
                            e
                        ))
                    })?;

                    MachineValidatingState::UnlockForBootRepair {
                        validation_id: *validation_id,
                        unlock_host_state: UnlockHostState::WaitForUefiBoot,
                    }
                }
                UnlockHostState::WaitForUefiBoot => {
                    let entered_at = mh_snapshot.host_snapshot.state.version.timestamp();
                    if wait(
                        &entered_at,
                        host_handler_params.reachability_params.uefi_boot_wait,
                    ) {
                        return Ok(StateHandlerOutcome::wait(format!(
                            "Waiting for UEFI boot to complete on {} after post-unlock reboot",
                            mh_snapshot.host_snapshot.id
                        )));
                    }
                    MachineValidatingState::CheckBootConfigForRepair {
                        validation_id: *validation_id,
                    }
                }
            };

            Ok(StateHandlerOutcome::transition(validating(next)))
        }
        MachineValidatingState::CheckBootConfigForRepair { validation_id } => {
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            let next = match check_host_boot_config(
                redfish_client.as_ref(),
                mh_snapshot,
                &host_handler_params.reachability_params,
                HostBootConfigDpuFreshness::CurrentHostState,
                None,
                ctx,
            )
            .await?
            {
                HostBootConfigCheckOutcome::Wait(reason) => {
                    return Ok(StateHandlerOutcome::wait(reason));
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::ConfigureBios) => {
                    MachineValidatingState::ConfigureBootBios {
                        validation_id: *validation_id,
                        retry_count: 0,
                    }
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::SetBootOrder) => {
                    MachineValidatingState::RepairBootConfig {
                        validation_id: *validation_id,
                        set_boot_order_info: initial_set_boot_order_info(),
                    }
                }
                HostBootConfigCheckOutcome::Ready(HostBootConfigDecision::Complete) => {
                    MachineValidatingState::LockAfterBootRepair {
                        validation_id: *validation_id,
                    }
                }
            };

            Ok(StateHandlerOutcome::transition(validating(next)))
        }
        MachineValidatingState::ConfigureBootBios {
            validation_id,
            retry_count,
        } => {
            handle_validation_boot_config_stage(
                ctx,
                host_handler_params,
                mh_snapshot,
                *validation_id,
                HostBootConfigStage::ConfigureBios {
                    retry_count: *retry_count,
                },
            )
            .await
        }
        MachineValidatingState::WaitingForBootBiosJob {
            validation_id,
            bios_config_info,
        } => {
            handle_validation_boot_config_stage(
                ctx,
                host_handler_params,
                mh_snapshot,
                *validation_id,
                HostBootConfigStage::WaitingForBiosJob {
                    bios_config_info: bios_config_info.clone(),
                },
            )
            .await
        }
        MachineValidatingState::PollingBootBiosSetup {
            validation_id,
            retry_count,
        } => {
            handle_validation_boot_config_stage(
                ctx,
                host_handler_params,
                mh_snapshot,
                *validation_id,
                HostBootConfigStage::PollingBiosSetup {
                    retry_count: *retry_count,
                },
            )
            .await
        }
        MachineValidatingState::RepairBootConfig {
            validation_id,
            set_boot_order_info,
        } => {
            handle_validation_boot_config_stage(
                ctx,
                host_handler_params,
                mh_snapshot,
                *validation_id,
                HostBootConfigStage::SetBootOrder {
                    set_boot_order_info: set_boot_order_info.clone(),
                },
            )
            .await
        }
        MachineValidatingState::LockAfterBootRepair { validation_id } => {
            // Restore the lockdown that boot repair temporarily opened, then
            // resume validation from its reboot step.
            let redfish_client = ctx
                .services
                .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
                .await?;

            if mh_snapshot.host_snapshot.host_profile.disable_lockdown {
                tracing::info!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Skipping lockdown re-enable after validation boot repair per expected-machine config",
                );
            } else {
                match redfish_client.lockdown_bmc(EnabledDisabled::Enabled).await {
                    Ok(()) => {}
                    Err(RedfishError::NotSupported(_)) => {
                        tracing::info!(
                            machine_id = %mh_snapshot.host_snapshot.id,
                            "BMC vendor does not support re-enabling lockdown after validation boot repair",
                        );
                    }
                    Err(e) => return Err(redfish_error("lockdown_bmc", e)),
                }
            }

            Ok(StateHandlerOutcome::transition(validating(
                MachineValidatingState::RebootHost {
                    validation_id: *validation_id,
                },
            )))
        }
    }
}

pub(crate) async fn handle_machine_validation_requested(
    services: &MachineStateHandlerServices,
    mh_snapshot: &ManagedHostStateSnapshot,
    clear_failure_details: bool,
) -> Result<Option<StateHandlerOutcome<ManagedHostState>>, StateHandlerError> {
    if is_machine_validation_requested(mh_snapshot).await {
        let mut txn = services.db_pool.begin().await?;
        if clear_failure_details {
            // Clear the error so that state machine doesnt get into loop
            db::machine::clear_failure_details(&mh_snapshot.host_snapshot.id, txn.as_mut()).await?;
        }
        let machine_validation =
            match db::machine_validation::find_active_machine_validation_by_machine_id(
                txn.as_mut(),
                &mh_snapshot.host_snapshot.id,
            )
            .await
            {
                Ok(data) => data,
                Err(e) => {
                    tracing::info!(
                        error = %e,
                        "find_active_machine_validation_by_machine_id"
                    );
                    db::machine::set_machine_validation_request(
                        txn.as_mut(),
                        &mh_snapshot.host_snapshot.id,
                        true,
                    )
                    .await?;
                    // Health Alert ?
                    // Rare screnario, if something googfed up in DB
                    return Ok(Some(StateHandlerOutcome::do_nothing().with_txn(txn)));
                }
            };

        let next_state = ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::RebootHost {
                    validation_id: machine_validation.id,
                },
            },
        };
        return Ok(Some(
            StateHandlerOutcome::transition(next_state).with_txn(txn),
        ));
    }
    Ok(None)
}
