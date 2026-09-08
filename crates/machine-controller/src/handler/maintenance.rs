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
use carbide_uuid::machine::MachineId;
use chrono::Utc;
use component_manager::compute_tray_manager::{ComputeTrayEndpoint, ComputeTrayResult};
use db::machine as db_machine;
use mac_address::MacAddress;
use model::component_manager::PowerAction;
use model::machine::{
    FailureCause, FailureDetails, FailureSource, Machine, MachineMaintenanceOperation,
    ManagedHostState, ManagedHostStateSnapshot, StateMachineArea,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;

/// Handles the Maintenance state for a host, dispatching on the requested
/// operation (`PowerOn` / `PowerOff` / `Reset`).
pub(super) async fn handle_maintenance(
    host_machine_id: &MachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let operation = match &mh_snapshot.managed_state {
        ManagedHostState::Maintenance { operation } => *operation,
        _ => unreachable!("handle_maintenance called with non-Maintenance state"),
    };

    match operation {
        MachineMaintenanceOperation::PowerOn => {
            handle_power_on(host_machine_id, mh_snapshot, ctx).await
        }
        MachineMaintenanceOperation::PowerOff => {
            handle_power_off(host_machine_id, mh_snapshot, ctx).await
        }
        MachineMaintenanceOperation::Reset => handle_reset(host_machine_id, mh_snapshot, ctx).await,
    }
}

async fn handle_power_on(
    host_machine_id: &MachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(machine_id = %host_machine_id, "Machine maintenance: PowerOn");
    invoke_power_operation(
        host_machine_id,
        mh_snapshot,
        ctx,
        PowerAction::On,
        "PowerOn",
    )
    .await
}

async fn handle_power_off(
    host_machine_id: &MachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(machine_id = %host_machine_id, "Machine maintenance: PowerOff");
    invoke_power_operation(
        host_machine_id,
        mh_snapshot,
        ctx,
        PowerAction::ForceOff,
        "PowerOff",
    )
    .await
}

async fn handle_reset(
    host_machine_id: &MachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    tracing::info!(machine_id = %host_machine_id, "Machine maintenance: Reset");
    invoke_power_operation(
        host_machine_id,
        mh_snapshot,
        ctx,
        PowerAction::ForceRestart,
        "Reset",
    )
    .await
}

/// Common driver for component-manager-backed power maintenance operations.
async fn invoke_power_operation(
    host_machine_id: &MachineId,
    mh_snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    action: PowerAction,
    operation_label: &'static str,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let machine = &mh_snapshot.host_snapshot;

    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return finish_maintenance_with_error(
            host_machine_id,
            ctx,
            format!(
                "Machine {host_machine_id} maintenance ({operation_label}): component manager not configured"
            ),
        )
        .await;
    };

    let endpoint = match build_compute_tray_endpoint(
        host_machine_id,
        machine,
        ctx.services.credential_manager.as_ref(),
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(cause) => {
            return finish_maintenance_with_error(
                host_machine_id,
                ctx,
                format!("Machine {host_machine_id} maintenance ({operation_label}): {cause}"),
            )
            .await;
        }
    };

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
            });

            if result.success {
                tracing::info!(
                    machine_id = %host_machine_id,
                    operation = operation_label,
                    backend = component_manager.compute_tray.name(),
                    "Machine power control succeeded; returning host to Ready"
                );
                let mut txn = ctx.services.db_pool.begin().await?;
                db_machine::clear_machine_maintenance_requested(&mut txn, *host_machine_id).await?;
                return Ok(StateHandlerOutcome::transition(ManagedHostState::Ready).with_txn(txn));
            }

            let summary = result
                .error
                .unwrap_or_else(|| "power control failed".into());
            tracing::warn!(
                machine_id = %host_machine_id,
                operation = operation_label,
                backend = component_manager.compute_tray.name(),
                summary = %summary,
                "Machine power control returned a non-success result",
            );
            finish_maintenance_with_error(
                host_machine_id,
                ctx,
                format!(
                    "Machine {host_machine_id} maintenance ({operation_label}): power control failed: {summary}"
                ),
            )
            .await
        }
        Err(error) => {
            tracing::warn!(
                machine_id = %host_machine_id,
                operation = operation_label,
                backend = component_manager.compute_tray.name(),
                error = %error,
                "Machine power control transport error",
            );
            finish_maintenance_with_error(
                host_machine_id,
                ctx,
                format!(
                    "Machine {host_machine_id} maintenance ({operation_label}): power control failed: {error}"
                ),
            )
            .await
        }
    }
}

/// Build the [`ComputeTrayEndpoint`] describing this host for component manager power operations.
pub(super) async fn build_compute_tray_endpoint(
    machine_id: &MachineId,
    machine: &Machine,
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

/// Clear the pending maintenance request and transition to `Failed` with the
/// given cause. Clearing the request breaks retry loops on persistent failures
/// and forces the operator to explicitly re-request maintenance to retry.
async fn finish_maintenance_with_error(
    host_machine_id: &MachineId,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    cause: String,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let mut txn = ctx.services.db_pool.begin().await?;
    db_machine::clear_machine_maintenance_requested(&mut txn, *host_machine_id).await?;
    Ok(StateHandlerOutcome::transition(ManagedHostState::Failed {
        details: FailureDetails {
            cause: FailureCause::UnhandledState { err: cause },
            failed_at: Utc::now(),
            source: FailureSource::StateMachineArea(StateMachineArea::MainFlow),
        },
        machine_id: *host_machine_id,
        retry_count: 0,
    })
    .with_txn(txn))
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
        ManagedHostState::maintenance_for_operation(req.operation),
    ))
}
