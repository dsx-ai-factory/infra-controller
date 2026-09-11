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

//! Releases the DPF maintenance hold for hosts whose DPUs are already running
//! the software their DPUDeployment declares.
//!
//! A DPUService change (helm chart or image version) drives every affected DPU
//! into the NodeEffect phase, where DPF waits for permission to disrupt it.
//! Granting that permission is only correct for a DPU that is otherwise up to
//! date: one still awaiting reprovisioning is about to have its OS replaced, and
//! the new services may not run on the OS currently installed.
//!
//! Scope: this decides *when* a declared service version reaches a DPU, not
//! *whether* that version belongs there. It asks only whether the DPU matches
//! its DPUDeployment, so a deployment that pairs a service version with an
//! incompatible BFB is rolled out faithfully. Keeping those versions coherent is
//! the operator's responsibility, deliberately -- a compatibility check here
//! would also reject the test builds that qualifying a new service depends on.
//!
//! Runs from the `Ready` arm only. `Ready` is not the only state where releasing
//! a hold would be safe -- `HostInit` has provisioned DPUs and no tenant either
//! -- but keeping the automatic path narrow means it can never collide with the
//! DPF states where NICo itself drives DPU operations.
//!
//! The accepted cost is that a host which never reaches `Ready` never receives a
//! service update. That matters most when the update *is* the fix: a DPUService
//! version that breaks host provisioning strands hosts in `HostInit`, and they
//! cannot be repaired by the automatic path. Recovering those is an explicit
//! operator action rather than something this handler infers.

use model::machine::{HostMachine, ManagedHostState, ManagedHostStateSnapshot};
use model::machine_pending_action::MachinePendingActionActor;
use model::machine_pending_action::MachinePendingActionKind::DpuServiceSync;
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::MachineStateHandlerContextObjects;
use crate::dpf::DpfOperations;
use crate::dpu_service_sync::{ReleaseOutcome, TenantPolicy};

/// Performs the DPU-side work a host owes, recorded as a pending action while
/// the host was busy elsewhere.
///
/// Only [`DpuServiceSync`] exists today: release the maintenance hold once every
/// one of the host's DPUs matches its owning DPUDeployment.
///
/// Always reports [`StateHandlerOutcome::do_nothing`]: this is background work
/// that must never preempt a lifecycle or operator-requested transition, and it
/// drives no state change of its own. Failures are logged rather than returned
/// for the same reason — the pending marker survives, so the next sweep retries.
pub(super) async fn handle_pending_dpu_actions(
    dpf_sdk: Option<&dyn DpfOperations>,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<StateHandlerOutcome<ManagedHostState>, StateHandlerError> {
    let host = &mh_snapshot.host_snapshot;

    if !ctx.services.site_config.dpu_service_sync_enabled || !host.config.dpf.used_for_ingestion {
        return Ok(StateHandlerOutcome::do_nothing());
    }

    // `used_for_ingestion` does not guarantee the SDK was configured.
    let Some(dpf_sdk) = dpf_sdk else {
        return Ok(StateHandlerOutcome::do_nothing());
    };

    // The marker is what keeps ordinary Ready sweeps off the Kubernetes API:
    // without one, DPF is not waiting on this host and there is nothing to do.
    //
    // A database failure here is logged like every other failure in this
    // function rather than returned. Propagating would abort the rest of Ready's
    // work for the sake of a background task, and the marker survives either way.
    match pending_sync_is_outstanding(ctx, host).await {
        Ok(true) => {}
        Ok(false) => return Ok(StateHandlerOutcome::do_nothing()),
        Err(error) => {
            tracing::warn!(
                machine_id = %host.id,
                %error,
                "Could not read the host's pending DPU actions"
            );
            return Ok(StateHandlerOutcome::do_nothing());
        }
    }

    let outcome = crate::dpu_service_sync::release_hold_if_dpus_are_current(
        dpf_sdk,
        &ctx.services.db_pool,
        host,
        &mh_snapshot.dpu_snapshots,
        // Nobody has consented to disrupting whichever tenant happens to
        // be on this host, so an instance appearing mid-check keeps the
        // hold.
        TenantPolicy::RefuseIfAssigned,
        MachinePendingActionActor::Automatic,
    )
    .await;

    match outcome {
        ReleaseOutcome::Released => tracing::info!(
            machine_id = %host.id,
            dpu_count = mh_snapshot.dpu_snapshots.len(),
            "Released the DPF maintenance hold so DPU services can roll out"
        ),
        ReleaseOutcome::DeferredDpuOutdated { dpu } => tracing::info!(
            machine_id = %host.id,
            dpu_machine_id = %dpu,
            "Holding the DPF maintenance hold: DPU still awaits reprovisioning"
        ),
        ReleaseOutcome::DeferredUnknown { dpu, reason } => tracing::warn!(
            machine_id = %host.id,
            dpu_machine_id = %dpu,
            %reason,
            "Could not determine whether the DPU is up to date; keeping the hold"
        ),
        ReleaseOutcome::DeferredHostAssigned { instance } => tracing::info!(
            machine_id = %host.id,
            instance_id = %instance,
            "Host was assigned while its DPUs were being checked; keeping the hold"
        ),
        ReleaseOutcome::Failed { reason } => tracing::warn!(
            machine_id = %host.id,
            %reason,
            "Could not release the DPF maintenance hold for DPU service sync"
        ),
    }

    Ok(StateHandlerOutcome::do_nothing())
}

async fn pending_sync_is_outstanding(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host: &HostMachine,
) -> Result<bool, StateHandlerError> {
    let mut conn = ctx.services.db_pool.acquire().await?;
    Ok(db::machine_pending_action::is_outstanding(&mut *conn, &host.id, DpuServiceSync).await?)
}

/// Marks this host's pending DPU service sync as done.
///
/// Callable from the DPF provisioning handler as well: releasing a node's hold
/// there satisfies any sync recorded for it, and the watcher cannot tell the two
/// causes apart when it writes the marker.
pub(super) async fn complete_pending_sync(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    host: &HostMachine,
) -> Result<bool, StateHandlerError> {
    let mut conn = ctx.services.db_pool.acquire().await?;
    // Both callers are carbide acting on its own: this handler having confirmed
    // the DPUs are current, and the provisioning path having released the hold
    // itself. An operator-driven release records itself separately.
    Ok(db::machine_pending_action::complete(
        &mut conn,
        &host.id,
        DpuServiceSync,
        MachinePendingActionActor::Automatic,
    )
    .await?)
}
