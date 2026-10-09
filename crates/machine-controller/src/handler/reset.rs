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

//! Operator-requested managed host reset: wait for the DPUs to return to `Admin`,
//! delete the tenant instance, clean up the host, and remove its DPF CRs before
//! DPU discovery re-ingests it. Hosts without an instance skip the network wait.

use eyre::eyre;
use model::machine::{
    CleanupContext, CleanupState, DpuDiscoveringState, DpuDiscoveringStates, ManagedHostState,
    ManagedHostStateSnapshot, ResetState,
};
use model::resource_pool::common::CommonPools;
use state_controller::CheckApplied as _;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use super::{
    process_dpu_use_admin_network_state_change, release_network_segments_with_vpc_prefix,
    release_vpc_dpu_loopback, waiting_for_cleanup_state,
};
use crate::context::MachineStateHandlerContextObjects;
use crate::dpf::{DpfOperations, dpf_dpudevices_and_dpunode_crs_noexist};

pub(super) async fn handle_reset(
    reset_state: &ResetState,
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    common_pools: Option<&CommonPools>,
    dpf_sdk: Option<&dyn DpfOperations>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    match reset_state {
        ResetState::DeletingInstance => handle_deleting_instance(state, ctx, common_pools).await,
        ResetState::DeletingCrs => handle_deleting_crs(state, ctx, dpf_sdk).await,
    }
}

async fn handle_deleting_instance(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    common_pools: Option<&CommonPools>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let Some(instance) = state.instance.as_ref() else {
        return Ok(StateHandlerOutcome::transition(ManagedHostState::Reset {
            reset_state: ResetState::DeletingCrs,
        }));
    };

    if !state.use_admin_network() {
        let mut txn = ctx.services.db_pool.begin().await?;
        let mut network_config = state.host_snapshot.network_config.value.clone();
        network_config.use_admin_network = Some(true);
        db::machine::try_update_network_config(
            &mut txn,
            &state.host_snapshot.id,
            state.host_snapshot.network_config.version,
            &network_config,
        )
        .await?
        .check_applied()?;

        if ctx
            .services
            .site_config
            .restart_ovs_on_use_admin_network_change
        {
            process_dpu_use_admin_network_state_change(&mut txn, state).await?;
        }

        // Commit Admin intent before checking observations from a fresh snapshot.
        // The persisted mode keeps retries from requesting another version.
        return Ok(StateHandlerOutcome::wait(
            "waiting for DPUs to apply Admin networking before deleting the Instance".to_string(),
        )
        .with_txn(txn));
    }

    // Retain the Instance and its resources until every topology DPU has
    // acknowledged Admin. Missing observations cannot establish isolation.
    if !state.managed_host_network_config_version_synced() {
        return Ok(StateHandlerOutcome::wait(
            "waiting for DPUs to apply Admin networking before deleting the Instance".to_string(),
        ));
    }

    // The delete and the segment release must commit together, as in the Assigned
    // termination path.
    let mut txn = ctx.services.db_pool.begin().await?;
    db::instance::delete(instance.id, &mut txn)
        .await
        .map_err(|err| StateHandlerError::GenericError(err.into()))?;

    release_network_segments_with_vpc_prefix(&instance.config.network.interfaces, &mut txn).await?;

    release_vpc_dpu_loopback(state, common_pools, &mut txn).await?;

    // Cleanup is not exempt from failure parking, so an old failure record would stop the reset.
    db::machine::clear_failure_details(&state.host_snapshot.id, &mut txn).await?;
    for dpu in &state.dpu_snapshots {
        db::machine::clear_failure_details(&dpu.id, &mut txn).await?;
    }

    let ignore_cleanup = state
        .host_snapshot
        .reset_requested
        .as_ref()
        .is_some_and(|request| request.ignore_cleanup);
    let next = if ignore_cleanup {
        ManagedHostState::Reset {
            reset_state: ResetState::DeletingCrs,
        }
    } else {
        waiting_for_cleanup_state(CleanupState::Init, CleanupContext::Reset)
    };
    Ok(StateHandlerOutcome::transition(next).with_txn(txn))
}

/// Deletes the host's DPF CRs and polls until they are gone.
///
/// Registration refuses a CR that still carries a deletionTimestamp, so re-ingestion has
/// to wait for the drain. `force_delete_host` tolerates CRs that are already gone, so
/// re-running it on each poll is safe.
async fn handle_deleting_crs(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    dpf_sdk: Option<&dyn DpfOperations>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let dpf_sdk = dpf_sdk.ok_or_else(|| {
        StateHandlerError::GenericError(eyre!(
            "managed host {} reset requires DPF, but DPF is not configured",
            state.host_snapshot.id
        ))
    })?;

    let host_dpf_id =
        state
            .host_snapshot
            .dpf_id()
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: state.host_snapshot.id.to_string(),
                missing: "dpf_id",
            })?;
    let dpu_dpf_ids = state
        .dpu_snapshots
        .iter()
        .map(|dpu| {
            dpu.dpf_id().ok_or_else(|| StateHandlerError::MissingData {
                object_id: dpu.id.to_string(),
                missing: "dpf_id",
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Reused from decommissioning; deletes the host's DPUNode and DPUDevice CRs.
    dpf_sdk
        .force_delete_host(&host_dpf_id, &dpu_dpf_ids)
        .await
        .map_err(|error| {
            StateHandlerError::GenericError(eyre!(
                "failed to delete managed host {} from DPF: {error}",
                state.host_snapshot.id
            ))
        })?;

    if !dpf_dpudevices_and_dpunode_crs_noexist(state, dpf_sdk)
        .await
        .map_err(|error| StateHandlerError::GenericError(error.into()))?
    {
        return Ok(StateHandlerOutcome::wait(
            "waiting for managed host DPF resources to be deleted".to_string(),
        ));
    }

    start_host_reingestion(state, ctx).await
}

/// Hands the drained host back to DPU discovery so DPF re-registers it from scratch.
///
/// Discovery, not `DPUInit`, decides whether the host is DPF-provisioned; its
/// `RebootAllDPUS` substate is the only path to `DpfState::Provisioning`, which recreates
/// the CRs deleted above. Entering at `Initializing` matches fresh ingestion, so the
/// re-ingested host also gets `EnableRshim` and the secure-boot substate that follows it.
async fn start_host_reingestion(
    state: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host_id = &state.host_snapshot.id;

    let mut dpu_states = std::collections::HashMap::new();
    let mut txn = ctx.services.db_pool.begin().await?;

    // A surviving failure record would re-park the host via `get_failed_state`.
    db::machine::clear_failure_details(host_id, &mut txn).await?;
    db::machine_topology::set_topology_update_needed(&mut txn, host_id, true).await?;

    for dpu in &state.dpu_snapshots {
        db::machine::clear_failure_details(&dpu.id, &mut txn).await?;
        // A stale request would win the reprovision hinge once the reset stops firing.
        db::machine::clear_dpu_reprovisioning_request(&mut txn, &dpu.id, false).await?;
        db::machine_topology::set_topology_update_needed(&mut txn, &dpu.id, true).await?;

        dpu_states.insert(dpu.id, DpuDiscoveringState::Initializing);
    }

    // A started reset cannot be replaced, so this clears the one just completed.
    db::machine::clear_managed_host_reset_request(&mut txn, host_id, false).await?;

    tracing::info!(
        host_machine_id = %host_id,
        "Managed host reset drained the host's DPF CRs; re-ingesting from DPU discovery",
    );

    Ok(
        StateHandlerOutcome::transition(ManagedHostState::DpuDiscoveringState {
            dpu_states: DpuDiscoveringStates { states: dpu_states },
        })
        .with_txn(txn),
    )
}
