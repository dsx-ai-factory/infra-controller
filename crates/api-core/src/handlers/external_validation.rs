// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Durable discovery and attempt APIs. Policy/lifecycle creation and allocation binding are separate milestones.

use carbide_uuid::external_validation::ExternalValidationRequestId;
use carbide_uuid::machine::{MachineId, StableHostMachineId};
use model::external_validation::{
    AllocationState, Attempt, AttemptState, AuditAction, ClosureKind, Hold, HoldFilter, HoldState,
    HoldWithAttempt,
};
use model::machine::LoadSnapshotOptions;
use rpc::forge as rpc;
use tonic::{Request, Response, Status};
use uuid::Uuid;

use crate::api::{Api, log_request_data};
use crate::auth::external_validation_actor;
use crate::{CarbideError, CarbideResult};

/// Lists every matching hold identity; tenant omission is authorized site inventory.
pub(crate) async fn find_ids(
    api: &Api,
    request: Request<rpc::ExternalValidationHoldSearchFilter>,
) -> Result<Response<rpc::ExternalValidationHoldIdList>, Status> {
    let actor = external_validation_actor(&request, "FindExternalValidationHoldIds")?;
    log_request_data(&request);
    let req = request.into_inner();
    optional_tenant(req.tenant_org_id.as_deref())?;
    let state = req.state.map(hold_state).transpose()?;
    let filter = HoldFilter {
        tenant_org_id: req.tenant_org_id,
        machine_id: req.machine_id,
        state,
    };
    let hold_ids = db::external_validation::find_ids(&api.database_connection, &filter).await?;
    tracing::debug!(%actor, count = hold_ids.len(), "external validation hold inventory");
    Ok(Response::new(rpc::ExternalValidationHoldIdList {
        hold_ids,
    }))
}

/// Retrieves a bounded, deduplicated batch, retaining terminal holds and outcomes.
pub(crate) async fn find_by_ids(
    api: &Api,
    request: Request<rpc::ExternalValidationHoldsByIdsRequest>,
) -> Result<Response<rpc::ExternalValidationHoldList>, Status> {
    let actor = external_validation_actor(&request, "FindExternalValidationHoldsByIds")?;
    log_request_data(&request);
    let req = request.into_inner();
    optional_tenant(req.tenant_org_id.as_deref())?;
    if req.hold_ids.is_empty()
        || !u32::try_from(req.hold_ids.len())
            .is_ok_and(|size| size <= api.runtime_config.max_find_by_ids)
    {
        return Err(CarbideError::InvalidArgument(format!(
            "hold_ids must contain 1..={} entries",
            api.runtime_config.max_find_by_ids
        ))
        .into());
    }
    let holds = db::external_validation::find_by_ids(
        &api.database_connection,
        &req.hold_ids,
        req.tenant_org_id.as_deref(),
    )
    .await?;
    tracing::debug!(%actor, count = holds.len(), "external validation hold retrieval");
    Ok(Response::new(rpc::ExternalValidationHoldList {
        holds: holds.into_iter().map(hold_rpc).collect(),
    }))
}

/// Opens one attempt under machine/hold locks, or returns the original keyed result before eligibility checks.
pub(crate) async fn start(
    api: &Api,
    request: Request<rpc::StartExternalValidationRequest>,
) -> Result<Response<rpc::StartExternalValidationResponse>, Status> {
    let actor = external_validation_actor(&request, "StartExternalValidation")?;
    log_request_data(&request);
    let req = request.into_inner();
    let machine = req
        .machine_id
        .ok_or(CarbideError::MissingArgument("machine_id"))?;
    tenant(&req.tenant_org_id)?;
    let key = Uuid::parse_str(&req.caller_idempotency_key).map_err(|_| {
        CarbideError::InvalidArgument("caller_idempotency_key must be a UUID".to_string())
    })?;
    let mut txn = api.txn_begin().await?;
    db::external_validation::lock_start_key(&mut txn, &req.tenant_org_id, key).await?;
    if let Some(original) =
        db::external_validation::find_by_key(&mut txn, &req.tenant_org_id, key).await?
    {
        if original.hold.machine_id != machine {
            return Err(CarbideError::AlreadyFoundError {
                kind: "idempotency key bound to another machine",
                id: key.to_string(),
            }
            .into());
        }
        let response = start_response(
            machine,
            Some(&original.hold),
            original.attempt.as_ref(),
            rpc::ExternalValidationStartStatus::Opened,
            true,
        );
        txn.commit().await?;
        tracing::info!(%actor, %machine, %key, replayed = true, "external validation Start");
        return Ok(Response::new(response));
    }
    db::machine::lock_by_id_for_update(&mut txn, &MachineId::from(machine)).await?;
    let mut host = db::managed_host::load_by_machine_ids(
        &mut txn,
        &[machine],
        LoadSnapshotOptions::default().with_host_health(api.runtime_config.host_health),
    )
    .await?
    .remove(&machine)
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: machine.to_string(),
    })?;
    let Some(hold) = db::external_validation::lock_active_hold(&mut txn, machine).await? else {
        txn.commit().await?;
        return Ok(Response::new(start_response(
            machine,
            None,
            None,
            rpc::ExternalValidationStartStatus::NoActiveHold,
            false,
        )));
    };
    owning_tenant(&hold, &req.tenant_org_id)?;
    match hold.state {
        HoldState::AttemptOpen => {
            let id = hold.current_attempt_id.ok_or_else(|| {
                CarbideError::internal("open validation hold has no current attempt".to_string())
            })?;
            let attempt = db::external_validation::lock_attempt(&mut txn, id).await?;
            let response = start_response(
                machine,
                Some(&hold),
                Some(&attempt),
                rpc::ExternalValidationStartStatus::AlreadyOpen,
                false,
            );
            txn.commit().await?;
            return Ok(Response::new(response));
        }
        HoldState::AwaitingCleanup | HoldState::Recovery => {
            return Err(CarbideError::FailedPrecondition(
                "external validation allocation or cleanup remains unresolved".to_string(),
            )
            .into());
        }
        HoldState::Satisfied => {
            return Err(CarbideError::internal(
                "active validation lookup returned a satisfied hold".to_string(),
            )
            .into());
        }
        HoldState::Pending => {}
    }
    // Only this workflow's own projection may be ignored. Repair and unrelated health remain blockers.
    let repair_requested = host
        .host_snapshot
        .health_reports
        .merges
        .contains_key(health_report::REPAIR_REQUEST_MERGE_SOURCE)
        || host
            .host_snapshot
            .health_reports
            .merges
            .contains_key(health_report::REQUEST_ONLINE_REPAIR_MERGE_SOURCE);
    host.host_snapshot
        .health_reports
        .merges
        .remove(&hold.health_report_source);
    if host
        .host_snapshot
        .health_reports
        .replace
        .as_ref()
        .is_some_and(|report| report.source == hold.health_report_source)
    {
        host.host_snapshot.health_reports.replace = None;
    }
    host.derive_aggregate_health(api.runtime_config.host_health);
    if repair_requested || host.is_usable_as_instance(false).is_err() {
        txn.commit().await?;
        return Ok(Response::new(start_response(
            machine,
            Some(&hold),
            None,
            rpc::ExternalValidationStartStatus::NoActiveHold,
            false,
        )));
    }
    let now = db::machine::database_now(&mut txn).await?;
    let attempt = db::external_validation::open_attempt(&mut txn, &hold, key, now).await?;
    db::external_validation::transition_hold(
        &mut txn,
        hold.hold_id,
        HoldState::AttemptOpen,
        Some(attempt.request_id),
        now,
    )
    .await?;
    db::external_validation::record_audit(
        &mut txn,
        attempt.request_id,
        AuditAction::Start,
        &actor,
        now,
    )
    .await?;
    let response = start_response(
        machine,
        Some(&hold),
        Some(&attempt),
        rpc::ExternalValidationStartStatus::Opened,
        false,
    );
    txn.commit().await?;
    tracing::info!(%actor, %machine, request_id = %attempt.request_id, replayed = false, "external validation Start");
    Ok(Response::new(response))
}

/// Cancels only a current NotStarted attempt; an identical recorded cancellation replays independently of newer work.
pub(crate) async fn cancel(
    api: &Api,
    request: Request<rpc::CancelExternalValidationRequest>,
) -> Result<Response<rpc::ExternalValidationAttempt>, Status> {
    let actor = external_validation_actor(&request, "CancelExternalValidation")?;
    log_request_data(&Request::new(rpc::CancelExternalValidationRequest {
        details: "[redacted]".to_string(),
        ..request.get_ref().clone()
    }));
    let req = request.into_inner();
    let id = req
        .request_id
        .ok_or(CarbideError::MissingArgument("request_id"))?;
    tenant(&req.tenant_org_id)?;
    details(Some(&req.details), true)?;
    let mut txn = api.txn_begin().await?;
    let original = lookup_request(&mut txn, id, &req.tenant_org_id, req.machine_id).await?;
    db::machine::lock_by_id_for_update(&mut txn, &MachineId::from(original.hold.machine_id))
        .await?;
    let hold = db::external_validation::lock_hold(&mut txn, original.hold.hold_id).await?;
    let attempt = db::external_validation::lock_attempt(&mut txn, id).await?;
    if attempt.closure_kind == Some(ClosureKind::Cancel)
        && attempt.details.as_deref() == Some(req.details.as_str())
    {
        txn.commit().await?;
        tracing::info!(%actor, request_id = %id, replayed = true, "external validation Cancel");
        return Ok(Response::new(attempt_rpc(&attempt)));
    }
    current_attempt(&hold, &attempt)?;
    if attempt.allocation_state != AllocationState::NotStarted {
        return Err(CarbideError::FailedPrecondition(
            "allocation must be NotStarted before cancellation".to_string(),
        )
        .into());
    }
    let now = db::machine::database_now(&mut txn).await?;
    let attempt = db::external_validation::close_attempt(
        &mut txn,
        id,
        AttemptState::Cancelled,
        ClosureKind::Cancel,
        Some(&req.details),
        now,
    )
    .await?;
    db::external_validation::transition_hold(&mut txn, hold.hold_id, HoldState::Pending, None, now)
        .await?;
    db::external_validation::record_audit(&mut txn, id, AuditAction::Cancel, &actor, now).await?;
    txn.commit().await?;
    tracing::info!(%actor, request_id = %id, replayed = false, "external validation Cancel");
    Ok(Response::new(attempt_rpc(&attempt)))
}

/// Records a terminal result only for the saved committed instance binding; cleanup still owns gate release.
pub(crate) async fn complete(
    api: &Api,
    request: Request<rpc::CompleteExternalValidationRequest>,
) -> Result<Response<rpc::ExternalValidationAttempt>, Status> {
    let actor = external_validation_actor(&request, "CompleteExternalValidation")?;
    log_request_data(&Request::new(rpc::CompleteExternalValidationRequest {
        details: request
            .get_ref()
            .details
            .as_ref()
            .map(|_| "[redacted]".to_string()),
        ..request.get_ref().clone()
    }));
    let req = request.into_inner();
    let id = req
        .request_id
        .ok_or(CarbideError::MissingArgument("request_id"))?;
    let instance_id = req
        .validation_instance_id
        .ok_or(CarbideError::MissingArgument("validation_instance_id"))?;
    tenant(&req.tenant_org_id)?;
    details(req.details.as_deref(), false)?;
    let state = match rpc::ExternalValidationOutcome::try_from(req.outcome) {
        Ok(rpc::ExternalValidationOutcome::Passed) => AttemptState::Passed,
        Ok(rpc::ExternalValidationOutcome::Failed) => AttemptState::Failed,
        Ok(rpc::ExternalValidationOutcome::Cancelled) => AttemptState::Cancelled,
        _ => {
            return Err(CarbideError::InvalidArgument(
                "outcome must be Passed, Failed, or Cancelled".to_string(),
            )
            .into());
        }
    };
    let mut txn = api.txn_begin().await?;
    let original = lookup_request(&mut txn, id, &req.tenant_org_id, req.machine_id).await?;
    db::machine::lock_by_id_for_update(&mut txn, &MachineId::from(original.hold.machine_id))
        .await?;
    let hold = db::external_validation::lock_hold(&mut txn, original.hold.hold_id).await?;
    let attempt = db::external_validation::lock_attempt(&mut txn, id).await?;
    if attempt.closure_kind == Some(ClosureKind::Complete)
        && attempt.state == state
        && attempt.validation_instance_id == Some(instance_id)
        && attempt.details == req.details
    {
        txn.commit().await?;
        tracing::info!(%actor, request_id = %id, replayed = true, "external validation Complete");
        return Ok(Response::new(attempt_rpc(&attempt)));
    }
    current_attempt(&hold, &attempt)?;
    if attempt.allocation_state != AllocationState::Created {
        return Err(CarbideError::FailedPrecondition(
            "completion requires a committed validation allocation".to_string(),
        )
        .into());
    }
    if attempt.validation_instance_id != Some(instance_id) {
        return Err(conflict(id).into());
    }
    let instance = db::instance::find_by_id_for_update(&mut txn, instance_id)
        .await?
        .ok_or_else(|| {
            CarbideError::FailedPrecondition("bound validation instance is missing".to_string())
        })?;
    if MachineId::from(instance.machine_id) != MachineId::from(hold.machine_id)
        || instance.tenant_organization_id.as_str() != hold.tenant_org_id
        || instance.deleted.is_some()
    {
        return Err(CarbideError::FailedPrecondition(
            "bound validation instance ownership or lifecycle is inconsistent".to_string(),
        )
        .into());
    }
    let now = db::machine::database_now(&mut txn).await?;
    let attempt = db::external_validation::close_attempt(
        &mut txn,
        id,
        state,
        ClosureKind::Complete,
        req.details.as_deref(),
        now,
    )
    .await?;
    db::external_validation::transition_hold(
        &mut txn,
        hold.hold_id,
        HoldState::AwaitingCleanup,
        Some(id),
        now,
    )
    .await?;
    db::external_validation::record_audit(&mut txn, id, AuditAction::Complete, &actor, now).await?;
    txn.commit().await?;
    tracing::info!(%actor, request_id = %id, replayed = false, "external validation Complete");
    Ok(Response::new(attempt_rpc(&attempt)))
}

async fn lookup_request(
    txn: &mut sqlx::PgConnection,
    id: ExternalValidationRequestId,
    tenant: &str,
    expected_machine: Option<StableHostMachineId>,
) -> CarbideResult<HoldWithAttempt> {
    let original = db::external_validation::find_by_request(txn, id)
        .await?
        .ok_or_else(|| request_not_found(id))?;
    owning_tenant(&original.hold, tenant)?;
    if expected_machine.is_some_and(|machine| machine != original.hold.machine_id) {
        return Err(request_not_found(id));
    }
    Ok(original)
}

fn request_not_found(id: ExternalValidationRequestId) -> CarbideError {
    CarbideError::NotFoundError {
        kind: "external validation request in the expected machine scope",
        id: id.to_string(),
    }
}

fn conflict(id: ExternalValidationRequestId) -> CarbideError {
    CarbideError::AlreadyFoundError {
        kind: "conflicting or stale external validation attempt",
        id: id.to_string(),
    }
}

fn current_attempt(hold: &Hold, attempt: &Attempt) -> CarbideResult<()> {
    if hold.removed_at.is_some()
        || attempt.state != AttemptState::Open
        || hold.current_attempt_id != Some(attempt.request_id)
        || matches!(hold.state, HoldState::Pending | HoldState::Satisfied)
    {
        return Err(conflict(attempt.request_id));
    }
    if hold.state != HoldState::AttemptOpen {
        return Err(CarbideError::FailedPrecondition(
            "hold requires cleanup or recovery before mutation".to_string(),
        ));
    }
    Ok(())
}

fn tenant(value: &str) -> CarbideResult<()> {
    if value.is_empty() {
        return Err(CarbideError::InvalidArgument(
            "tenant_org_id must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn optional_tenant(value: Option<&str>) -> CarbideResult<()> {
    value.map(tenant).transpose().map(|_| ())
}

fn owning_tenant(hold: &Hold, supplied: &str) -> CarbideResult<()> {
    if hold.tenant_org_id != supplied {
        return Err(CarbideError::PermissionDeniedError(
            "tenant organization does not own the validation hold".to_string(),
        ));
    }
    Ok(())
}

fn details(value: Option<&str>, required: bool) -> CarbideResult<()> {
    if value.is_some_and(|value| value.len() > 4096)
        || (required && value.is_none_or(str::is_empty))
    {
        return Err(CarbideError::InvalidArgument(
            "details must be at most 4096 UTF-8 bytes and a cancellation reason must be nonempty"
                .to_string(),
        ));
    }
    Ok(())
}

fn hold_state(value: i32) -> CarbideResult<HoldState> {
    match rpc::ExternalValidationHoldState::try_from(value) {
        Ok(rpc::ExternalValidationHoldState::Pending) => Ok(HoldState::Pending),
        Ok(rpc::ExternalValidationHoldState::AttemptOpen) => Ok(HoldState::AttemptOpen),
        Ok(rpc::ExternalValidationHoldState::AwaitingCleanup) => Ok(HoldState::AwaitingCleanup),
        Ok(rpc::ExternalValidationHoldState::Satisfied) => Ok(HoldState::Satisfied),
        Ok(rpc::ExternalValidationHoldState::Recovery) => Ok(HoldState::Recovery),
        _ => Err(CarbideError::InvalidArgument(
            "state must be a specified hold state".to_string(),
        )),
    }
}

fn start_response(
    machine: StableHostMachineId,
    hold: Option<&Hold>,
    attempt: Option<&Attempt>,
    status: rpc::ExternalValidationStartStatus,
    replayed: bool,
) -> rpc::StartExternalValidationResponse {
    rpc::StartExternalValidationResponse {
        status: status.into(),
        machine_id: Some(machine),
        hold_id: hold.map(|hold| hold.hold_id),
        attempt: attempt.map(attempt_rpc),
        replayed,
    }
}

fn attempt_rpc(attempt: &Attempt) -> rpc::ExternalValidationAttempt {
    let state = match attempt.state {
        AttemptState::Open => rpc::ExternalValidationAttemptState::Open,
        AttemptState::Passed => rpc::ExternalValidationAttemptState::Passed,
        AttemptState::Failed => rpc::ExternalValidationAttemptState::Failed,
        AttemptState::Cancelled => rpc::ExternalValidationAttemptState::Cancelled,
    };
    let allocation = match attempt.allocation_state {
        AllocationState::NotStarted => rpc::ExternalValidationAllocationState::NotStarted,
        AllocationState::Creating => rpc::ExternalValidationAllocationState::Creating,
        AllocationState::Created => rpc::ExternalValidationAllocationState::Created,
    };
    rpc::ExternalValidationAttempt {
        request_id: Some(attempt.request_id),
        hold_id: Some(attempt.hold_id),
        caller_idempotency_key: attempt.caller_idempotency_key.to_string(),
        state: state.into(),
        allocation_state: allocation.into(),
        validation_instance_id: attempt.validation_instance_id,
        opened_at: Some(attempt.opened_at.into()),
        details: attempt.details.clone(),
        closed_at: attempt.closed_at.map(Into::into),
    }
}

fn hold_rpc(record: HoldWithAttempt) -> rpc::ExternalValidationHold {
    let hold = record.hold;
    let state = match hold.state {
        HoldState::Pending => rpc::ExternalValidationHoldState::Pending,
        HoldState::AttemptOpen => rpc::ExternalValidationHoldState::AttemptOpen,
        HoldState::AwaitingCleanup => rpc::ExternalValidationHoldState::AwaitingCleanup,
        HoldState::Satisfied => rpc::ExternalValidationHoldState::Satisfied,
        HoldState::Recovery => rpc::ExternalValidationHoldState::Recovery,
    };
    rpc::ExternalValidationHold {
        hold_id: Some(hold.hold_id),
        machine_id: Some(hold.machine_id),
        tenant_org_id: hold.tenant_org_id,
        validation_cycle_id: Some(hold.validation_cycle_id),
        state: state.into(),
        created_at: Some(hold.created_at.into()),
        attempt: record.attempt.as_ref().map(attempt_rpc),
        recovery_reason: hold.recovery_reason,
        state_changed_at: Some(hold.state_changed_at.into()),
    }
}
