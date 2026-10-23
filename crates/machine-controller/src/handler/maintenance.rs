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

//! Handler for [`ManagedHostState::Maintenance`].

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, Credentials,
};
use carbide_uuid::machine::HostMachineId;
use chrono::Utc;
use component_manager::compute_tray_manager::{ComputeTrayEndpoint, ComputeTrayResult};
use db::machine as db_machine;
use mac_address::MacAddress;
use model::component_manager::PowerAction;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{
    FailureCause, FailureDetails, FailureSource, HostMachine, MachineMaintenanceOperation,
    MachineMaintenanceRequest, ManagedHostState, ManagedHostStateSnapshot, StateMachineArea,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;

/// Handles the Maintenance state for a host, dispatching on the requested
/// operation. Power-state failures retain the request for reconciliation; reset
/// dispatch failures stop because the device may already have executed the action.
pub(super) async fn handle_maintenance(
    host_machine_id: &HostMachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let ManagedHostState::Maintenance { operation, request } = &mh_snapshot.managed_state else {
        unreachable!("handle_maintenance called with non-Maintenance state");
    };
    let request = request.as_ref();

    // Replace a failed attempt between calls, without cancelling in-flight work
    // or guessing the identity of a legacy state's unrecorded request.
    if matches!(
        mh_snapshot.host_snapshot.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { .. })
    ) && let (Some(original), Some(pending)) = (
        request,
        mh_snapshot
            .host_snapshot
            .machine_maintenance_requested
            .as_ref(),
    ) && original != pending
    {
        return Ok(StateHandlerOutcome::transition(
            ManagedHostState::maintenance_for_request(pending.clone()),
        ));
    }

    match operation {
        MachineMaintenanceOperation::PowerOn => {
            handle_power_on(host_machine_id, mh_snapshot, request, ctx).await
        }
        MachineMaintenanceOperation::PowerOff { graceful } => {
            handle_power_off(host_machine_id, mh_snapshot, request, ctx, *graceful).await
        }
        MachineMaintenanceOperation::Reset => {
            handle_reset(host_machine_id, mh_snapshot, request, ctx).await
        }
        MachineMaintenanceOperation::ChassisReset { chassis_id } => {
            handle_chassis_reset(host_machine_id, mh_snapshot, request, ctx, chassis_id).await
        }
    }
}

async fn handle_power_on(
    host_machine_id: &HostMachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(machine_id = %host_machine_id, "Machine maintenance: PowerOn");
    invoke_power_operation(
        host_machine_id,
        mh_snapshot,
        request,
        ctx,
        PowerAction::On,
        "PowerOn",
    )
    .await
}

async fn handle_power_off(
    host_machine_id: &HostMachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    graceful: bool,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let action = if graceful {
        PowerAction::GracefulShutdown
    } else {
        PowerAction::ForceOff
    };
    tracing::info!(machine_id = %host_machine_id, graceful, "Machine maintenance: PowerOff");
    invoke_power_operation(
        host_machine_id,
        mh_snapshot,
        request,
        ctx,
        action,
        "PowerOff",
    )
    .await
}

async fn handle_reset(
    host_machine_id: &HostMachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(machine_id = %host_machine_id, "Machine maintenance: Reset");
    invoke_power_operation(
        host_machine_id,
        mh_snapshot,
        request,
        ctx,
        PowerAction::ForceRestart,
        "Reset",
    )
    .await
}

async fn handle_chassis_reset(
    host_machine_id: &HostMachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    chassis_id: &str,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(
        machine_id = %host_machine_id,
        %chassis_id,
        "Machine maintenance: ChassisReset",
    );

    let redfish_client = ctx
        .services
        .create_redfish_client_from_machine(&mh_snapshot.host_snapshot)
        .await?;

    if let Err(error) = redfish_client
        .chassis_reset(chassis_id, libredfish::SystemPowerControl::ForceRestart)
        .await
    {
        return finish_uncertain_reset(host_machine_id, request, ctx, error.to_string()).await;
    }

    tracing::info!(
        machine_id = %host_machine_id,
        %chassis_id,
        "Chassis reset request accepted; verify recovery before clearing operator maintenance",
    );
    finish_maintenance(host_machine_id, request, ctx, ManagedHostState::Ready).await
}

/// Common driver for component-manager-backed power maintenance operations.
async fn invoke_power_operation(
    host_machine_id: &HostMachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    action: PowerAction,
    operation_label: &'static str,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let machine = &mh_snapshot.host_snapshot;

    let component_manager = ctx.services.component_manager.as_ref().ok_or_else(|| {
        StateHandlerError::GenericError(eyre::eyre!(
            "Machine {host_machine_id} maintenance ({operation_label}): component manager not configured"
        ))
    })?;

    let endpoint = build_compute_tray_endpoint(
        host_machine_id,
        machine,
        ctx.services.credential_manager.as_ref(),
    )
    .await
    .map_err(|cause| {
        StateHandlerError::GenericError(eyre::eyre!(
            "Machine {host_machine_id} maintenance ({operation_label}): {cause}"
        ))
    })?;

    match component_manager
        .compute_tray
        .power_control(std::slice::from_ref(&endpoint), action)
        .await
    {
        Ok(results) => {
            let result = results.into_iter().next().unwrap_or(ComputeTrayResult {
                bmc_ip: endpoint.bmc_ip,
                bmc_mac: endpoint.bmc_mac,
                success: false,
                error: Some("component manager returned no result".into()),
                backend_job_id: None,
            });

            if result.success {
                tracing::info!(
                    machine_id = %host_machine_id,
                    operation = operation_label,
                    backend = component_manager.compute_tray.name(),
                    "Machine power control succeeded; returning host to Ready"
                );
                return finish_maintenance(host_machine_id, request, ctx, ManagedHostState::Ready)
                    .await;
            }

            let summary = result
                .error
                .unwrap_or_else(|| "power control failed".into());
            if matches!(action, PowerAction::ForceRestart) {
                return finish_uncertain_reset(host_machine_id, request, ctx, summary).await;
            }
            tracing::warn!(
                machine_id = %host_machine_id,
                operation = operation_label,
                backend = component_manager.compute_tray.name(),
                summary = %summary,
                "Machine power control returned a non-success result",
            );
            Err(StateHandlerError::GenericError(eyre::eyre!(
                "Machine {host_machine_id} maintenance ({operation_label}): power control failed: {summary}"
            )))
        }
        Err(error) => {
            if matches!(action, PowerAction::ForceRestart) {
                return finish_uncertain_reset(host_machine_id, request, ctx, error.to_string())
                    .await;
            }
            tracing::warn!(
                machine_id = %host_machine_id,
                operation = operation_label,
                backend = component_manager.compute_tray.name(),
                error = %error,
                "Machine power control transport error",
            );
            let context = format!(
                "Machine {host_machine_id} maintenance ({operation_label}): power control failed: {error}"
            );
            Err(StateHandlerError::GenericError(
                eyre::Report::new(error).wrap_err(context),
            ))
        }
    }
}

/// Build the [`ComputeTrayEndpoint`] describing this host for component manager power operations.
pub(super) async fn build_compute_tray_endpoint(
    machine_id: &HostMachineId,
    machine: &HostMachine,
    credential_manager: &dyn CredentialManager,
) -> Result<ComputeTrayEndpoint, String> {
    let bmc_mac = machine
        .status
        .bmc_info
        .mac
        .ok_or_else(|| format!("machine {machine_id} has no BMC MAC address recorded"))?;

    let bmc_ip =
        machine.status.bmc_info.ip.ok_or_else(|| {
            format!("no BMC IP found for machine {machine_id} (bmc_mac {bmc_mac})")
        })?;

    let credentials = lookup_bmc_credentials(credential_manager, bmc_mac).await?;

    Ok(ComputeTrayEndpoint {
        vendor: machine.bmc_vendor().into(),
        bmc_ip,
        bmc_mac,
        bmc_credentials: credentials,
    })
}

async fn lookup_bmc_credentials(
    credential_manager: &dyn CredentialManager,
    bmc_mac: MacAddress,
) -> Result<Credentials, String> {
    let bmc_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: bmc_mac,
        },
    };
    match credential_manager.get_credentials(&bmc_key).await {
        Ok(Some(creds)) => Ok(creds),
        Ok(None) => Err(format!(
            "no per-device BMC credentials configured for {bmc_mac}; the device must be (re)ingested"
        )),
        Err(error) => Err(format!(
            "failed to read BMC credentials for {bmc_mac}: {error}"
        )),
    }
}

/// Do not replay a reset whose response cannot establish whether it executed.
async fn finish_uncertain_reset(
    host_machine_id: &HostMachineId,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    cause: String,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    finish_maintenance(
        host_machine_id,
        request,
        ctx,
        ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::UnhandledState {
                    err: format!(
                        "reset outcome unknown; verify the device before retrying: {cause}"
                    ),
                },
                failed_at: Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
            },
            machine_id: (*host_machine_id).into(),
            retry_count: 0,
        },
    )
    .await
}

async fn finish_maintenance(
    host_machine_id: &HostMachineId,
    request: Option<&MachineMaintenanceRequest>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    next_state: ManagedHostState,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let Some(request) = request else {
        // Older saved states cannot identify the request that started this
        // operation. Preserve the pending request, even if that repeats work.
        tracing::warn!(
            machine_id = %host_machine_id,
            "Maintenance completed without its original request; preserving pending maintenance"
        );
        return Ok(StateHandlerOutcome::transition(next_state));
    };

    let mut txn = ctx.services.db_pool.begin().await?;
    if let db::ConditionalWrite::NotApplied(reason) =
        db_machine::clear_machine_maintenance_requested(&mut txn, *host_machine_id, request).await?
    {
        tracing::debug!(
            machine_id = %host_machine_id,
            ?reason,
            "Completed maintenance request is no longer pending"
        );
    }
    Ok(StateHandlerOutcome::transition(next_state).with_txn(txn))
}

/// If a maintenance request has been posted via `machine_maintenance_requested`,
/// transitions to [`ManagedHostState::Maintenance`] with the requested operation.
pub(super) fn maintenance_transition_if_requested(
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Option<StateHandlerOutcome<ManagedHostState>> {
    let req = mh_snapshot
        .host_snapshot
        .machine_maintenance_requested
        .as_ref()?;
    tracing::info!(
        operation = ?req.operation,
        initiator = %req.initiator,
        "Machine maintenance requested; transitioning to Maintenance"
    );
    Some(StateHandlerOutcome::transition(
        ManagedHostState::maintenance_for_request(req.clone()),
    ))
}
