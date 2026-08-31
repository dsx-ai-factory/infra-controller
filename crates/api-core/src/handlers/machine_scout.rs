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
use ::rpc::forge::ForgeAgentControlResponse;
use ::rpc::model::machine::get_action_for_dpu_state;
use ::rpc::{forge as rpc, forge_agent_control_response as fac, scout_firmware_upgrade as sfu};
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{
    BomValidating, CleanupContext, CleanupState, FailureCause, FailureDetails, FailureSource,
    HostReprovisionState, InstanceState, MachineState, MachineValidatingState, ManagedHostState,
    MeasuringState, StateMachineArea, ValidationState,
};
use model::machine_validation::{MachineValidationState, MachineValidationStatus};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::metrics::ApiMetricsEmitter;
use crate::api::{Api, log_request_data};
use crate::compat::BuildAndFillLegacyFields;
use crate::handlers::utils::convert_and_log_machine_id;

// Records Scout cleanup success/failure and wakes the host state controller.
// The state controller decides whether cleanup returns to discovery or deprovision flow.
pub(crate) async fn cleanup_machine_completed(
    api: &Api,
    request: Request<rpc::MachineCleanupInfo>,
) -> Result<Response<rpc::MachineCleanupResult>, Status> {
    log_request_data(&request);

    let cleanup_info = request.into_inner();
    tracing::info!(?cleanup_info, "cleanup_machine_completed");

    let machine_id = convert_and_log_machine_id(cleanup_info.machine_id.as_ref())?;

    // Load machine from DB
    let (machine, mut txn) = api
        .load_machine(&machine_id, MachineSearchConfig::default())
        .await?;

    let cleanup_error = [
        ("NVMe", cleanup_info.nvme.as_ref()),
        ("HDD/SAS", cleanup_info.hdd.as_ref()),
    ]
    .into_iter()
    .find_map(|(label, result)| {
        result.and_then(|result| {
            (rpc::machine_cleanup_info::CleanupResult::Error as i32 == result.result)
                .then(|| format!("{label} cleanup failed: {}", result.message))
        })
    });

    // Check if cleanup failed
    if let Some(err) = cleanup_error {
        // Storage cleanup failed. Move machine to failed state.
        tracing::warn!(
            machine_id = %machine_id,
            error = %err,
            "Storage cleanup failed"
        );
        let failure_source = match machine.current_state() {
            ManagedHostState::WaitingForCleanup {
                cleanup_context: CleanupContext::InitialDiscovery,
                ..
            } => FailureSource::StateMachineArea(StateMachineArea::HostInit),
            // Preserve the original HostInit context across cleanup retries so recovery returns to
            // HostInit/WaitingForDiscovery instead of the deprovision cleanup flow.
            ManagedHostState::Failed {
                details:
                    FailureDetails {
                        cause: FailureCause::NVMECleanFailed { .. },
                        source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
                        ..
                    },
                ..
            } => FailureSource::StateMachineArea(StateMachineArea::HostInit),
            _ => FailureSource::Scout,
        };
        db::machine::update_failure_details(
            &machine,
            &mut txn,
            FailureDetails {
                cause: FailureCause::NVMECleanFailed { err },
                failed_at: chrono::Utc::now(),
                source: failure_source,
            },
        )
        .await?;
    } else {
        // Cleanup succeeded or was skipped (field not present means scout skipped it)
        if cleanup_info.nvme.is_none() {
            tracing::info!(
                machine_id = %machine_id,
                "NVMe cleanup skipped by scout (likely due to safety check)"
            );
        }
        if cleanup_info.hdd.is_none() {
            tracing::info!(
                machine_id = %machine_id,
                "HDD/SAS cleanup skipped by scout (likely due to safety check)"
            );
        }
        // Update cleanup time on success
        db::machine::update_cleanup_time(&machine, &mut txn).await?;
    }

    txn.commit().await?;

    // State handler should mark Machine as Adopted and reboot host for bios/bmc lockdown.
    // Wake it up
    if machine_id.machine_type().is_host()
        && let Err(err) = api
            .machine_state_handler_enqueuer
            .enqueue_object(&machine_id)
            .await
    {
        tracing::warn!(%err, %machine_id, "Failed to wake up state handler for machine");
    }

    Ok(Response::new(rpc::MachineCleanupResult {}))
}

// Invoked by forge-scout whenever a certain Machine can not be properly acted on
pub(crate) fn report_forge_scout_error(
    _api: &Api,
    request: Request<rpc::ForgeScoutErrorReport>,
) -> Result<Response<rpc::ForgeScoutErrorReportResult>, Status> {
    log_request_data(&request);
    let _machine_id = convert_and_log_machine_id(request.into_inner().machine_id.as_ref())?;

    // `log_request_data` will already provide us the error message
    // Therefore we don't have to do anything else
    Ok(Response::new(rpc::ForgeScoutErrorReportResult {}))
}

// Called on x86 boot by 'forge-scout auto-detect --uuid=<uuid>'.
// Tells it whether to discover or cleanup based on current machine state.
pub(crate) async fn forge_agent_control(
    api: &Api,
    request: Request<rpc::ForgeAgentControlRequest>,
) -> Result<Response<rpc::ForgeAgentControlResponse>, Status> {
    log_request_data(&request);

    use rpc::forge_agent_control_response::Action;

    let machine_id = convert_and_log_machine_id(request.into_inner().machine_id.as_ref())?;

    let (machine, mut txn) = api
        .load_machine(&machine_id, MachineSearchConfig::default())
        .await?;

    let is_dpu = machine.is_dpu();
    let host_machine = if !is_dpu {
        machine.clone()
    } else {
        db::machine::find_host_by_dpu_machine_id(&mut txn, &machine_id)
            .await?
            .ok_or(CarbideError::NotFoundError {
                kind: "machine",
                id: machine_id.to_string(),
            })?
    };

    if !is_dpu {
        db::machine::update_scout_contact_time(&machine_id, &mut txn).await?;
    }

    // Respond based on machine current state
    let state = host_machine.current_state();

    let (action, maybe_pending_txn) = if is_dpu {
        (
            get_action_for_dpu_state(state, &machine_id).map_err(CarbideError::from)?,
            Some(txn),
        )
    } else {
        match state {
            ManagedHostState::HostInit {
                machine_state: MachineState::Init,
            } => (Action::retry(), Some(txn)),
            ManagedHostState::Validation {
                validation_state:
                    ValidationState::MachineValidation {
                        machine_validation:
                            MachineValidatingState::MachineValidating {
                                context,
                                id,
                                completed,
                                total,
                                is_enabled,
                            },
                    },
            } => {
                tracing::info!(
                    " context : {} id: {} is_enabled: {}, completed {}, total {}",
                    context,
                    id,
                    is_enabled,
                    completed,
                    total,
                );
                if *is_enabled {
                    db::machine_validation::update_status(
                        &mut txn,
                        id,
                        MachineValidationStatus {
                            state: MachineValidationState::InProgress,
                            ..MachineValidationStatus::default()
                        },
                    )
                    .await?;
                    let machine_validation =
                        db::machine_validation::find_by_id(&mut txn, id).await?;
                    (
                        Action::MachineValidation(fac::MachineValidation {
                            is_enabled: true,
                            context: context.clone(),
                            validation_id: Some(*id),
                            filter: Some(machine_validation.filter.unwrap_or_default().into()),
                        }),
                        Some(txn),
                    )
                } else {
                    // This avoids sending Machine validation command scout
                    tracing::info!("Skipped machine validation");
                    (Action::noop(), Some(txn))
                }
            }
            ManagedHostState::HostInit {
                machine_state: MachineState::WaitingForDiscovery,
            } => {
                // A predicted host isn't the real machine yet, so don't make it wait on
                // cleanup: send it to discovery, which promotes it; the promoted host then
                // waits for its storage cleanup. Mirrors the state handler's
                // WaitingForDiscovery guard.
                if host_machine.last_cleanup_time.is_some()
                    || !host_machine.id.machine_type().is_host()
                {
                    (Action::discovery(), Some(txn))
                } else {
                    tracing::info!("Waiting for initial storage cleanup before host discovery");
                    (Action::retry(), Some(txn))
                }
            }
            ManagedHostState::Failed {
                details:
                    FailureDetails {
                        cause: FailureCause::Discovery { .. },
                        ..
                    },
                ..
            } => (Action::discovery(), Some(txn)),
            // If the API is configured with attestation_enabled, and
            // the machine has been Discovered (and progressed on to the
            // point where it is WaitingForMeasurements), then let Scout (or
            // whoever the caller is) know that it's time for measurements
            // to be sent.
            ManagedHostState::Measuring {
                measuring_state: MeasuringState::WaitingForMeasurements,
            } => (Action::measure(), Some(txn)),
            ManagedHostState::WaitingForCleanup {
                cleanup_state: CleanupState::HostCleanup { .. },
                ..
            }
            | ManagedHostState::Failed {
                details:
                    FailureDetails {
                        cause: FailureCause::NVMECleanFailed { .. },
                        ..
                    },
                ..
            } => {
                let last_cleanup_time = host_machine.last_cleanup_time;
                let state_version = host_machine.state.version;
                tracing::info!(
                    "last_cleanup_time: {:?}, state_version: {:?}",
                    last_cleanup_time,
                    state_version
                );
                // Check scout has already cleaned up the machine
                if last_cleanup_time.unwrap_or_default() > state_version.timestamp() {
                    tracing::info!("Cleanup is already done");
                    (Action::noop(), Some(txn))
                } else {
                    (Action::reset(), Some(txn))
                }
            }
            ManagedHostState::BomValidating {
                bom_validating_state: BomValidating::UpdatingInventory(_),
            } => {
                tracing::info!(
                    "Request Discovery {} < {}",
                    machine.last_discovery_time.unwrap_or_default(),
                    machine.current_version().timestamp()
                );
                if machine.last_discovery_time.unwrap_or_default()
                    < machine.current_version().timestamp()
                {
                    (Action::discovery(), Some(txn))
                } else {
                    (Action::noop(), Some(txn))
                }
            }
            ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForDpaToBeReady,
            } => {
                // Commit the transaction now, to avoid holding across an unrelated await point
                txn.commit().await?;
                match crate::handlers::svpc::process_scout_req(api, machine_id).await {
                    Ok(action) => (action, None),
                    Err(e) => {
                        tracing::error!("Error returned from process_scout_req: {e}");
                        (Action::noop(), None)
                    }
                }
            }

            ManagedHostState::HostReprovision {
                reprovision_state:
                    HostReprovisionState::WaitingForScoutUpgrade {
                        task_json,
                        result: None,
                        ..
                    },
                ..
            }
            | ManagedHostState::Assigned {
                instance_state:
                    InstanceState::HostReprovision {
                        reprovision_state:
                            HostReprovisionState::WaitingForScoutUpgrade {
                                task_json,
                                result: None,
                                ..
                            },
                    },
            } => {
                tracing::info!(
                    machine_id = %machine.id,
                    "Sending firmware upgrade task to scout",
                );
                let action = match serde_json::from_str::<sfu::ScoutFirmwareUpgradeTask>(task_json)
                {
                    Ok(task) => Action::FirmwareUpgrade(fac::FirmwareUpgrade { task: Some(task) }),
                    Err(e) => {
                        tracing::warn!(
                            "Could not deserialize firmware upgrade task, sending no-op action to scout: {e}"
                        );
                        Action::noop()
                    }
                };
                (action, Some(txn))
            }

            _ => {
                // Later this might go to site admin dashboard for manual intervention
                tracing::info!(
                    machine_id = %machine.id,
                    machine_type = "Host",
                    %state,
                    "forge agent control",
                );
                (Action::noop(), Some(txn))
            }
        }
    };

    tracing::info!(
        machine_id = %machine.id,
        action = action.as_str_name(),
        "forge agent control",
    );

    if let Some(txn) = maybe_pending_txn {
        txn.commit().await?;
    }

    Ok(Response::new(
        ForgeAgentControlResponse::build_and_fill_legacy_fields(action)?,
    ))
}

/// Records reboot duration metric for a machine if applicable
fn record_reboot_duration_metric(
    metric_emitter: &ApiMetricsEmitter,
    machine: &model::machine::Machine,
) {
    let Some(last_reboot_requested) = &machine.last_reboot_requested else {
        return;
    };

    // Skip recording metrics for PowerOff requests
    if matches!(
        last_reboot_requested.mode,
        model::machine::MachineLastRebootRequestedMode::PowerOff
    ) {
        return;
    }

    let reboot_duration_secs = (chrono::Utc::now() - last_reboot_requested.time).num_seconds();

    // Only record positive durations (in case of clock skew)
    if reboot_duration_secs <= 0 {
        return;
    }

    // Extract product name and vendor from hardware info
    let product_name = machine
        .hardware_info
        .as_ref()
        .and_then(|hi| hi.dmi_data.as_ref())
        .map(|dmi| dmi.product_name.clone())
        .unwrap_or_else(|| "unknown".to_string());

    let vendor = machine
        .hardware_info
        .as_ref()
        .and_then(|hi| hi.dmi_data.as_ref())
        .map(|dmi| dmi.sys_vendor.clone())
        .unwrap_or_else(|| "unknown".to_string());

    metric_emitter.record_machine_reboot_duration(
        reboot_duration_secs as u64,
        product_name,
        vendor,
        last_reboot_requested.mode.to_string(),
    );
}

// Host has rebooted
pub(crate) async fn reboot_completed(
    api: &Api,
    request: Request<rpc::MachineRebootCompletedRequest>,
) -> Result<Response<rpc::MachineRebootCompletedResponse>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let machine_id = convert_and_log_machine_id(req.machine_id.as_ref())?;

    let (machine, mut txn) = api
        .load_machine(&machine_id, MachineSearchConfig::default())
        .await?;

    record_reboot_duration_metric(&api.metric_emitter, &machine);

    db::machine::update_reboot_time(&machine, &mut txn).await?;

    txn.commit().await?;

    // Wake up the state handler for the machine
    // Don't do it for DPUs - state handlers only run on hosts
    if (machine_id.machine_type().is_host() || machine_id.machine_type().is_predicted_host())
        && let Err(err) = api
            .machine_state_handler_enqueuer
            .enqueue_object(&machine_id)
            .await
    {
        tracing::warn!(%err, %machine_id, "Failed to wake up state handler for machine");
    }

    Ok(Response::new(rpc::MachineRebootCompletedResponse {}))
}
