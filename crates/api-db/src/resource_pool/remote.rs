// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Remember remote allocation requests before contacting their provider.
//!
//! `begin` saves the shared allocation row in the caller's transaction. Its
//! inner savepoint does not commit that transaction; the caller must commit
//! before provider I/O, separately from entity attachment. Subsequent writes
//! use the caller's connection or transaction. Attachment and release belong
//! in the entity's transaction.
//! Terminal rows retain the request and result; deleting an entity must not
//! discard work still owed to a provider.

use std::num::NonZeroU16;

use model::resource_pool::allocation::{AllocationSource, AllocationState, ResourcePoolAllocation};
use model::resource_pool::remote::{
    AllocationAssignment, AllocationResult, CleanupConfirmation, CompletionAuthority,
    RemoteAllocationParameters, RemoteAllocationRequest,
};
use sqlx::types::Json;
use sqlx::{PgConnection, PgTransaction};
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, DatabaseError, Transaction};

/// `RemoteAllocationError` explains why an allocation request could not be saved.
/// If a different request conflicts with both a saved generation and an unfinished
/// owner slot, PostgreSQL may report either `GenerationExists` or `SlotInUse`.
#[derive(Debug, thiserror::Error)]
pub enum RemoteAllocationError {
    /// A database read, write, or commit failed.
    #[error(transparent)]
    Database(#[from] DatabaseError),
    /// The same request ID was reused with different inputs or completion authority.
    #[error("remote allocation request {0} already has different inputs")]
    RequestConflict(Uuid),
    /// The binding is absent, is not gRPC, or names a different logical pool or authority.
    #[error("remote allocation request does not match pool binding {0}")]
    BindingMismatch(Uuid),
    /// Another request already used this owner, slot, and generation.
    #[error("the owner slot already has an allocation for this generation")]
    GenerationExists(#[source] DatabaseError),
    /// Another request is still pending or awaiting confirmed cancellation.
    #[error("the owner slot has an unfinished allocation request")]
    SlotInUse(#[source] DatabaseError),
}

/// `OperationNotCurrent` means the remote allocation is missing or the write's
/// state, assignment, provider handle, authority, or deadline checks failed.
/// It does not distinguish those cases; callers can reload the request to decide
/// what to do next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationNotCurrent;

/// `begin` saves a request in the caller's transaction. Commit that transaction
/// before provider I/O; no transaction may span the provider call.
/// The shared allocation UUID is the request UUID, from `Pending` through cleanup.
/// An identical retry returns the original record, even after cleanup. Changing
/// any request input or completion authority fails without replacing that record.
/// The stored row must decode and match before this helper returns.
///
/// Each owner slot identifies one purpose across pools, e.g. `vni` for a VPC or
/// `loopback:<vpc-id>` for a DPU. A new generation cannot bypass an older `Pending`
/// or `CancelRequested` attempt, including by switching to another pool binding.
/// This also applies to a saved failure: the slot stays blocked until cancellation
/// is confirmed. If both generation and slot constraints conflict, either
/// `GenerationExists` or `SlotInUse` can be returned.
/// Use a separate transaction from entity attachment. This helper checks the
/// binding's backend kind, logical pool, and authority. Provider admission,
/// eligibility, value domain, and resolved provider pool remain the caller's
/// responsibility.
/// String inputs must satisfy the text contract in [`model::resource_pool::remote`].
///
/// This write does not include an entity or controller desire write. Persist any
/// controller desire first; attachment must check that it is still current.
pub async fn begin(
    txn: &mut PgTransaction<'_>,
    request: &RemoteAllocationRequest,
    completion_authority: &CompletionAuthority,
) -> Result<ResourcePoolAllocation, RemoteAllocationError> {
    let parameters = RemoteAllocationParameters {
        authority_id: request.pool.authority_id.clone(),
        pool_id: request.pool.pool_id.clone(),
        slot: request.owner.slot.clone(),
        generation: request.owner.generation,
        eligibility_revision: request.eligibility_revision.clone(),
        selection: request.selection.clone(),
        completion_authority: completion_authority.clone(),
    };
    let mut inner_txn = Transaction::begin_inner(txn).await?;
    let query = "INSERT INTO resource_pool_allocations
        (allocation_id, binding_id, pool_name, owner_type, owner_id, remote_request, state)
        SELECT $1, binding_id, pool_name, $3, $4, $5, 'pending'
        FROM resource_pool_binding
        WHERE binding_id = $2 AND authority_id = $6 AND pool_name = $7
          AND backend_kind = 'grpc'
        ON CONFLICT (allocation_id) DO NOTHING";
    let inserted = sqlx::query(query)
        .bind(request.request_id)
        .bind(request.binding_id)
        .bind(request.owner.owner_type.to_string())
        .bind(&request.owner.owner_id)
        .bind(Json(&parameters))
        .bind(&request.pool.authority_id)
        .bind(&request.pool.logical_pool)
        .execute(inner_txn.as_pgconn())
        .await;

    if let Err(error) = inserted {
        let error = DatabaseError::query(query, error);
        let conflict = if error.violates_constraint("resource_pool_allocations_owner_key") {
            RemoteAllocationError::GenerationExists(error)
        } else if error.violates_constraint("resource_pool_allocations_pending_slot") {
            RemoteAllocationError::SlotInUse(error)
        } else {
            return Err(error.into());
        };

        // Concurrent identical inserts can hit a secondary unique constraint
        // before ON CONFLICT handles their request ID. Roll back the savepoint
        // before reading the winner, keeping the caller's fence and timeout.
        inner_txn.rollback().await?;
        let Some(stored) = load_allocation(&mut **txn, request.request_id).await? else {
            return Err(conflict);
        };
        return check_replay(stored, request, &parameters);
    }

    // A concurrent insert may have supplied the row. This separate statement
    // sees its committed inputs under READ COMMITTED before comparing them.
    let stored = load_allocation(inner_txn.as_pgconn(), request.request_id)
        .await?
        .ok_or(RemoteAllocationError::BindingMismatch(request.binding_id))?;
    let stored = check_replay(stored, request, &parameters)?;
    inner_txn.commit().await?;
    Ok(stored)
}

fn check_replay(
    stored: ResourcePoolAllocation,
    request: &RemoteAllocationRequest,
    parameters: &RemoteAllocationParameters,
) -> Result<ResourcePoolAllocation, RemoteAllocationError> {
    let AllocationSource::Remote {
        binding_id,
        request: saved_parameters,
        ..
    } = &stored.source
    else {
        return Err(RemoteAllocationError::RequestConflict(request.request_id));
    };
    if *binding_id != request.binding_id
        || stored.pool_name != request.pool.logical_pool
        || stored.owner_type != request.owner.owner_type
        || stored.owner_id != request.owner.owner_id
        || saved_parameters.as_ref() != parameters
    {
        return Err(RemoteAllocationError::RequestConflict(request.request_id));
    }
    Ok(stored)
}

/// `load` returns one saved request, including its original provider result and
/// terminal cleanup state, or `None` if no remote request uses this ID.
pub async fn load(
    db: impl DbReader<'_>,
    request_id: Uuid,
) -> Result<Option<ResourcePoolAllocation>, DatabaseError> {
    Ok(load_allocation(db, request_id)
        .await?
        .filter(|allocation| matches!(&allocation.source, AllocationSource::Remote { .. })))
}

// `begin` must also see an Integrated row with this UUID to reject its reuse.
async fn load_allocation(
    db: impl DbReader<'_>,
    allocation_id: Uuid,
) -> Result<Option<ResourcePoolAllocation>, DatabaseError> {
    let query = "SELECT allocation_id, pool_name, owner_type, owner_id, state,
        created_at, updated_at, integrated_resource_id, value, value_type,
        auto_assign, allocated_at, binding_id, remote_request, result
        FROM resource_pool_allocations WHERE allocation_id = $1";
    sqlx::query_as(query)
        .bind(allocation_id)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `last_request_id` finds the end of a recovery pass for one state.
/// Keeping this ID across pages lets recovery finish a pass even while callers
/// keep adding requests. `None` means there are no remote requests in this state.
pub async fn last_request_id(
    db: impl DbReader<'_>,
    state: AllocationState,
) -> Result<Option<Uuid>, DatabaseError> {
    let query = "SELECT allocation_id FROM resource_pool_allocations
        WHERE binding_id IS NOT NULL AND state = $1
        ORDER BY allocation_id DESC LIMIT 1";
    sqlx::query_scalar(query)
        .bind(state)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `list` reads up to `limit` remote allocations in request-ID order for recovery.
/// Pass the last returned ID as `after`; `None` starts at the beginning. `through`
/// includes requests up to that ID, or has no upper limit when omitted. Recovery
/// uses `last_request_id` to keep new arrivals from extending a pass indefinitely.
/// State can change between pages, so callers must repeat completed passes.
/// An undecodable record fails the whole page; this helper never skips it.
pub async fn list(
    db: impl DbReader<'_>,
    state: AllocationState,
    after: Option<Uuid>,
    through: Option<Uuid>,
    limit: NonZeroU16,
) -> Result<Vec<ResourcePoolAllocation>, DatabaseError> {
    let mut query = sqlx::QueryBuilder::new(
        "SELECT allocation_id, pool_name, owner_type, owner_id, state,
        created_at, updated_at, integrated_resource_id, value, value_type,
        auto_assign, allocated_at, binding_id, remote_request, result
        FROM resource_pool_allocations
        WHERE binding_id IS NOT NULL AND state = ",
    );
    query.push_bind(state);
    // A direct cursor comparison lets prepared plans seek into the state's index.
    if let Some(after) = after {
        query.push(" AND allocation_id > ");
        query.push_bind(after);
    }
    if let Some(through) = through {
        query.push(" AND allocation_id <= ");
        query.push_bind(through);
    }
    query.push(" ORDER BY allocation_id LIMIT ");
    query.push_bind(i64::from(limit.get()));
    query
        .build_query_as()
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query.sql(), error))
}

/// `cleanup_backlog` counts remote allocations awaiting confirmed cancellation
/// or release across every provider authority. Terminal rows remain stored but
/// no longer contribute to the count.
pub async fn cleanup_backlog(db: impl DbReader<'_>) -> Result<i64, DatabaseError> {
    let query = "SELECT count(*) FROM resource_pool_allocations
        WHERE binding_id IS NOT NULL
          AND state IN ('cancel_requested', 'release_requested')";
    sqlx::query_scalar(query)
        .fetch_one(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `record_result` saves the first assignment or definite rejection while the
/// request is `Pending` or `CancelRequested`. An exact replay in either state is
/// accepted; a different result cannot overwrite it. Other states reject writes.
/// The allocation keeps its UUID and creation time from `begin`; an applied
/// write, including replay, advances `updated_at`. A rejection preserves its state
/// without an assignment and does not free the owner slot. Request cancellation
/// and confirm cleanup before starting another generation.
/// Commit this write separately if the result must survive an entity rollback.
///
/// Before saving, the caller must validate the response's echoed request, binding,
/// authority, and provider pool against the saved request. For an allocation,
/// also check its owner and eligibility revision. `AllocationResult` does not
/// contain those echoed fields. Save only a validated assignment. If it cannot
/// be attached, request cancellation first. An invalid response must not supply
/// a saved handle: cancellation uses the original request identity without it.
/// Saving a result during cancellation never reopens attachment. Unknown,
/// pending, and transport failures are not definite results.
/// Provider handles and failure messages must also satisfy the text contract in
/// [`model::resource_pool::remote`].
///
/// A reused provider handle fails with a database constraint error. If this write
/// uses a transaction, roll it back before requesting cancellation in a new one.
/// The bounded handle index also rejects a SHA-256 collision; the digest is never
/// used to identify an assignment or accept a replay.
pub async fn record_result(
    txn: &mut PgConnection,
    request_id: Uuid,
    result: &AllocationResult,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let query = "UPDATE resource_pool_allocations
        SET result = $2, updated_at = clock_timestamp()
        WHERE allocation_id = $1 AND binding_id IS NOT NULL
          AND state IN ('pending', 'cancel_requested')
          AND (result IS NULL OR result = $2)";
    let updated = sqlx::query(query)
        .bind(request_id)
        .bind(Json(result))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(updated.rows_affected() == 1))
}

/// `attach` associates the saved assignment with its entity in this transaction.
/// Call `record_result` first. Attachment requires `Pending` state, the exact saved
/// assignment and authority, and an unexpired foreground deadline. Cancellation
/// competes for the same row. A repeated attachment returns `NotApplied`.
///
/// The caller must recheck entity admission, current desired generation, and
/// deletion state here, and roll back its entity changes on `NotApplied`. A
/// controller's saved `desire_id` alone does not prove that desire still exists.
/// Recovery may cancel an abandoned foreground create; it may not create the
/// entity using that attempt's saved authority. Callers that also lock the entity
/// must account for the allocation row lock taken here and choose a consistent
/// order in attachment, deletion, and recovery paths. This helper does not lock
/// the entity.
pub async fn attach(
    txn: &mut PgTransaction<'_>,
    request_id: Uuid,
    completion_authority: &CompletionAuthority,
    assignment: &AllocationAssignment,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    // Wait for any earlier writer before checking the deadline. Checking time
    // in a statement that then waits for a lock can accept an expired attempt.
    let query = "SELECT allocation_id FROM resource_pool_allocations
        WHERE allocation_id = $1 AND binding_id IS NOT NULL FOR UPDATE";
    sqlx::query_scalar::<_, Uuid>(query)
        .bind(request_id)
        .fetch_optional(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    let query = "UPDATE resource_pool_allocations
        SET state = 'attached', updated_at = clock_timestamp()
        WHERE allocation_id = $1 AND binding_id IS NOT NULL AND state = 'pending'
          AND remote_request->'completion_authority' = $2 AND result = $3
          AND (remote_request #>> '{completion_authority,kind}' = 'controller'
            OR (remote_request #>> '{completion_authority,kind}' = 'foreground'
              AND (remote_request #>> '{completion_authority,attach_before}')::timestamptz
                > clock_timestamp()))";
    let result = sqlx::query(query)
        .bind(request_id)
        .bind(Json(completion_authority))
        .bind(Json(AllocationResult::Allocated(assignment.clone())))
        .execute(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(result.rows_affected() == 1))
}

/// `request_cancel` prevents an unattached request from attaching later.
/// `NotApplied` includes an assignment that already attached; that assignment
/// must follow its entity's normal removal path before requesting release.
/// Repeating cancellation after the state changes also returns `NotApplied`.
pub async fn request_cancel(
    txn: &mut PgConnection,
    request_id: Uuid,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let query = "UPDATE resource_pool_allocations
        SET state = 'cancel_requested', updated_at = clock_timestamp()
        WHERE allocation_id = $1 AND binding_id IS NOT NULL AND state = 'pending'";
    let result = sqlx::query(query)
        .bind(request_id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(result.rows_affected() == 1))
}

/// `request_cancel_if_expired` cancels an abandoned foreground attempt using the
/// database clock. An unexpired attempt, controller request, or non-pending row
/// returns `NotApplied`. The caller keeps this write in its fenced transaction.
pub async fn request_cancel_if_expired(
    txn: &mut PgTransaction<'_>,
    request_id: Uuid,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    // As in `attach`, take the row lock before checking the deadline. A request
    // that expires while we wait must be cancelled on this iteration.
    let query = "SELECT allocation_id FROM resource_pool_allocations
        WHERE allocation_id = $1 AND binding_id IS NOT NULL FOR UPDATE";
    sqlx::query_scalar::<_, Uuid>(query)
        .bind(request_id)
        .fetch_optional(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    let query = "UPDATE resource_pool_allocations
        SET state = 'cancel_requested', updated_at = clock_timestamp()
        WHERE allocation_id = $1 AND binding_id IS NOT NULL AND state = 'pending'
          AND remote_request #>> '{completion_authority,kind}' = 'foreground'
          AND (remote_request #>> '{completion_authority,attach_before}')::timestamptz
            <= clock_timestamp()";
    let result = sqlx::query(query)
        .bind(request_id)
        .execute(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(result.rows_affected() == 1))
}

/// `request_release` saves cleanup of one attached allocation alongside the
/// entity change that stops using it. The caller must first satisfy its drain
/// or retention checks. The original request ID and `provider_allocation_id`
/// select this exact assignment, even if a later generation has reused its owner
/// slot or value. The provider handle is distinct from NICo's allocation UUID.
/// Pass the original validated handle unchanged.
/// Repeating release after the state changes returns `NotApplied`.
pub async fn request_release(
    txn: &mut PgTransaction<'_>,
    request_id: Uuid,
    provider_allocation_id: &str,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let query = "UPDATE resource_pool_allocations
        SET state = 'release_requested', updated_at = clock_timestamp()
        WHERE allocation_id = $1 AND binding_id IS NOT NULL AND state = 'attached'
          AND result #>> '{result,allocation_id}' = $2";
    let result = sqlx::query(query)
        .bind(request_id)
        .bind(provider_allocation_id)
        .execute(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(result.rows_affected() == 1))
}

/// `confirm_cleanup` records a provider's confirmed terminal state while keeping
/// the allocation's UUID, original request, and result.
/// Provider cancellation can confirm either `Cancelled` or `Released`; release
/// cleanup requires `Released`. Repeating the same confirmation is accepted and
/// advances `updated_at`. `NotFound`, `Release.applied = false`, or an unknown
/// response does not establish either confirmation; the caller must recover the
/// original request until the provider confirms cleanup.
///
/// The caller must obtain confirmation before opening this transaction; no
/// provider I/O belongs in it.
pub async fn confirm_cleanup(
    txn: &mut PgTransaction<'_>,
    request_id: Uuid,
    confirmation: CleanupConfirmation,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let state = match confirmation {
        CleanupConfirmation::Cancelled => AllocationState::Cancelled,
        CleanupConfirmation::Released => AllocationState::Released,
    };
    let query = "UPDATE resource_pool_allocations
        SET state = $2, updated_at = clock_timestamp()
        WHERE allocation_id = $1 AND binding_id IS NOT NULL
          AND (state = 'cancel_requested'
            OR (state = 'release_requested' AND $2 = 'released') OR state = $2)";
    let updated = sqlx::query(query)
        .bind(request_id)
        .bind(state)
        .execute(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(updated.rows_affected() == 1))
}

fn applied(updated: bool) -> ConditionalWrite<(), OperationNotCurrent> {
    if updated {
        ConditionalWrite::Applied(())
    } else {
        ConditionalWrite::NotApplied(OperationNotCurrent)
    }
}

#[cfg(test)]
mod tests;
