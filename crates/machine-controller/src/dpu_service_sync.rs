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

//! Releasing the DPF maintenance hold that parks a host's DPUs while a changed
//! DPUService waits to roll out.
//!
//! Shared by the two callers that can decide to lift a hold: the `Ready` state
//! handler, which does it on carbide's own initiative for idle hosts, and the
//! admin API, which does it for a host an operator has named. They differ in
//! which hosts they are willing to consider, not in what "safe to release" means
//! -- so the decision lives here once and the gates stay with the callers.

use carbide_dpf::sdk::{dpu_cr_name, dpu_node_cr_name};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::DpuMachineId;
use model::machine::{DpuMachine, HostMachine};
use model::machine_pending_action::MachinePendingActionActor;
use model::machine_pending_action::MachinePendingActionKind::DpuServiceSync;
use sqlx::PgPool;

use crate::dpf::DpfOperations;

/// What to do when the host turns out to be assigned.
///
/// Checked fresh immediately before the release rather than by the caller, for
/// the reasons in [`release_hold_if_dpus_are_current`].
pub enum TenantPolicy {
    /// Refuse to release an assigned host.
    ///
    /// The automatic sweep and the operator's bulk path both use this: neither
    /// has consent to disrupt whichever tenant happens to be on the host.
    RefuseIfAssigned,
    /// Release even though the host is assigned, but only to this instance.
    ///
    /// An operator naming a tenant's instance is the acknowledgement that the
    /// tenant will be disrupted. The consent is for *that* instance, so this is
    /// deliberately not a bypass: if the host has since been reallocated, the
    /// new tenant never consented and the release is refused.
    AllowNamedInstance(InstanceId),
}

/// Why a release did or did not happen.
///
/// Every decision is a value rather than a log line, because the admin API
/// answers a caller synchronously and has to say which of these happened. The
/// state handler logs off the same values.
#[derive(Debug)]
pub enum ReleaseOutcome {
    /// The hold was lifted and the pending action completed.
    Released,
    /// A DPU still differs from its DPUDeployment, so its OS is about to be
    /// replaced. Retrying achieves nothing until it has been reprovisioned.
    DeferredDpuOutdated { dpu: DpuMachineId },
    /// A DPU could not be evaluated at all. Unlike an outdated DPU this is worth
    /// retrying, since it usually means Kubernetes was unreachable.
    DeferredUnknown { dpu: DpuMachineId, reason: String },
    /// The host is assigned and the policy did not permit disrupting it.
    DeferredHostAssigned { instance: InstanceId },
    /// The attempt failed part-way. Retryable.
    Failed { reason: String },
}

/// Lifts `host`'s DPF maintenance hold, once every one of its DPUs is confirmed
/// to match its owning DPUDeployment, and completes the pending action.
///
/// The caller is responsible for every gate that decides *whether* to call
/// this at all: the site's `dpu_service_sync_enabled` switch, `used_for_ingestion`,
/// reading the outstanding pending action, and any requirement on the host's
/// [`ManagedHostState`]. This function does not check them, because the two
/// callers disagree on that last one: the state handler only calls this from
/// the `Ready` arm, while the admin API calls it regardless of state, which is
/// what lets an operator rescue a host that can never reach `Ready` on its own.
///
/// [`ManagedHostState`]: model::machine::ManagedHostState
pub async fn release_hold_if_dpus_are_current(
    dpf_sdk: &dyn DpfOperations,
    db_pool: &PgPool,
    host: &HostMachine,
    dpus: &[DpuMachine],
    tenant_policy: TenantPolicy,
    actor: MachinePendingActionActor,
) -> ReleaseOutcome {
    let Some(node_id) = host.dpf_id() else {
        return ReleaseOutcome::Failed {
            reason: "host has no BMC MAC, so its DPF node cannot be identified".to_string(),
        };
    };
    let node_name = dpu_node_cr_name(&node_id);

    // "Every DPU is current" is only a reason to release when there was at least
    // one DPU to check. An empty list means the host's DPUs could not be
    // resolved, not that they are all up to date, and releasing on it would
    // grant DPF permission having verified nothing.
    if dpus.is_empty() {
        return ReleaseOutcome::Failed {
            reason: "host has no DPU snapshots, so its DPUs cannot be checked for currency"
                .to_string(),
        };
    }

    // The hold is per node, so every DPU on the host must be current before it is
    // safe to lift.
    for dpu in dpus {
        let Some(device_id) = dpu.dpf_id() else {
            return ReleaseOutcome::DeferredUnknown {
                dpu: dpu.id,
                reason: "DPU has no BMC MAC, so its DPU CR cannot be identified".to_string(),
            };
        };

        // `is_dpu_outdated` folds a comparison it could not make into `true`, so
        // an unevaluable DPU reports as outdated rather than unknown here. Both
        // decline, so the invariant holds either way; only the advice to the
        // operator is coarser than it could be.
        match dpf_sdk
            .is_dpu_outdated(&dpu_cr_name(&device_id, &node_id))
            .await
        {
            Ok(false) => {}
            Ok(true) => return ReleaseOutcome::DeferredDpuOutdated { dpu: dpu.id },
            Err(error) => {
                return ReleaseOutcome::DeferredUnknown {
                    dpu: dpu.id,
                    reason: error.to_string(),
                };
            }
        }
    }

    // The caller's tenant check ran against a snapshot, and the DPU checks since
    // have cost a Kubernetes round trip each. Allocation commits under a row lock
    // no caller here holds, so an instance can appear in that window.
    //
    // Other work in the `Ready` arm does not need this: it only proposes a state
    // and loses harmlessly to the optimistic-concurrency version check. Releasing
    // a hold is an external action that cannot be taken back, so it re-reads
    // instead. That narrows the window to this query plus the patch rather than
    // closing it; the pending action survives, so a host that slips through is
    // caught once its instance is gone.
    let assigned = match db::instance::find_id_by_machine_id(db_pool, &host.id).await {
        Ok(assigned) => assigned,
        Err(error) => {
            return ReleaseOutcome::Failed {
                reason: format!("could not confirm whether the host is assigned: {error}"),
            };
        }
    };
    if let Some(instance) = assigned {
        let consented = match tenant_policy {
            TenantPolicy::RefuseIfAssigned => false,
            TenantPolicy::AllowNamedInstance(named) => named == instance,
        };
        if !consented {
            return ReleaseOutcome::DeferredHostAssigned { instance };
        }
    }

    if let Err(error) = dpf_sdk.release_maintenance_hold(&node_name).await {
        return ReleaseOutcome::Failed {
            reason: format!("failed to release the DPF maintenance hold: {error}"),
        };
    }

    // Only now, after a release that actually succeeded. A DPU already sitting in
    // NodeEffect does not re-enter it, so the watcher will not re-fire; an action
    // completed on any other path is a signal lost until a watcher relist.
    //
    // Failing to record the completion is the safe direction: the action stays
    // outstanding and a later pass releases an already-released hold, which is a
    // no-op.
    let mut conn = match db_pool.acquire().await {
        Ok(conn) => conn,
        Err(error) => {
            return ReleaseOutcome::Failed {
                reason: format!(
                    "released the hold but could not acquire a database connection to record it: {error}"
                ),
            };
        }
    };
    if let Err(error) =
        db::machine_pending_action::complete(&mut conn, &host.id, DpuServiceSync, actor).await
    {
        return ReleaseOutcome::Failed {
            reason: format!("released the hold but could not record it as completed: {error}"),
        };
    }

    ReleaseOutcome::Released
}
