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

//! The DPF operator manages all provisioning logic. Carbide's role is:
//! 1. Declare setup (register devices + node)
//! 2. Wait for watcher callbacks (DPU ready, reboot required)
//! 3. Handle cleanup on error/reprovisioning

use std::collections::HashSet;
use std::net::IpAddr;

use carbide_dpf::{DpfError, DpuDeploymentType, DpuPhase, dpu_node_cr_name};
use carbide_libmlx_model::nvconfig::DpuNvConfigProfile;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSubtypeTrait};
use libredfish::SystemPowerControl;
use model::hardware_info::HardwareInfo;
use model::machine::{
    DpfState, DpuInitState, DpuMachine, DpuReprovisionStates, FailureCause, FailureDetails,
    FailureSource, InstanceState, Machine, ManagedHostState, ManagedHostStateSnapshot,
    PerformPowerOperation, ReprovisionState, StateMachineArea,
};
use model::rack_type::{RackProductFamily, select_dpu_nvconfig_profile};
use state_controller::state_handler::{
    ExternalServiceError, StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::helpers::{DpuInitStateHelper, ManagedHostStateHelper, ReprovisionStateHelper};
use super::{handler_host_power_control, host_power_state};
use crate::context::MachineStateHandlerContextObjects;
use crate::dpf::DpfOperations;

// `deployment_type_for_dpu_profile` currently refines exactly one deployment:
// generic BF3 becomes GB200 BF3. Only that forward migration is supported.
const DEPLOYMENT_MIGRATION_SOURCE: DpuDeploymentType = DpuDeploymentType::Bf3;
const DEPLOYMENT_MIGRATION_TARGET: DpuDeploymentType = DpuDeploymentType::Bf3Gb200;

fn dpf_error(error: DpfError) -> StateHandlerError {
    ExternalServiceError::with_source("dpf", "", error.to_string(), "dpf_error", error).into()
}

fn bmc_ip(machine: &Machine<impl MachineIdSubtypeTrait>) -> Result<IpAddr, StateHandlerError> {
    machine.status.bmc_info.ip.ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!("BMC IP is not set for machine {}", machine.id))
    })
}

// wrapper so we can get an error without copying it at every call site
fn dpf_id(machine: &Machine<impl MachineIdSubtypeTrait>) -> Result<String, StateHandlerError> {
    machine.dpf_id().ok_or_else(|| {
        StateHandlerError::InvalidState(format!("BMC MAC is not set for machine {}", machine.id))
    })
}

/// Returns the product family reported by Site Explorer, or falls back to the
/// configured rack profile when that report does not name a known family.
async fn host_product_family(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Option<RackProductFamily>, StateHandlerError> {
    let mut conn = ctx.services.db_pool.acquire().await?;

    if let Some(host_bmc_ip) = state.host_snapshot.status.bmc_info.ip {
        let endpoints =
            db::explored_endpoints::find_by_ips(conn.as_mut(), vec![host_bmc_ip]).await?;
        if let Some(product_family) = endpoints
            .first()
            .and_then(|endpoint| endpoint.report.model())
            .as_deref()
            .and_then(RackProductFamily::from_hardware_model)
        {
            return Ok(Some(product_family));
        }
    }

    let Some(rack_id) = state.host_snapshot.rack_id.as_ref() else {
        return Ok(None);
    };
    let Some(rack) = db::rack::find_by(
        conn.as_mut(),
        db::ObjectColumnFilter::One(db::rack::IdColumn, rack_id),
    )
    .await?
    .pop() else {
        return Ok(None);
    };
    let Some(profile_id) = rack.rack_profile_id.as_ref() else {
        return Ok(None);
    };
    let Some(profile) = ctx
        .services
        .site_config
        .rack_profiles
        .get(profile_id.as_str())
    else {
        return Ok(None);
    };

    Ok(profile.product_family.clone())
}

async fn deployment_types_for_host(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: &dyn DpfOperations,
    astra_nics: bool,
) -> Result<Vec<DpuDeploymentType>, StateHandlerError> {
    let product_family = host_product_family(state, ctx).await?;
    state
        .dpu_snapshots
        .iter()
        .map(|dpu| {
            let base = dpf_sdk
                .deployment_type_for_dpu(dpu, astra_nics)
                .map_err(dpf_error)?;
            Ok::<_, StateHandlerError>(deployment_type_for_dpu_profile(
                base,
                product_family.as_ref(),
                dpu.status.hardware_info.as_ref(),
            ))
        })
        .collect()
}

fn deployment_type_for_dpu_profile(
    base: DpuDeploymentType,
    product_family: Option<&RackProductFamily>,
    hardware_info: Option<&HardwareInfo>,
) -> DpuDeploymentType {
    match select_dpu_nvconfig_profile(product_family, hardware_info) {
        Some(DpuNvConfigProfile::Gb200B3240V1) if base == DpuDeploymentType::Bf3 => {
            DpuDeploymentType::Bf3Gb200
        }
        Some(DpuNvConfigProfile::Gb200B3240V1) | None => base,
    }
}

fn consistent_deployment_type(
    deployment_types: &[DpuDeploymentType],
) -> Result<DpuDeploymentType, String> {
    let Some(first) = deployment_types.first().copied() else {
        return Err(
            "cannot determine DPF deployment type for a host without attached DPUs".to_string(),
        );
    };
    if deployment_types.iter().all(|candidate| *candidate == first) {
        Ok(first)
    } else {
        Err("host has mixed DPF deployment types across attached DPUs".to_string())
    }
}

/// Returns whether any attached DPU reprovision request has started.
fn any_dpu_reprovision_request_has_started(state: &ManagedHostStateSnapshot) -> bool {
    state.dpu_snapshots.iter().any(|dpu| {
        dpu.reprovision_requested
            .as_ref()
            .is_some_and(|request| request.started_at.is_some())
    })
}

/// Returns whether every attached DPU has a started reprovision request.
fn all_dpu_reprovision_requests_have_started(state: &ManagedHostStateSnapshot) -> bool {
    state.dpu_snapshots.iter().all(|dpu| {
        dpu.reprovision_requested
            .as_ref()
            .is_some_and(|request| request.started_at.is_some())
    })
}

/// Returns whether inventory, snapshots, and reprovision state describe the
/// same complete, nonempty set of attached DPUs.
fn deployment_migration_has_complete_dpu_set(
    state: &ManagedHostStateSnapshot,
    dpu_states: &DpuReprovisionStates,
) -> bool {
    let expected_dpu_ids = state
        .host_snapshot
        .associated_dpu_machine_ids()
        .into_iter()
        .collect::<HashSet<_>>();
    let snapshot_dpu_ids = state
        .dpu_snapshots
        .iter()
        .map(|dpu| dpu.id)
        .collect::<HashSet<_>>();
    let state_dpu_ids = dpu_states.states.keys().copied().collect::<HashSet<_>>();

    !expected_dpu_ids.is_empty()
        && expected_dpu_ids == snapshot_dpu_ids
        && expected_dpu_ids == state_dpu_ids
}

/// Returns whether a DPF-managed host may have a parked migration.
///
/// The migration handler validates the full DPU set before changing external
/// resources. Keeping this detector broad turns damaged parked state into a
/// visible failure instead of leaving every DPU idle indefinitely.
pub(super) fn deployment_migration_is_parked(state: &ManagedHostStateSnapshot) -> bool {
    if !state.host_snapshot.config.dpf.used_for_ingestion {
        return false;
    }

    let Some(dpu_states) = state.managed_state.dpu_reprovision_states() else {
        return false;
    };
    !dpu_states.states.is_empty()
        && dpu_states
            .states
            .values()
            .all(|dpu_state| matches!(dpu_state, ReprovisionState::NotUnderReprovision))
        && any_dpu_reprovision_request_has_started(state)
}

/// Returns whether every attached DPU has reached the migration handoff point.
fn deployment_migration_can_start(state: &ManagedHostStateSnapshot) -> bool {
    let Some(dpu_states) = state.managed_state.dpu_reprovision_states() else {
        return false;
    };

    deployment_migration_has_complete_dpu_set(state, dpu_states)
        && all_dpu_reprovision_requests_have_started(state)
        && dpu_states.states.values().all(|dpu_state| {
            matches!(
                dpu_state,
                ReprovisionState::DpfStates {
                    substate: DpfState::Reprovisioning
                }
            )
        })
}

/// Parks every attached DPU before changing its shared DPUNode selector.
fn park_for_deployment_migration(
    state: &ManagedHostStateSnapshot,
) -> Result<ManagedHostState, StateHandlerError> {
    // Reuse `NotUnderReprovision` so controllers from the same rollout can
    // deserialize the state and leave it alone. Persisting it before the
    // selector change also keeps retries out of ordinary handling for one DPU.
    ReprovisionState::NotUnderReprovision.next_state_with_all_dpus_updated(
        &state.managed_state,
        &state.dpu_snapshots,
        Vec::new(),
    )
}

/// Returns the deterministic DPU names for every attached DPU snapshot.
fn dpu_device_names(state: &ManagedHostStateSnapshot) -> Result<Vec<String>, StateHandlerError> {
    state
        .dpu_snapshots
        .iter()
        .map(dpf_id)
        .collect::<Result<Vec<_>, _>>()
}

/// Transition all DPU sub-states to the given DPF state, preserving the
/// outer managed-host state (`DPUInit` or `DPUReprovision`).
fn transition_all_dpus_to_dpf_state(
    next_dpf: DpfState,
    state: &ManagedHostStateSnapshot,
) -> Result<ManagedHostState, StateHandlerError> {
    match &state.managed_state {
        ManagedHostState::DPUInit { .. } | ManagedHostState::DpuDiscoveringState { .. } => {
            DpuInitState::DpfStates { state: next_dpf }
                .next_state_with_all_dpus_updated(&state.managed_state)
        }
        ManagedHostState::DPUReprovision { .. }
        | ManagedHostState::Assigned {
            instance_state: InstanceState::DPUReprovision { .. },
        } => {
            let all_dpu_ids = state
                .dpu_snapshots
                .iter()
                .map(|dpu| dpu.id)
                .collect::<Vec<_>>();
            ReprovisionState::DpfStates { substate: next_dpf }.next_state_with_all_dpus_updated(
                &state.managed_state,
                &state.dpu_snapshots,
                all_dpu_ids,
            )
        }
        other => Err(StateHandlerError::InvalidState(format!(
            "Cannot transition DPF sub-states in {other:?}"
        ))),
    }
}

/// Update a single DPU's DPF sub-state. All other DPUs are unchanged.
/// Use when persisting a phase change or moving one DPU to the next DpfState.
fn set_one_dpu_dpf_state(
    state: &ManagedHostStateSnapshot,
    dpu_id: &DpuMachineId,
    next_dpf: DpfState,
) -> Result<ManagedHostState, StateHandlerError> {
    let mut next_state = state.managed_state.clone();
    match &mut next_state {
        ManagedHostState::DPUInit { dpu_states } => {
            dpu_states
                .states
                .insert(*dpu_id, DpuInitState::DpfStates { state: next_dpf });
        }
        ManagedHostState::DPUReprovision { dpu_states } => {
            dpu_states
                .states
                .insert(*dpu_id, ReprovisionState::DpfStates { substate: next_dpf });
        }
        ManagedHostState::Assigned {
            instance_state: InstanceState::DPUReprovision { dpu_states },
        } => {
            dpu_states
                .states
                .insert(*dpu_id, ReprovisionState::DpfStates { substate: next_dpf });
        }
        other => {
            return Err(StateHandlerError::InvalidState(format!(
                "Cannot set DPF state for one DPU in {other:?}"
            )));
        }
    }
    Ok(next_state)
}

/// If the DPU phase reported by the DPF operator changed since last
/// persisted, return a `Transition` that writes the new phase string.
/// Otherwise return a `Wait` with the given reason.
fn update_phase_detail_or_wait(
    state: &ManagedHostStateSnapshot,
    dpu_id: &DpuMachineId,
    stored_phase_detail: &Option<String>,
    current_phase: &carbide_dpf::DpuPhase,
    wait_reason: &str,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    // if we're no longer in provisioning, there's no need to update the phase detail.
    // the phase detail will be dropped when we move from WaitingForReady to another state.
    if let DpuPhase::Provisioning(phase_detail) = current_phase
        && stored_phase_detail.as_ref() != Some(phase_detail)
    {
        let updated = set_one_dpu_dpf_state(
            state,
            dpu_id,
            DpfState::WaitingForReady {
                phase_detail: Some(phase_detail.clone()),
            },
        )?;
        return Ok(StateHandlerOutcome::transition(updated));
    }
    Ok(StateHandlerOutcome::wait(wait_reason.to_string()))
}

/// Determine the correct next state when exiting `DeviceReady`, based on
/// whether we are in initial provisioning (`DPUInit` -> `WaitingForPlatformConfiguration`)
/// or reprovisioning (`DPUReprovision`).
fn waiting_for_ready_exit_state(
    state: &ManagedHostStateSnapshot,
) -> Result<ManagedHostState, StateHandlerError> {
    match &state.managed_state {
        ManagedHostState::DPUInit { .. } | ManagedHostState::DpuDiscoveringState { .. } => {
            DpuInitState::WaitingForPlatformConfiguration
                .next_state_with_all_dpus_updated(&state.managed_state)
        }
        ManagedHostState::DPUReprovision { .. }
        | ManagedHostState::Assigned {
            instance_state: InstanceState::DPUReprovision { .. },
        } => {
            let all_dpu_ids = state
                .dpu_snapshots
                .iter()
                .map(|dpu| dpu.id)
                .collect::<Vec<_>>();
            ReprovisionState::WaitingForNetworkConfig.next_state_with_all_dpus_updated(
                &state.managed_state,
                &state.dpu_snapshots,
                all_dpu_ids,
            )
        }
        other => Err(StateHandlerError::InvalidState(format!(
            "Cannot exit DPF WaitingForReady in {other:?}"
        ))),
    }
}

async fn create_and_register_dpudevices_and_dpunode(
    state: &ManagedHostStateSnapshot,
    dpf_sdk: &dyn DpfOperations,
    deployment_type: DpuDeploymentType,
) -> Result<(), StateHandlerError> {
    let primary_dpu_id = state
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|iface| iface.primary_interface)
        .and_then(|iface| iface.attached_dpu_machine_id)
        .ok_or_else(|| StateHandlerError::MissingData {
            object_id: state.host_snapshot.id.to_string(),
            missing: "primary_dpu",
        })?;

    // Currently, we don't have dual DPU systems with Astra NICs
    // attached to them.
    let astra_nics = state.astra_nics();
    let astra_underlay_nics =
        (deployment_type == DpuDeploymentType::Bf4Astra).then(|| astra_nics.clone());
    if state.dpu_snapshots.len() > 1 && !astra_nics.is_empty() {
        return Err(StateHandlerError::InvalidState(format!(
            "dual DPU systems with Astra NICs are not supported (host {})",
            state.host_snapshot.id
        )));
    }

    tracing::info!(host = %state.host_snapshot.id, num_astra_nics = %astra_nics.len(), "Astra NICs");

    if !state
        .dpu_snapshots
        .iter()
        .any(|dpu| dpu.id == primary_dpu_id)
    {
        return Err(StateHandlerError::MissingData {
            object_id: state.host_snapshot.id.to_string(),
            missing: "primary_dpu_snapshot",
        });
    }

    for dpu in &state.dpu_snapshots {
        let serial_number = dpu
            .status
            .hardware_info
            .as_ref()
            .and_then(|x| x.dmi_data.as_ref())
            .map(|x| x.product_serial.as_str())
            .unwrap_or_default();
        if serial_number.is_empty() {
            tracing::warn!(
                dpu_machine_id = %dpu.id,
                host_machine_id = %state.host_snapshot.id,
                "DPU product serial is missing; registering DPU device with DPF using an empty serial"
            );
        }
        let device_info = carbide_dpf::DpuDeviceInfo {
            device_id: dpf_id(dpu)?,
            dpu_bmc_ip: bmc_ip(dpu)?,
            host_bmc_ip: bmc_ip(&state.host_snapshot)?,
            serial_number: serial_number.to_string(),
            dpu_machine_id: dpu.id.to_string(),
            is_primary: dpu.id == primary_dpu_id,
        };
        dpf_sdk
            .register_dpu_device(device_info, astra_underlay_nics.clone())
            .await
            .map_err(dpf_error)?;
    }

    let device_ids: Vec<String> = state
        .dpu_snapshots
        .iter()
        .map(dpf_id)
        .collect::<Result<_, _>>()?;
    let node_info = carbide_dpf::DpuNodeInfo {
        node_id: dpf_id(&state.host_snapshot)?,
        host_bmc_ip: bmc_ip(&state.host_snapshot)?,
        device_ids,
        deployment_type,
    };
    dpf_sdk
        .register_dpu_node(node_info)
        .await
        .map_err(dpf_error)?;

    Ok(())
}

/// Build the correct failure state depending on whether the host is currently
/// `Assigned` (DPU reprovision path). When `Assigned`, we preserve the outer
/// state and embed the failure as `InstanceState::Failed`; otherwise we use
/// the top-level `ManagedHostState::Failed`.
fn make_failure_state(
    state: &ManagedHostStateSnapshot,
    details: FailureDetails,
    machine_id: MachineId,
) -> ManagedHostState {
    if matches!(state.managed_state, ManagedHostState::Assigned { .. }) {
        ManagedHostState::Assigned {
            instance_state: InstanceState::Failed {
                details,
                machine_id,
            },
        }
    } else {
        ManagedHostState::Failed {
            details,
            machine_id,
            retry_count: 0,
        }
    }
}

fn dpf_cr_creation_failed(
    state: &ManagedHostStateSnapshot,
    err: &StateHandlerError,
) -> StateHandlerOutcome<ManagedHostState> {
    let details = FailureDetails {
        cause: FailureCause::DpfProvisioning {
            err: format!(
                "DPUDevice/DPUNode creation failed. Force-delete/restart reprovisioning (reprovisioning case) to clean old values. Wait until DPU CR are deleted. {err}"
            ),
        },
        failed_at: chrono::Utc::now(),
        source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
    };
    StateHandlerOutcome::transition(make_failure_state(
        state,
        details,
        state.host_snapshot.id.into(),
    ))
}

fn dpf_deployment_selection_failed(
    state: &ManagedHostStateSnapshot,
    error: &str,
) -> StateHandlerOutcome<ManagedHostState> {
    let details = FailureDetails {
        cause: FailureCause::DpfProvisioning {
            err: format!("DPF deployment selection failed: {error}"),
        },
        failed_at: chrono::Utc::now(),
        source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
    };
    StateHandlerOutcome::transition(make_failure_state(
        state,
        details,
        state.host_snapshot.id.into(),
    ))
}

/// Builds the terminal failure used when a deployment was selected but the
/// host does not meet a migration precondition.
fn dpf_deployment_migration_failed(
    state: &ManagedHostStateSnapshot,
    error: &str,
) -> StateHandlerOutcome<ManagedHostState> {
    let details = FailureDetails {
        cause: FailureCause::DpfProvisioning {
            err: format!(
                "DPF deployment migration failed: {error}. Resolve the reported precondition \
                 before retrying DPU reprovisioning"
            ),
        },
        failed_at: chrono::Utc::now(),
        source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
    };
    StateHandlerOutcome::transition(make_failure_state(
        state,
        details,
        state.host_snapshot.id.into(),
    ))
}

/// Handle DpfState::Provisioning: register all DPU devices and the node, then
/// transition all DPUs to WaitingForReady.
async fn handle_dpf_provisioning(
    state: &ManagedHostStateSnapshot,
    dpf_sdk: &dyn DpfOperations,
    deployment_type: DpuDeploymentType,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    if let Err(err) =
        create_and_register_dpudevices_and_dpunode(state, dpf_sdk, deployment_type).await
    {
        return Ok(dpf_cr_creation_failed(state, &err));
    }

    let next =
        transition_all_dpus_to_dpf_state(DpfState::WaitingForReady { phase_detail: None }, state)?;
    Ok(StateHandlerOutcome::transition(next))
}

/// Handle `DpfState::HandleReboot`: drive a DPF-requested power cycle using
/// explicit per-step state rather than timestamp heuristics.
///
/// Transitions:
/// - `Off`: wait for `power_state == Off` + delay → persist
///   `HandleReboot { On, 0 }`
/// - `On`: wait for `power_state == On`  + delay → `reboot_complete` →
///   `WaitingForReady`
async fn handle_dpf_handle_reboot(
    state: &ManagedHostStateSnapshot,
    op: &PerformPowerOperation,
    retry_count: u32,
    node_name: &str,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: &dyn DpfOperations,
    power_down_wait: chrono::Duration,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    const MAX_RETRIES: u32 = 3;

    // Due to enqueue based event handling, On is triggered immedietely after Off while chassis is
    // not able to process Off completely. So this time delay is needed.
    if super::wait(
        &state.host_snapshot.state.version.timestamp(),
        power_down_wait,
    ) {
        return Ok(StateHandlerOutcome::wait(
            "waiting for power transition delay".into(),
        ));
    }

    let power_state = {
        let redfish_client = ctx
            .services
            .create_redfish_client_from_machine(&state.host_snapshot)
            .await?;
        host_power_state(redfish_client.as_ref()).await?
    };

    match op {
        PerformPowerOperation::Off => {
            if power_state == libredfish::PowerState::Off {
                let next = transition_all_dpus_to_dpf_state(
                    DpfState::HandleReboot {
                        op: PerformPowerOperation::On,
                        retry_count: 0,
                    },
                    state,
                )?;
                Ok(StateHandlerOutcome::transition(next))
            } else if retry_count < MAX_RETRIES {
                tracing::warn!(
                    host = %state.host_snapshot.id,
                    retry_count,
                    "Host did not power off; retrying ForceOff"
                );
                handler_host_power_control(state, ctx, SystemPowerControl::ForceOff).await?;
                let next = transition_all_dpus_to_dpf_state(
                    DpfState::HandleReboot {
                        op: PerformPowerOperation::Off,
                        retry_count: retry_count + 1,
                    },
                    state,
                )?;
                Ok(StateHandlerOutcome::transition(next))
            } else {
                tracing::error!(
                    host = %state.host_snapshot.id,
                    max_retries = MAX_RETRIES,
                    "Host did not power off after max retries; manual intervention required"
                );
                Err(StateHandlerError::ManualInterventionRequired(
                    "host did not power off; manual intervention required".into(),
                ))
            }
        }
        PerformPowerOperation::On => {
            if power_state == libredfish::PowerState::On {
                dpf_sdk
                    .reboot_complete(node_name)
                    .await
                    .map_err(dpf_error)?;
                let next = transition_all_dpus_to_dpf_state(
                    DpfState::WaitingForReady { phase_detail: None },
                    state,
                )?;
                Ok(StateHandlerOutcome::transition(next))
            } else if retry_count <= MAX_RETRIES {
                // Zero means the On intent is durable but its initial command
                // has not yet been issued. Later attempts are retries.
                if retry_count > 0 {
                    tracing::warn!(
                        host = %state.host_snapshot.id,
                        retry_count,
                        "Host did not power on; retrying On"
                    );
                }
                handler_host_power_control(state, ctx, SystemPowerControl::On).await?;
                let next = transition_all_dpus_to_dpf_state(
                    DpfState::HandleReboot {
                        op: PerformPowerOperation::On,
                        retry_count: retry_count + 1,
                    },
                    state,
                )?;
                Ok(StateHandlerOutcome::transition(next))
            } else {
                tracing::error!(
                    host = %state.host_snapshot.id,
                    max_retries = MAX_RETRIES,
                    "Host did not power on after max retries; manual intervention required"
                );
                Err(StateHandlerError::ManualInterventionRequired(
                    "host did not power on; manual intervention required".into(),
                ))
            }
        }
    }
}

/// Handle DpfState::WaitingForReady: release hold, reboot handling,
/// phase/error checks, and per-DPU transition to DeviceReady.
async fn handle_dpf_waiting_for_ready(
    state: &ManagedHostStateSnapshot,
    dpu_snapshot: &DpuMachine,
    waiting_phase_detail: &Option<String>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: &dyn DpfOperations,
    deployment_type: DpuDeploymentType,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let dpu_machine_id = dpu_snapshot.id;
    let node_name = dpu_node_cr_name(&dpf_id(&state.host_snapshot)?);
    let dpu_device_name = dpf_id(dpu_snapshot)?;
    // During a deployment migration the source and target DPUSet reuse the
    // deterministic DPU name. Read ownership, phase, and Ready conformance from
    // one observation scoped to the target so an old source DPU cannot release
    // the hold, trigger a reboot, or satisfy target readiness.
    let current_phase = match if deployment_type == DEPLOYMENT_MIGRATION_TARGET {
        let dpu_device_names = dpu_device_names(state)?;
        dpf_sdk
            .get_dpu_phases_for_deployment_type(&dpu_device_names, &node_name, deployment_type)
            .await
            .map(|phases| phases.and_then(|phases| phases.get(&dpu_device_name).cloned()))
    } else {
        dpf_sdk
            .get_dpu_phase(&dpu_device_name, &node_name)
            .await
            .map(Some)
    } {
        Ok(Some(phase)) => phase,
        Ok(None) => {
            return Ok(StateHandlerOutcome::wait(
                "Waiting for DPF to recreate the DPU from the GB200 deployment".to_string(),
            ));
        }
        // The operator briefly removes the old DPU CR before recreating a fresh one
        // during reprovision; treat that window as "keep waiting" rather than erroring.
        Err(DpfError::NotFound { .. }) => {
            return Ok(StateHandlerOutcome::wait(
                "DPU CR not yet recreated after reprovision".to_string(),
            ));
        }
        Err(DpfError::InvalidState(error)) if deployment_type == DEPLOYMENT_MIGRATION_TARGET => {
            return Ok(dpf_deployment_migration_failed(state, &error));
        }
        Err(err) => return Err(dpf_error(err)),
    };

    // A DPU with a deletionTimestamp is a terminating old CR (get_dpu_phase maps that to
    // Deleting; its status.phase is still stale, often Ready or Error). Do nothing until
    // the operator has deleted it and created the fresh CR: don't release the maintenance
    // hold, don't trigger a reboot, and don't read Ready/Error off it. This is what lets a
    // reprovision of an *errored* DPU proceed instead of immediately re-failing on the old
    // CR's stale Error phase.
    if current_phase == DpuPhase::Deleting {
        return Ok(StateHandlerOutcome::wait(
            "DPU CR is being deleted (reprovision in progress); waiting for the new CR".to_string(),
        ));
    }

    dpf_sdk
        .release_maintenance_hold(&node_name)
        .await
        .map_err(dpf_error)?;

    // The watcher records a pending service sync for any DPU it sees in the
    // NodeEffect phase, because the phase does not say why the DPU is parked --
    // a provisioning pass produces one just as a DPUService change does. This
    // release satisfies whichever it was, so retire the marker here rather than
    // leave the Ready handler to rediscover it and call Kubernetes for work that
    // has already happened.
    //
    // Bookkeeping only: a failure here costs a redundant check later, so it must
    // not fail provisioning.
    if let Err(error) =
        super::dpu_action_handler::complete_pending_sync(ctx, &state.host_snapshot).await
    {
        tracing::warn!(
            machine_id = %state.host_snapshot.id,
            node = %node_name,
            %error,
            "Could not retire the host's pending DPU service sync after releasing its hold"
        );
    }

    if dpf_sdk
        .is_reboot_required(&node_name)
        .await
        .map_err(dpf_error)?
    {
        handler_host_power_control(state, ctx, SystemPowerControl::ForceOff).await?;
        let next = transition_all_dpus_to_dpf_state(
            DpfState::HandleReboot {
                op: PerformPowerOperation::Off,
                retry_count: 0,
            },
            state,
        )?;
        return Ok(StateHandlerOutcome::transition(next));
    }

    if current_phase == carbide_dpf::DpuPhase::Error {
        tracing::error!(
            machine_id = %state.host_snapshot.id,
            dpu_machine_id = %dpu_snapshot.id,
            "DPU entered error phase during DPF provisioning"
        );
        let details = FailureDetails {
            cause: FailureCause::DpfProvisioning {
                err: format!(
                    "DPU {} entered error phase during DPF provisioning",
                    dpu_snapshot.id
                ),
            },
            failed_at: chrono::Utc::now(),
            source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
        };
        return Ok(StateHandlerOutcome::transition(make_failure_state(
            state,
            details,
            dpu_snapshot.id.into(),
        )));
    }
    // wait for dpf to report that the dpu is ready
    if current_phase != carbide_dpf::DpuPhase::Ready {
        return update_phase_detail_or_wait(
            state,
            &dpu_machine_id,
            waiting_phase_detail,
            &current_phase,
            "Waiting for DPU to reach Ready phase",
        );
    }

    let next = set_one_dpu_dpf_state(state, &dpu_machine_id, DpfState::DeviceReady)?;
    Ok(StateHandlerOutcome::transition(next))
}

/// Handle DpfState::DeviceReady: wait for all DPUs to sync, then
/// transition to the next state.
fn handle_dpf_device_ready(
    state: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    if !state.managed_state.all_dpu_states_in_sync()? {
        return Ok(StateHandlerOutcome::wait(
            "Waiting for all DPUs to reach DeviceReady".to_string(),
        ));
    }

    let next = waiting_for_ready_exit_state(state)?;
    Ok(StateHandlerOutcome::transition(next))
}

/// Moves one host's existing DPF resources from generic BF3 to the GB200
/// deployment during an active DPU reprovision request.
///
/// The DPUNode and initialized DPUDevices stay in place. Parking every DPU in
/// `NotUnderReprovision` makes the label transfer and DPU deletions retryable
/// when the process restarts before or after the selector changes.
pub(super) async fn handle_dpf_deployment_migration(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: &dyn DpfOperations,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let Some(dpu_states) = state.managed_state.dpu_reprovision_states() else {
        return Ok(dpf_deployment_migration_failed(
            state,
            "parked state does not contain DPU reprovision state",
        ));
    };
    if !deployment_migration_has_complete_dpu_set(state, dpu_states)
        || !all_dpu_reprovision_requests_have_started(state)
    {
        return Ok(dpf_deployment_migration_failed(
            state,
            "parked state does not contain a started request for every attached DPU",
        ));
    }

    let astra_nics = machine_has_astra_nics(state, ctx).await?;
    let deployment_types = deployment_types_for_host(state, ctx, dpf_sdk, astra_nics).await?;
    let desired_deployment = match consistent_deployment_type(&deployment_types) {
        Ok(deployment_type) => deployment_type,
        Err(error) => return Ok(dpf_deployment_selection_failed(state, error.as_str())),
    };
    if desired_deployment != DEPLOYMENT_MIGRATION_TARGET {
        let error = format!(
            "parked DPF deployment migration now selects {desired_deployment:?}, expected \
             {DEPLOYMENT_MIGRATION_TARGET:?}"
        );
        return Ok(dpf_deployment_migration_failed(state, &error));
    }

    let node_name = dpu_node_cr_name(&dpf_id(&state.host_snapshot)?);
    let dpu_device_names = dpu_device_names(state)?;

    dpf_sdk
        .transfer_dpu_node_deployment_labels(
            &node_name,
            DEPLOYMENT_MIGRATION_SOURCE,
            DEPLOYMENT_MIGRATION_TARGET,
        )
        .await
        .map_err(dpf_error)?;

    dpf_sdk
        .delete_source_dpus_for_deployment_migration(
            &dpu_device_names,
            &node_name,
            DEPLOYMENT_MIGRATION_SOURCE,
            DEPLOYMENT_MIGRATION_TARGET,
        )
        .await
        .map_err(dpf_error)?;

    // Keep the durable parked state until one DPF observation contains every
    // target DPU. Controllers from before migration support ignore this state,
    // so they cannot accept a source DPU recreated during the selector handoff.
    match dpf_sdk
        .get_dpu_phases_for_deployment_type(
            &dpu_device_names,
            &node_name,
            DEPLOYMENT_MIGRATION_TARGET,
        )
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) | Err(DpfError::NotFound { .. }) => {
            return Ok(StateHandlerOutcome::wait(
                "Waiting for DPF to recreate every DPU from the GB200 deployment".to_string(),
            ));
        }
        Err(DpfError::InvalidState(error)) => {
            return Ok(dpf_deployment_migration_failed(state, &error));
        }
        Err(error) => return Err(dpf_error(error)),
    }

    let next =
        transition_all_dpus_to_dpf_state(DpfState::WaitingForReady { phase_detail: None }, state)?;
    Ok(StateHandlerOutcome::transition(next))
}

/// Handle DpfState::Reprovisioning
/// If the DPUNode and DPUDevice CRs do not exist, then create them
/// and transition to the next state to reprovision all DPUs to DPF.
/// Else handle the reprovisioning of a single DPU
async fn handle_dpf_reprovisioning(
    state: &ManagedHostStateSnapshot,
    dpu_snapshot: &DpuMachine,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: &dyn DpfOperations,
    deployment_type: DpuDeploymentType,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let dpu_machine_id = dpu_snapshot.id;
    let node_name = dpu_node_cr_name(&dpf_id(&state.host_snapshot)?);
    let dpf_dpudevices_and_dpunode_crs_noexist =
        crate::dpf::dpf_dpudevices_and_dpunode_crs_noexist(state, dpf_sdk)
            .await
            .map_err(dpf_error)?;
    if dpf_dpudevices_and_dpunode_crs_noexist {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "DPUDevice/DPUNode CRs do not exist, creating them before reprovisioning"
        );
        if let Err(err) =
            create_and_register_dpudevices_and_dpunode(state, dpf_sdk, deployment_type).await
        {
            return Ok(dpf_cr_creation_failed(state, &err));
        }
        let next = transition_all_dpus_to_dpf_state(
            DpfState::WaitingForReady { phase_detail: None },
            state,
        )?;

        let outcome = StateHandlerOutcome::transition(next);
        let mut txn = ctx.services.db_pool.begin().await?;
        db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &state.host_snapshot.id)
            .await?;
        return Ok(outcome.with_txn(txn));
    }

    tracing::info!(machine_id = %dpu_snapshot.id, "DPF initiate reprovision of DPU");
    dpf_sdk
        .reprovision_dpu(&dpf_id(dpu_snapshot)?, &node_name)
        .await
        .map_err(dpf_error)?;
    let next = set_one_dpu_dpf_state(
        state,
        &dpu_machine_id,
        DpfState::WaitingForReady { phase_detail: None },
    )?;
    Ok(StateHandlerOutcome::transition(next))
}

// Early in machine ingestion, the dpa_intetrfaces objects for the host are not populated.
// So we will have to check the expected_machine table for the given host to see if it has
// any NICs of type CX9. If so, return true. Otherwise, return false.
async fn machine_has_astra_nics(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<bool, StateHandlerError> {
    // its unlikely we got here without a bmc mac
    let Some(bmc_mac_address) = state.host_snapshot.status.bmc_info.mac else {
        tracing::error!(
            machine_id = %state.host_snapshot.id,
            "machine_has_astra_nics: No BMC MAC address configured"
        );
        return Err(StateHandlerError::MissingData {
            object_id: state.host_snapshot.id.to_string(),
            missing: "bmc_mac_address",
        });
    };

    let mut txn = ctx.services.db_pool.begin().await?;

    // Retrieve the expected_machines table entry for this managed host.
    let expected_machine = db::expected_machine::find_by_bmc_mac_address(
        txn.as_mut(),
        bmc_mac_address,
    )
    .await
    .map_err(|err| {
        tracing::error!(
            machine_id = %state.host_snapshot.id,
            %bmc_mac_address,
            error = %err,
            "machine_has_astra_nics: Failed to look up expected machine for Astra enablement"
        );
        StateHandlerError::DBError(Box::new(err))
    })?;

    txn.commit().await?;

    // No expected-machine entry means there are no declared host NICs to act on.
    let Some(expected_machine) = expected_machine else {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "machine_has_astra_nics: No expected-machine entry found"
        );
        return Ok(false);
    };

    let host_nics = expected_machine.data.interfaces;
    if host_nics.is_empty() {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            "machine_has_astra_nics: No host NICs found"
        );
        return Ok(false);
    }

    // At this point, we need to use Redfish to get all the CX cards in the host.
    // The end point to explore is /redfish/v1/Chassis/CX_$i

    let has_cx9 = host_nics
        .iter()
        .any(|nic| nic.nic_type.as_deref() == Some("CX9"));

    Ok(has_cx9)
}

/// Handle DPF state transitions.
///
/// Provisioning registers all DPUs at once and moves them to WaitingForReady
/// together. All other states (Reprovisioning, WaitingForReady, DeviceReady)
/// advance the given `dpu_snapshot` independently. DeviceReady acts as a sync
/// barrier that waits for all DPUs before proceeding.
pub(super) async fn handle_dpf_state(
    state: &ManagedHostStateSnapshot,
    dpu_snapshot: &DpuMachine,
    dpf_state: &DpfState,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: &dyn DpfOperations,
    power_down_wait: chrono::Duration,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let dpu_machine_id = dpu_snapshot.id;
    let node_name = dpu_node_cr_name(&dpf_id(&state.host_snapshot)?);

    let astra_nics = machine_has_astra_nics(state, ctx).await?;

    let deployment_types = deployment_types_for_host(state, ctx, dpf_sdk, astra_nics).await?;
    let mut deployment_type = match consistent_deployment_type(&deployment_types) {
        Ok(deployment_type) => deployment_type,
        Err(error) => {
            let selections = state
                .dpu_snapshots
                .iter()
                .zip(&deployment_types)
                .map(|(dpu, deployment_type)| format!("{}={deployment_type:?}", dpu.id))
                .collect::<Vec<_>>()
                .join(", ");
            let error = format!("{error}: {selections}");

            return Ok(dpf_deployment_selection_failed(state, error.as_str()));
        }
    };
    let node_has_desired_labels = dpf_sdk
        .verify_node_labels(&node_name, deployment_type)
        .await
        .map_err(dpf_error)?;
    if !node_has_desired_labels {
        let node_has_migration_source_labels = if deployment_type == DEPLOYMENT_MIGRATION_TARGET {
            dpf_sdk
                .verify_node_labels(&node_name, DEPLOYMENT_MIGRATION_SOURCE)
                .await
                .map_err(dpf_error)?
        } else {
            false
        };

        if node_has_migration_source_labels
            && matches!(dpf_state, DpfState::Reprovisioning)
            && deployment_migration_can_start(state)
        {
            tracing::info!(
                machine_id = %state.host_snapshot.id,
                node = %node_name,
                from = ?DEPLOYMENT_MIGRATION_SOURCE,
                to = ?deployment_type,
                "parking DPF deployment selector migration"
            );
            let next = park_for_deployment_migration(state)?;
            return Ok(StateHandlerOutcome::transition(next));
        } else if node_has_migration_source_labels && any_dpu_reprovision_request_has_started(state)
        {
            // A controller from before deployment migration support may have
            // started only part of the DPU set or advanced one DPU before the
            // rollout completed. Finish that work under its current deployment;
            // a later host request can migrate the complete DPU set.
            tracing::warn!(
                machine_id = %state.host_snapshot.id,
                node = %node_name,
                from = ?DEPLOYMENT_MIGRATION_SOURCE,
                to = ?deployment_type,
                "deferring DPF deployment migration for work already in progress"
            );
            deployment_type = DEPLOYMENT_MIGRATION_SOURCE;
        } else {
            tracing::error!(
                machine_id = %state.host_snapshot.id,
                node = %node_name,
                "DPUNode has stale labels, failing for reprovisioning"
            );
            let details = FailureDetails {
                cause: FailureCause::DpfProvisioning {
                    err: format!(
                        "DPUNode {node_name} has stale labels; \
                         must be deleted and reprovisioned"
                    ),
                },
                failed_at: chrono::Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
            };
            return Ok(StateHandlerOutcome::transition(make_failure_state(
                state,
                details,
                state.host_snapshot.id.into(),
            )));
        }
    }
    if matches!(dpf_state, DpfState::Provisioning) {
        tracing::info!(
            machine_id = %state.host_snapshot.id,
            ?deployment_type,
            "selected DPF deployment type for host"
        );
    }

    match dpf_state {
        DpfState::Provisioning => handle_dpf_provisioning(state, dpf_sdk, deployment_type).await,
        DpfState::WaitingForReady { phase_detail } => {
            handle_dpf_waiting_for_ready(
                state,
                dpu_snapshot,
                phase_detail,
                ctx,
                dpf_sdk,
                deployment_type,
            )
            .await
        }
        DpfState::HandleReboot { op, retry_count } => {
            handle_dpf_handle_reboot(
                state,
                op,
                *retry_count,
                &node_name,
                ctx,
                dpf_sdk,
                power_down_wait,
            )
            .await
        }
        DpfState::DeviceReady => handle_dpf_device_ready(state),
        DpfState::Reprovisioning => {
            handle_dpf_reprovisioning(state, dpu_snapshot, ctx, dpf_sdk, deployment_type).await
        }
        DpfState::Unknown => {
            tracing::warn!(dpu_machine_id = %dpu_snapshot.id, "unknown DPF state in DB, transitioning to provisioning");
            let next = set_one_dpu_dpf_state(state, &dpu_machine_id, DpfState::Provisioning)?;
            Ok(StateHandlerOutcome::transition(next))
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::hardware_info::DpuData;
    use model::machine::ReprovisionRequest;
    use model::test_support::machine_snapshot::managed_host_state_snapshot;

    use super::*;

    fn hardware_info(part_number: &str) -> HardwareInfo {
        HardwareInfo {
            dpu_info: Some(DpuData {
                part_number: part_number.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn parked_deployment_migration_state(used_for_ingestion: bool) -> ManagedHostStateSnapshot {
        let mut state = managed_host_state_snapshot();
        state.host_snapshot.config.dpf.used_for_ingestion = used_for_ingestion;
        for dpu in &mut state.dpu_snapshots {
            dpu.reprovision_requested = Some(ReprovisionRequest {
                requested_at: chrono::DateTime::UNIX_EPOCH,
                initiator: "test".to_string(),
                update_firmware: false,
                started_at: Some(chrono::DateTime::UNIX_EPOCH),
                user_approval_received: false,
                restart_reprovision_requested_at: chrono::DateTime::UNIX_EPOCH,
            });
        }
        state.managed_state = ManagedHostState::DPUReprovision {
            dpu_states: DpuReprovisionStates {
                states: state
                    .dpu_snapshots
                    .iter()
                    .map(|dpu| (dpu.id, ReprovisionState::NotUnderReprovision))
                    .collect(),
            },
        };
        state
    }

    #[test]
    fn only_dpf_managed_hosts_resume_parked_deployment_migrations() {
        check_values(
            [
                Check {
                    scenario: "DPF-managed host",
                    input: true,
                    expect: true,
                },
                Check {
                    scenario: "Non-DPF host",
                    input: false,
                    expect: false,
                },
            ],
            |used_for_ingestion| {
                deployment_migration_is_parked(&parked_deployment_migration_state(
                    used_for_ingestion,
                ))
            },
        );
    }

    #[test]
    fn gb200_b3240_profile_selects_only_the_specialized_bf3_deployment() {
        /// Hardware and rack inputs used to refine one base DPF deployment class.
        struct ProfileInput {
            base: DpuDeploymentType,
            product_family: Option<RackProductFamily>,
            hardware_info: Option<HardwareInfo>,
        }

        check_values(
            [
                Check {
                    scenario: "GB200 B3240 BF3",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf3Gb200,
                },
                Check {
                    scenario: "non-GB200 B3240 BF3",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: Some(RackProductFamily::Other("other".to_string())),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf3,
                },
                Check {
                    scenario: "GB200 other BF3",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CV-AA0")),
                    },
                    expect: DpuDeploymentType::Bf3,
                },
                Check {
                    scenario: "BF3 without rack profile or hardware information",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf3,
                        product_family: None,
                        hardware_info: None,
                    },
                    expect: DpuDeploymentType::Bf3,
                },
                Check {
                    scenario: "GB200 BF4 remains BF4",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf4Generic,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf4Generic,
                },
                Check {
                    scenario: "Astra remains Astra",
                    input: ProfileInput {
                        base: DpuDeploymentType::Bf4Astra,
                        product_family: Some(RackProductFamily::Gb200),
                        hardware_info: Some(hardware_info("900-9D3B6-00CN-AB0")),
                    },
                    expect: DpuDeploymentType::Bf4Astra,
                },
            ],
            |input| {
                deployment_type_for_dpu_profile(
                    input.base,
                    input.product_family.as_ref(),
                    input.hardware_info.as_ref(),
                )
            },
        );
    }

    #[test]
    fn attached_dpus_require_one_consistent_deployment_type() {
        check_values(
            [
                Check {
                    scenario: "ordinary BF3 pair",
                    input: vec![DpuDeploymentType::Bf3, DpuDeploymentType::Bf3],
                    expect: Some(DpuDeploymentType::Bf3),
                },
                Check {
                    scenario: "GB200 BF3 pair",
                    input: vec![DpuDeploymentType::Bf3Gb200, DpuDeploymentType::Bf3Gb200],
                    expect: Some(DpuDeploymentType::Bf3Gb200),
                },
                Check {
                    scenario: "mixed GB200 eligibility",
                    input: vec![DpuDeploymentType::Bf3Gb200, DpuDeploymentType::Bf3],
                    expect: None,
                },
                Check {
                    scenario: "host without attached DPUs",
                    input: vec![],
                    expect: None,
                },
            ],
            |deployment_types| consistent_deployment_type(&deployment_types).ok(),
        );
    }
}
