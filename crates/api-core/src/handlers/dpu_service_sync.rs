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

//! Operator-driven release of the DPF maintenance holds that park DPUs while a
//! changed DPUService waits to roll out.
//!
//! Carbide releases these on its own, but only for idle hosts, and only when the
//! site has left the automatic rollout enabled. Neither is always true: a site
//! can turn it off and drive rollouts by hand, and a host that never reaches
//! `Ready` -- stranded, say, by the very DPUService version the operator is
//! trying to replace -- can never be released automatically at all.
//!
//! This relaxes the host-state requirement and nothing else. Every DPU is still
//! checked against its DPUDeployment before its hold is lifted.

use std::collections::{HashMap, HashSet};

use ::rpc::forge as rpc;
use carbide_machine_controller::dpu_service_sync::{
    ReleaseOutcome, TenantPolicy, release_hold_if_dpus_are_current,
};
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSubtypeTrait, StableHostMachineId};
use db::managed_host::load_snapshot;
use model::machine::LoadSnapshotOptions;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine_pending_action::MachinePendingActionActor;
use model::machine_pending_action::MachinePendingActionKind::DpuServiceSync;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// Ceiling on machines per release call.
///
/// The automatic path is paced by the state controller's bounded concurrency.
/// A direct RPC has no such bound, and `list | xargs release` would otherwise
/// reconstruct the fleet-wide form this API deliberately omits. Batching stays
/// possible, but only in visible, deliberate chunks.
const MAX_RELEASE_BATCH: usize = 256;

/// The machines DPF is waiting on, ids only.
///
/// Ids rather than detail because a fleet-wide rollout can leave every host
/// waiting at once; callers page through the detail with
/// [`find_pending_dpu_service_syncs_by_ids`].
pub(crate) async fn find_pending_dpu_service_sync_ids(
    api: &Api,
    request: Request<rpc::FindPendingDpuServiceSyncIdsRequest>,
) -> Result<Response<::rpc::common::HostMachineIdList>, Status> {
    log_request_data(&request);

    let machine_ids =
        db::machine_pending_action::find_outstanding_machine_ids(api.pg_pool(), DpuServiceSync)
            .await?;

    let machine_ids = machine_ids
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()
        .map_err(CarbideError::from)?;
    Ok(Response::new(::rpc::common::HostMachineIdList {
        machine_ids,
    }))
}

/// Detail for a bounded slice of the worklist.
pub(crate) async fn find_pending_dpu_service_syncs_by_ids(
    api: &Api,
    request: Request<rpc::FindPendingDpuServiceSyncsByIdsRequest>,
) -> Result<Response<rpc::ListPendingDpuServiceSyncsResponse>, Status> {
    log_request_data(&request);
    let machine_ids = request.into_inner().machine_ids;

    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if machine_ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if machine_ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    let mut txn = api.txn_begin().await?;
    let actions = db::machine_pending_action::find_outstanding_by_machine_ids(
        &mut txn,
        DpuServiceSync,
        &machine_ids,
    )
    .await?;
    let pending = project(api, &mut txn, actions).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::ListPendingDpuServiceSyncsResponse {
        pending,
    }))
}

/// One machine's recorded history, newest first.
///
/// Needs no paging: the database caps retained history per machine.
pub(crate) async fn list_dpu_service_sync_history(
    api: &Api,
    request: Request<rpc::ListDpuServiceSyncHistoryRequest>,
) -> Result<Response<rpc::ListPendingDpuServiceSyncsResponse>, Status> {
    log_request_data(&request);
    let machine_id: HostMachineId =
        convert_and_log_machine_id(request.get_ref().machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;
    let actions = db::machine_pending_action::find_all_for_machine(&mut txn, &machine_id).await?;
    let pending = project(api, &mut txn, actions).await?;
    txn.commit().await?;

    Ok(Response::new(rpc::ListPendingDpuServiceSyncsResponse {
        pending,
    }))
}

/// Turns stored actions into their wire form, resolving each machine's state and
/// tenancy in one query apiece rather than a pair per machine.
async fn project(
    _api: &Api,
    txn: &mut db::Transaction<'_>,
    actions: Vec<model::machine_pending_action::MachinePendingAction>,
) -> Result<Vec<rpc::PendingDpuServiceSync>, Status> {
    let machine_ids: Vec<HostMachineId> = actions
        .iter()
        .filter_map(|action| HostMachineId::try_from(action.machine_id).ok())
        .collect();
    let states = machine_states(txn, &machine_ids).await?;
    let instances = db::instance::find_by_machine_ids(txn, &machine_ids.iter().collect::<Vec<_>>())
        .await?
        .into_iter()
        .map(|instance| (instance.machine_id, instance.id))
        .collect::<HashMap<_, _>>();

    actions
        .into_iter()
        .map(|action| {
            let host_machine_id = HostMachineId::try_from(action.machine_id)?;
            Ok::<_, CarbideError>(rpc::PendingDpuServiceSync {
                machine_id: Some(host_machine_id),
                requested_at: Some(action.requested_at.into()),
                state: states
                    .get(&host_machine_id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_string()),
                instance_id: instances.get(&host_machine_id).copied(),
                completed_at: action.completed_at.map(Into::into),
                // Left absent while outstanding. `UpdateInitiator::AdminCli` is zero,
                // so a default here would misreport every waiting machine as
                // operator-completed.
                completed_by: action.completed_by.map(|actor| {
                    match actor {
                        MachinePendingActionActor::Automatic => rpc::UpdateInitiator::Automatic,
                        MachinePendingActionActor::AdminCli => rpc::UpdateInitiator::AdminCli,
                    }
                    .into()
                }),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Releases the DPF maintenance hold for the named machines.
pub(crate) async fn release_dpu_service_sync_hold(
    api: &Api,
    request: Request<rpc::ReleaseDpuServiceSyncHoldRequest>,
) -> Result<Response<rpc::ReleaseDpuServiceSyncHoldResponse>, Status> {
    log_request_data(&request);

    let Some(dpf_sdk) = api.dpf_sdk.as_ref() else {
        return Err(CarbideError::InvalidArgument(
            "DPF is not enabled on this nico instance".to_string(),
        )
        .into());
    };

    // Each entry carries its own tenant policy: naming an instance consents to
    // disrupting that instance's tenant and nobody else's.
    let targets = resolve_target(api, request.get_ref()).await?;
    let machine_ids: Vec<HostMachineId> = targets.iter().map(|(id, _)| *id).collect();
    validate(api, &machine_ids).await?;

    // One machine at a time, each committed as it goes. Nothing spans the batch:
    // a rollback partway would undo completions for holds that are already gone
    // externally, leaving the release done but the action still saying it is
    // owed -- the one outcome worse than not having released at all.
    let mut results = Vec::with_capacity(targets.len());
    for (machine_id, tenant_policy) in targets {
        let status = release_one(api, dpf_sdk.as_ref(), machine_id, &tenant_policy).await;
        results.push(status);
    }

    Ok(Response::new(rpc::ReleaseDpuServiceSyncHoldResponse {
        results,
    }))
}

/// Turns the request's target into the machines to act on, each paired with the
/// tenant policy that applies to it.
///
/// The policy is per machine rather than per request because consent is per
/// tenant: naming three instances is three separate acknowledgements, each
/// covering only its own.
async fn resolve_target(
    api: &Api,
    request: &rpc::ReleaseDpuServiceSyncHoldRequest,
) -> Result<Vec<(HostMachineId, TenantPolicy)>, Status> {
    use rpc::release_dpu_service_sync_hold_request::Target;

    match request.target.as_ref() {
        Some(Target::MachineIds(list)) => Ok(list
            .machine_ids
            .iter()
            // Naming a machine says nothing about the tenant that may be on it.
            .map(|machine_id| (*machine_id, TenantPolicy::RefuseIfAssigned))
            .collect()),
        Some(Target::InstanceIds(list)) => {
            // Checked before the loop, not just in `validate`: resolving costs a
            // query per instance, so an oversized batch would otherwise do all
            // of that work inside a transaction only to be rejected after.
            check_batch_size(list.instance_ids.len())?;
            let mut txn = api.txn_begin().await?;
            let mut targets = Vec::with_capacity(list.instance_ids.len());
            for instance_id in &list.instance_ids {
                let instance = db::instance::find_by_id(&mut txn, *instance_id)
                    .await?
                    .ok_or_else(|| CarbideError::NotFoundError {
                        kind: "instance",
                        id: instance_id.to_string(),
                    })?;
                // Only machines with stable IDs can be assigned to instances.
                let instance_machine_id = StableHostMachineId::try_from(instance.machine_id)
                    .map_err(|e| {
                        CarbideError::internal(format!("bug: invalid machine ID in instance: {e}"))
                    })?
                    .into();
                // Consent is for this instance, not for its host: if the host
                // has been reallocated since, the new tenant agreed to nothing.
                targets.push((
                    instance_machine_id,
                    TenantPolicy::AllowNamedInstance(*instance_id),
                ));
            }
            txn.commit().await?;
            Ok(targets)
        }
        None => Err(CarbideError::InvalidArgument(
            "a target is required: either machine_ids or instance_ids".to_string(),
        )
        .into()),
    }
}

fn check_batch_size(len: usize) -> Result<(), Status> {
    if len > MAX_RELEASE_BATCH {
        return Err(CarbideError::InvalidArgument(format!(
            "at most {MAX_RELEASE_BATCH} may be released per call, got {len}"
        ))
        .into());
    }
    Ok(())
}

/// Rejects the whole request before anything irreversible happens.
///
/// Releasing is an external action that cannot be undone, so a malformed batch
/// must not release half of itself first. It also keeps `FAILED` meaning
/// "retry": a mistyped machine id is not retryable and would be actively
/// misleading reported that way.
async fn validate(api: &Api, machine_ids: &[HostMachineId]) -> Result<(), Status> {
    if machine_ids.is_empty() {
        return Err(CarbideError::InvalidArgument("no machines were named".to_string()).into());
    }
    check_batch_size(machine_ids.len())?;

    let mut txn = api.txn_begin().await?;
    let found = db::machine::find(
        &mut txn,
        db::ObjectFilter::List(machine_ids),
        MachineSearchConfig {
            include_predicted_host: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?
    .into_iter()
    .map(|machine| machine.id)
    .collect::<HashSet<_>>();
    txn.commit().await?;

    let missing: Vec<String> = machine_ids
        .iter()
        .filter(|machine_id| !found.contains(*machine_id))
        .map(ToString::to_string)
        .collect();
    if !missing.is_empty() {
        return Err(CarbideError::NotFoundError {
            kind: "machine",
            id: missing.join(", "),
        }
        .into());
    }

    Ok(())
}

/// Releases one host's hold, reporting what happened rather than failing the
/// call: a batch is expected to contain machines that decline for good reasons.
async fn release_one(
    api: &Api,
    dpf_sdk: &dyn carbide_machine_controller::dpf::DpfOperations,
    machine_id: HostMachineId,
    tenant_policy: &TenantPolicy,
) -> rpc::DpuServiceSyncReleaseResult {
    use rpc::DpuServiceSyncReleaseStatus as ProtoStatus;

    let (status, detail) = match release_one_inner(api, dpf_sdk, machine_id, tenant_policy).await {
        Ok(None) => (ProtoStatus::NotPending, String::new()),
        Ok(Some(ReleaseOutcome::Released)) => (ProtoStatus::Released, String::new()),
        Ok(Some(ReleaseOutcome::DeferredDpuOutdated { dpu })) => (
            ProtoStatus::DeferredDpuOutdated,
            format!("DPU {dpu} does not match its DPUDeployment and awaits reprovisioning"),
        ),
        Ok(Some(ReleaseOutcome::DeferredUnknown { dpu, reason })) => (
            ProtoStatus::DeferredUnknown,
            format!("could not evaluate DPU {dpu}: {reason}"),
        ),
        Ok(Some(ReleaseOutcome::DeferredHostAssigned { instance })) => (
            ProtoStatus::DeferredHostAssigned,
            format!("host is assigned to instance {instance}; name that instance to release it"),
        ),
        Ok(Some(ReleaseOutcome::Failed { reason })) => (ProtoStatus::Failed, reason),
        Err(reason) => (ProtoStatus::Failed, reason),
    };

    rpc::DpuServiceSyncReleaseResult {
        machine_id: Some(machine_id),
        status: status.into(),
        detail,
    }
}

/// `Ok(None)` means nothing was owed for this machine.
async fn release_one_inner(
    api: &Api,
    dpf_sdk: &dyn carbide_machine_controller::dpf::DpfOperations,
    machine_id: HostMachineId,
    tenant_policy: &TenantPolicy,
) -> Result<Option<ReleaseOutcome>, String> {
    // A pooled connection rather than a transaction, matching the automatic path.
    //
    // There is nothing here to roll back: the only write is the completion, and
    // it is the last statement, reached only once the hold has actually been
    // released. Every earlier failure returns having written nothing.
    //
    // Wrapping it would be worse than useless. Its one effect would be to undo
    // that completion when the commit itself failed -- after the release had
    // already happened externally and could not be taken back -- leaving the
    // hold lifted while the record still said the work was owed. It would also
    // pin the connection `idle in transaction` across a Kubernetes round trip
    // per DPU, neither of which has a timeout.
    let mut conn = api
        .database_connection
        .acquire()
        .await
        .map_err(|error| format!("could not acquire a database connection: {error}"))?;

    let outstanding =
        db::machine_pending_action::is_outstanding(&mut *conn, &machine_id, DpuServiceSync)
            .await
            .map_err(|error| format!("could not read the pending action: {error}"))?;
    if !outstanding {
        return Ok(None);
    }

    let snapshot = load_snapshot(&mut *conn, &machine_id, LoadSnapshotOptions::default())
        .await
        .map_err(|error| format!("could not load the machine snapshot: {error}"))?
        .ok_or_else(|| format!("no snapshot for machine {machine_id}"))?;
    // Dropped here: the release call below acquires its own connections around
    // each of its writes, so nothing pins this one idle across its Kubernetes calls.
    drop(conn);

    let policy = match tenant_policy {
        TenantPolicy::RefuseIfAssigned => TenantPolicy::RefuseIfAssigned,
        TenantPolicy::AllowNamedInstance(instance_id) => {
            TenantPolicy::AllowNamedInstance(*instance_id)
        }
    };

    Ok(Some(
        release_hold_if_dpus_are_current(
            dpf_sdk,
            &api.database_connection,
            &snapshot.host_snapshot,
            &snapshot.dpu_snapshots,
            policy,
            MachinePendingActionActor::AdminCli,
        )
        .await,
    ))
}

/// The managed state of each machine, for the worklist's "why is this waiting"
/// column.
async fn machine_states<ID>(
    txn: &mut db::Transaction<'_>,
    machine_ids: &[ID],
) -> Result<HashMap<ID, String>, Status>
where
    ID: MachineIdSubtypeTrait,
    db::DatabaseError: From<<ID as TryFrom<MachineId>>::Error>,
{
    if machine_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let machines = db::machine::find(
        txn,
        db::ObjectFilter::List(machine_ids),
        MachineSearchConfig {
            include_predicted_host: true,
            ..MachineSearchConfig::default()
        },
    )
    .await?;
    Ok(machines
        .into_iter()
        .map(|machine| (machine.id, machine.current_state().to_string()))
        .collect())
}
