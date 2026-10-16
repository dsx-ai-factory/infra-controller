// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Remember remote allocation requests before contacting their provider.
//!
//! `begin` commits the request independently of entity creation. The remaining
//! writes join the caller's transaction, so attaching an assignment or requesting
//! its release commits together with the entity change. Terminal records stay in
//! this table; deleting an entity must not discard work still owed to a provider.

use std::num::NonZeroU16;

use model::resource_pool::allocation::AssignmentMode;
use model::resource_pool::remote::{
    AllocationAssignment, AllocationResult, CleanupConfirmation, CompletionAuthority,
    RemoteAllocationOperation, RemoteAllocationRequest, RemoteOperationState,
};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, PgTransaction};
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{ConditionalWrite, DatabaseError, TransactionVending};

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
    /// The binding is absent or names a different logical pool or authority.
    #[error("remote allocation request does not match pool binding {0}")]
    BindingMismatch(Uuid),
    /// Another request already used this owner, slot, and generation.
    #[error("the owner slot already has an allocation for this generation")]
    GenerationExists(#[source] DatabaseError),
    /// Another request is still pending or awaiting confirmed cancellation.
    #[error("the owner slot has an unfinished allocation request")]
    SlotInUse(#[source] DatabaseError),
}

/// `OperationNotCurrent` means the record is missing or the write's state,
/// assignment, allocation-handle, completion-authority, or deadline checks failed.
/// It does not distinguish those cases; callers can reload the request to decide
/// what to do next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationNotCurrent;

/// `begin` saves a request and commits it before returning, ready for provider I/O.
/// An identical retry returns the original record, even after cleanup. Changing
/// any request input or completion authority fails without replacing that record.
///
/// Each owner slot identifies one purpose across pools, e.g. `vni` for a VPC or
/// `loopback:<vpc-id>` for a DPU. A new generation cannot bypass an older Pending
/// or CancelRequested attempt, including by switching to another pool binding.
/// This also applies to a saved failure: the slot stays blocked until cancellation
/// is confirmed. If both generation and slot constraints conflict, either
/// `GenerationExists` or `SlotInUse` can be returned.
/// Call this before opening the entity transaction; no transaction may span the
/// subsequent provider call. Provider admission and eligibility checks belong to
/// the caller, not this storage helper. That includes checking the backend kind,
/// value domain, stable site identity, and resolved provider pool.
///
/// This commit does not include an entity or controller-desire write. Persist any
/// controller desire first; attachment must check that it is still current.
pub async fn begin(
    pool: &PgPool,
    request: &RemoteAllocationRequest,
    completion_authority: &CompletionAuthority,
) -> Result<RemoteAllocationOperation, RemoteAllocationError> {
    let mut txn = pool.txn_begin().await?;
    let query = "INSERT INTO resource_pool_operations
        (request_id, binding_id, pool, owner, eligibility_revision, selection,
         completion_authority, state)
        SELECT $1, binding_id, $3, $4, $5, $6, $7, 'pending'
        FROM resource_pool_binding
        WHERE binding_id = $2 AND authority_id = $8 AND pool_name = $9
        ON CONFLICT (request_id) DO NOTHING";
    let inserted = sqlx::query(query)
        .bind(request.request_id)
        .bind(request.binding_id)
        .bind(Json(&request.pool))
        .bind(Json(&request.owner))
        .bind(&request.eligibility_revision)
        .bind(Json(&request.selection))
        .bind(Json(completion_authority))
        .bind(&request.pool.authority_id)
        .bind(&request.pool.logical_pool)
        .execute(&mut **txn)
        .await;

    if let Err(error) = inserted {
        let error = DatabaseError::query(query, error);
        let conflict = if error.violates_constraint("resource_pool_operations_owner_key") {
            RemoteAllocationError::GenerationExists(error)
        } else if error.violates_constraint("resource_pool_operations_pending_slot") {
            RemoteAllocationError::SlotInUse(error)
        } else {
            return Err(error.into());
        };

        // Concurrent identical inserts can hit a secondary unique constraint
        // before ON CONFLICT handles their request ID. The failed transaction
        // cannot read, so roll it back before checking the winning request.
        txn.rollback().await?;
        let Some(stored) = load(pool, request.request_id).await? else {
            return Err(conflict);
        };
        return check_replay(stored, request, completion_authority);
    }

    // A concurrent insert may have supplied the row. This separate statement
    // sees its committed inputs under READ COMMITTED before comparing them.
    let stored = load(&mut **txn, request.request_id)
        .await?
        .ok_or(RemoteAllocationError::BindingMismatch(request.binding_id))?;
    let stored = check_replay(stored, request, completion_authority)?;
    txn.commit().await?;
    Ok(stored)
}

fn check_replay(
    stored: RemoteAllocationOperation,
    request: &RemoteAllocationRequest,
    completion_authority: &CompletionAuthority,
) -> Result<RemoteAllocationOperation, RemoteAllocationError> {
    if stored.request != *request || stored.completion_authority != *completion_authority {
        return Err(RemoteAllocationError::RequestConflict(request.request_id));
    }
    Ok(stored)
}

/// `load` returns one saved request, including its original provider result and
/// terminal cleanup state, or `None` if this request ID was never saved.
pub async fn load(
    db: impl DbReader<'_>,
    request_id: Uuid,
) -> Result<Option<RemoteAllocationOperation>, DatabaseError> {
    let query = "SELECT request_id, binding_id, pool, owner, eligibility_revision,
        selection, completion_authority, state, result, created_at, updated_at
        FROM resource_pool_operations WHERE request_id = $1";
    sqlx::query_as(query)
        .bind(request_id)
        .fetch_optional(db)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `list` reads a bounded page in request-ID order for recovery or inspection.
/// Pass the last returned ID as `after`; `None` starts a new scan. State can
/// change between pages, so recovery starts another scan on its next iteration.
/// An undecodable record fails the whole page; this helper never skips it.
pub async fn list(
    db: impl DbReader<'_>,
    state: RemoteOperationState,
    after: Option<Uuid>,
    limit: NonZeroU16,
) -> Result<Vec<RemoteAllocationOperation>, DatabaseError> {
    let mut query = sqlx::QueryBuilder::new(
        "SELECT request_id, binding_id, pool, owner, eligibility_revision,
        selection, completion_authority, state, result, created_at, updated_at
        FROM resource_pool_operations
        WHERE state = ",
    );
    query.push_bind(state);
    // A direct cursor comparison lets prepared plans seek into the state's index.
    if let Some(after) = after {
        query.push(" AND request_id > ");
        query.push_bind(after);
    }
    query.push(" ORDER BY request_id LIMIT ");
    query.push_bind(i64::from(limit.get()));
    query
        .build_query_as()
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(query.sql(), error))
}

/// `record_result` saves the first assignment or definite rejection while the
/// request is Pending or CancelRequested. An exact replay in either state is
/// accepted; a different result cannot overwrite it. Other states reject writes.
/// A successful assignment also creates its allocation record in this statement;
/// exact replay keeps that record's UUID and timestamp. A rejection creates no
/// allocation record and does not free the owner slot. Request cancellation and
/// confirm cleanup before starting another generation.
/// Commit this write separately if the result must survive an entity rollback.
///
/// Before saving, the caller must validate the response's echoed request, site,
/// authority, and provider pool against the saved request. For an allocation,
/// also check its owner and eligibility revision. `AllocationResult` does not
/// contain those echoed fields. If a matching assignment cannot be used, request
/// cancellation first, then save its provider
/// handle for cleanup. Saving a result during cancellation never reopens
/// attachment. Unknown, pending, and transport failures are not definite results.
///
/// A reused provider handle fails with a database constraint error. Roll back
/// that transaction before requesting cancellation in a fresh transaction.
pub async fn record_result(
    txn: &mut PgConnection,
    request_id: Uuid,
    result: &AllocationResult,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let assignment = match result {
        AllocationResult::Allocated(assignment) => Some(assignment),
        AllocationResult::Failed(_) => None,
    };
    let query = "WITH saved AS (
        UPDATE resource_pool_operations
        SET result = $2, updated_at = clock_timestamp()
        WHERE request_id = $1 AND state IN ('pending', 'cancel_requested')
          AND (result IS NULL OR result = $2)
        RETURNING request_id, pool, owner
    ), allocation_record AS (
        INSERT INTO resource_pool_allocations
            (pool_name, value, value_type, owner_type, owner_id, auto_assign,
             remote_request_id)
        SELECT pool->>'logical_pool', $3, $4, owner->>'owner_type',
               owner->>'owner_id', $5, request_id
        FROM saved WHERE $3::text IS NOT NULL
        ON CONFLICT (remote_request_id) DO NOTHING
    )
    SELECT EXISTS(SELECT 1 FROM saved)";
    let updated = sqlx::query_scalar::<_, bool>(query)
        .bind(request_id)
        .bind(Json(result))
        .bind(assignment.map(|assignment| assignment.value.to_string()))
        .bind(assignment.map(|assignment| assignment.value.value_type()))
        .bind(assignment.map(|assignment| assignment.assignment_mode == AssignmentMode::Automatic))
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(updated))
}

/// `attach` associates the saved assignment with its entity in this transaction.
/// Call `record_result` first. Attachment requires Pending state, the exact saved
/// assignment and authority, and an unexpired foreground deadline. Cancellation
/// competes for the same row. A repeated attachment returns `NotApplied`.
///
/// The caller must recheck entity admission, current desired generation, and
/// deletion state here, and roll back its entity changes on `NotApplied`. A
/// controller's saved `desire_id` alone does not prove that desire still exists.
/// Recovery may cancel an abandoned foreground create; it may not create the
/// entity using that attempt's saved authority. Callers that also lock the entity
/// must account for the operation row lock taken here and choose a consistent
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
    let query = "SELECT request_id FROM resource_pool_operations
        WHERE request_id = $1 FOR UPDATE";
    sqlx::query_scalar::<_, Uuid>(query)
        .bind(request_id)
        .fetch_optional(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    let query = "UPDATE resource_pool_operations
        SET state = 'attached', updated_at = clock_timestamp()
        WHERE request_id = $1 AND state = 'pending'
          AND completion_authority = $2 AND result = $3
          AND (completion_authority->>'kind' = 'controller'
            OR (completion_authority->>'kind' = 'foreground'
              AND (completion_authority->>'attach_before')::timestamptz > clock_timestamp()))";
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
    let query = "UPDATE resource_pool_operations
        SET state = 'cancel_requested', updated_at = clock_timestamp()
        WHERE request_id = $1 AND state = 'pending'";
    let result = sqlx::query(query)
        .bind(request_id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(result.rows_affected() == 1))
}

/// `request_release` saves cleanup of one attached allocation alongside the
/// entity change that stops using it. The caller must first satisfy its drain
/// or retention checks. The original request ID and `provider_allocation_id`
/// select this exact assignment, even if a later generation has reused its owner
/// slot or value. The provider handle is distinct from NICo's allocation UUID.
/// Repeating release after the state changes returns `NotApplied`.
pub async fn request_release(
    txn: &mut PgConnection,
    request_id: Uuid,
    provider_allocation_id: &str,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let query = "UPDATE resource_pool_operations
        SET state = 'release_requested', updated_at = clock_timestamp()
        WHERE request_id = $1 AND state = 'attached'
          AND result #>> '{result,allocation_id}' = $2";
    let result = sqlx::query(query)
        .bind(request_id)
        .bind(provider_allocation_id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(result.rows_affected() == 1))
}

/// `confirm_cleanup` records a provider's confirmed terminal state and removes
/// its allocation record atomically, retaining the original request and result.
/// Provider cancellation can confirm either Cancelled or Released; release
/// cleanup requires Released. The same confirmation is accepted, even after its
/// allocation record is gone. `NotFound`, `Release.applied = false`, or an unknown
/// response does not establish either confirmation; the caller must recover the
/// original request until the provider confirms cleanup.
///
/// This requires the caller's transaction so a lock taken before the cleanup
/// statement remains held through allocation deletion. No provider I/O belongs in
/// that transaction.
pub async fn confirm_cleanup(
    txn: &mut PgTransaction<'_>,
    request_id: Uuid,
    confirmation: CleanupConfirmation,
) -> Result<ConditionalWrite<(), OperationNotCurrent>, DatabaseError> {
    let state = match confirmation {
        CleanupConfirmation::Cancelled => RemoteOperationState::Cancelled,
        CleanupConfirmation::Released => RemoteOperationState::Released,
    };
    // Wait before taking the cleanup statement's snapshot. Otherwise, an
    // UPDATE can see a result committed while it waited for this row, but
    // DELETE still misses that result's newly inserted allocation record.
    let query = "SELECT request_id FROM resource_pool_operations
        WHERE request_id = $1 FOR UPDATE";
    sqlx::query_scalar::<_, Uuid>(query)
        .bind(request_id)
        .fetch_optional(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;

    let query = "WITH confirmed AS (
        UPDATE resource_pool_operations
        SET state = $2, updated_at = clock_timestamp()
        WHERE request_id = $1 AND (state = 'cancel_requested'
          OR (state = 'release_requested' AND $2 = 'released') OR state = $2)
        RETURNING request_id
    ), allocation_record AS (
        DELETE FROM resource_pool_allocations AS allocation
        USING confirmed WHERE allocation.remote_request_id = confirmed.request_id
    )
    SELECT EXISTS(SELECT 1 FROM confirmed)";
    let updated = sqlx::query_scalar::<_, bool>(query)
        .bind(request_id)
        .bind(state)
        .fetch_one(&mut **txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))?;
    Ok(applied(updated))
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
