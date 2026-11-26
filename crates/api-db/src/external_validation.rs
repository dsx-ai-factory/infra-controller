// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Persistence for external-validation gates, immutable replay identities, and mutation audits.
//! Writers lock machine, hold, then attempt, and commit state and audit together.

use carbide_uuid::external_validation::{
    ExternalValidationCycleId, ExternalValidationHoldId, ExternalValidationRequestId,
};
use carbide_uuid::machine::{MachineId, StableHostMachineId};
use chrono::{DateTime, Utc};
use model::external_validation::{
    AllocationState, Attempt, AttemptState, AuditAction, ClosureKind, Hold, HoldFilter, HoldState,
    HoldWithAttempt,
};
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const HOLD_COLUMNS: &str = "h.hold_id, h.machine_id, h.tenant_org_id, h.validation_cycle_id,
    h.health_report_source, h.state, h.created_at, h.state_changed_at, h.current_attempt_id, h.recovery_reason, h.removed_at";

#[derive(sqlx::FromRow)]
struct HoldRow {
    #[sqlx(flatten)]
    hold: Hold,
    attempt: Option<Json<Attempt>>,
}

impl From<HoldRow> for HoldWithAttempt {
    fn from(row: HoldRow) -> Self {
        Self {
            hold: row.hold,
            attempt: row.attempt.map(|attempt| attempt.0),
        }
    }
}

/// Creates NICo's Pending gate for an existing host. No tenant RPC exposes this operation.
/// The machine row is locked until commit, so lifecycle admission can share this transaction.
/// Active-machine or same-cycle duplicates are rejected, not silently rebound.
pub async fn create_hold(
    txn: &mut PgConnection,
    machine_id: StableHostMachineId,
    tenant_org_id: &str,
    cycle_id: ExternalValidationCycleId,
    health_report_source: &str,
) -> DatabaseResult<Hold> {
    crate::machine::lock_by_id_for_update(&mut *txn, &MachineId::from(machine_id)).await?;
    let query = "WITH clock AS (SELECT clock_timestamp() AS now)
        INSERT INTO external_validation_holds
            (hold_id, machine_id, tenant_org_id, validation_cycle_id, health_report_source,
             state, created_at, state_changed_at)
        SELECT $1, m.id, $3, $4, $5, 'pending', clock.now, clock.now
        FROM machines m CROSS JOIN clock WHERE m.id = $2
        RETURNING hold_id, machine_id, tenant_org_id, validation_cycle_id, health_report_source,
                  state, created_at, state_changed_at, current_attempt_id, recovery_reason, removed_at";
    sqlx::query_as(query)
        .bind(ExternalValidationHoldId::new())
        .bind(machine_id)
        .bind(tenant_org_id)
        .bind(cycle_id)
        .bind(health_report_source)
        .fetch_optional(txn)
        .await
        .map_err(|error| write_error(query, error))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })
}

/// Returns all matching IDs ordered by immutable creation time and UUID byte order.
pub async fn find_ids(
    db: impl DbReader<'_>,
    filter: &HoldFilter,
) -> DatabaseResult<Vec<ExternalValidationHoldId>> {
    const QUERY: &str = "SELECT hold_id FROM external_validation_holds
        WHERE removed_at IS NULL AND ($1::text IS NULL OR tenant_org_id = $1)
          AND ($2::varchar IS NULL OR machine_id = $2)
          AND (($3::text IS NULL AND state <> 'satisfied') OR state = $3)
        ORDER BY created_at, hold_id";
    sqlx::query_scalar(QUERY)
        .bind(filter.tenant_org_id.as_deref())
        .bind(filter.machine_id)
        .bind(filter.state)
        .fetch_all(db)
        .await
        .map_err(|error| DatabaseError::query(QUERY, error))
}

/// Returns unique current holds and their current attempts in one statement, including Satisfied.
/// Missing or tenant-mismatched identities are omitted; the caller enforces its input batch limit.
pub async fn find_by_ids(
    db: impl DbReader<'_>,
    ids: &[ExternalValidationHoldId],
    tenant: Option<&str>,
) -> DatabaseResult<Vec<HoldWithAttempt>> {
    let query = format!("SELECT {HOLD_COLUMNS}, to_jsonb(a) AS attempt
        FROM external_validation_holds h LEFT JOIN external_validation_attempts a ON a.request_id = h.current_attempt_id
        WHERE h.removed_at IS NULL AND h.hold_id = ANY($1) AND ($2::text IS NULL OR h.tenant_org_id = $2)
        ORDER BY h.created_at, h.hold_id");
    sqlx::query_as::<_, HoldRow>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(ids)
        .bind(tenant)
        .fetch_all(db)
        .await
        .map(|rows| rows.into_iter().map(Into::into).collect())
        .map_err(|error| DatabaseError::query(&query, error))
}

/// Serializes one tenant/key namespace before machine locks, including keys used on different hosts.
/// Hash collisions only serialize unrelated calls; the unique constraint still owns key identity.
pub async fn lock_start_key(txn: &mut PgConnection, tenant: &str, key: Uuid) -> DatabaseResult<()> {
    const QUERY: &str = "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))";
    sqlx::query(QUERY)
        .bind(format!(
            "external-validation-start:{}:{tenant}:{key}",
            tenant.len()
        ))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(QUERY, error))?;
    Ok(())
}

/// Finds an original keyed operation before checking today's host eligibility or current attempt.
pub async fn find_by_key(
    db: impl DbReader<'_>,
    tenant: &str,
    key: Uuid,
) -> DatabaseResult<Option<HoldWithAttempt>> {
    let query = format!("SELECT {HOLD_COLUMNS}, to_jsonb(a) AS attempt
        FROM external_validation_holds h JOIN external_validation_attempts a ON a.hold_id = h.hold_id
        WHERE a.tenant_org_id = $1 AND a.caller_idempotency_key = $2");
    sqlx::query_as::<_, HoldRow>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(tenant)
        .bind(key)
        .fetch_optional(db)
        .await
        .map(|row| row.map(Into::into))
        .map_err(|error| DatabaseError::query(&query, error))
}

/// Resolves a request to its original hold and attempt, never the hold's newer current attempt.
pub async fn find_by_request(
    db: impl DbReader<'_>,
    id: ExternalValidationRequestId,
) -> DatabaseResult<Option<HoldWithAttempt>> {
    let query = format!("SELECT {HOLD_COLUMNS}, to_jsonb(a) AS attempt
        FROM external_validation_holds h JOIN external_validation_attempts a ON a.hold_id = h.hold_id
        WHERE a.request_id = $1");
    sqlx::query_as::<_, HoldRow>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .fetch_optional(db)
        .await
        .map(|row| row.map(Into::into))
        .map_err(|error| DatabaseError::query(&query, error))
}

/// Locks an active host gate after its machine row has been locked.
pub async fn lock_active_hold(
    txn: &mut PgConnection,
    machine: StableHostMachineId,
) -> DatabaseResult<Option<Hold>> {
    let query = format!(
        "SELECT {HOLD_COLUMNS} FROM external_validation_holds h
        WHERE h.machine_id = $1 AND h.state <> 'satisfied' AND h.removed_at IS NULL FOR UPDATE OF h"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(machine)
        .fetch_optional(txn)
        .await
        .map_err(|error| DatabaseError::query(&query, error))
}

/// Locks the original gate, including a terminal gate retained for replay.
pub async fn lock_hold(
    txn: &mut PgConnection,
    id: ExternalValidationHoldId,
) -> DatabaseResult<Hold> {
    let query = format!(
        "SELECT {HOLD_COLUMNS} FROM external_validation_holds h WHERE h.hold_id = $1 FOR UPDATE OF h"
    );
    sqlx::query_as(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(&query, error))
}

/// Locks an attempt after its original machine and hold have been locked.
pub async fn lock_attempt(
    txn: &mut PgConnection,
    id: ExternalValidationRequestId,
) -> DatabaseResult<Attempt> {
    const QUERY: &str = "SELECT request_id, hold_id, tenant_org_id, caller_idempotency_key, state,
        allocation_state, validation_instance_id, opened_at, details, closed_at, closure_kind
        FROM external_validation_attempts WHERE request_id = $1 FOR UPDATE";
    sqlx::query_as(QUERY)
        .bind(id)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(QUERY, error))
}

/// Inserts a new Open/NotStarted attempt under the locked Pending gate.
pub async fn open_attempt(
    txn: &mut PgConnection,
    hold: &Hold,
    key: Uuid,
    now: DateTime<Utc>,
) -> DatabaseResult<Attempt> {
    const QUERY: &str = "INSERT INTO external_validation_attempts
        (request_id, hold_id, tenant_org_id, caller_idempotency_key, state, allocation_state, opened_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING request_id, hold_id, tenant_org_id, caller_idempotency_key, state,
                  allocation_state, validation_instance_id, opened_at, details, closed_at, closure_kind";
    sqlx::query_as(QUERY)
        .bind(ExternalValidationRequestId::new())
        .bind(hold.hold_id)
        .bind(&hold.tenant_org_id)
        .bind(key)
        .bind(AttemptState::Open)
        .bind(AllocationState::NotStarted)
        .bind(now)
        .fetch_one(txn)
        .await
        .map_err(|error| write_error(QUERY, error))
}

/// Transitions only mutable gate fields under its row lock; unchanged states retain their age.
pub async fn transition_hold(
    txn: &mut PgConnection,
    id: ExternalValidationHoldId,
    state: HoldState,
    attempt: Option<ExternalValidationRequestId>,
    now: DateTime<Utc>,
) -> DatabaseResult<()> {
    const QUERY: &str = "UPDATE external_validation_holds
        SET state_changed_at = CASE WHEN state = $2 THEN state_changed_at ELSE $4 END,
            state = $2, current_attempt_id = $3, recovery_reason = NULL
        WHERE hold_id = $1";
    let result = sqlx::query(QUERY)
        .bind(id)
        .bind(state)
        .bind(attempt)
        .bind(now)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(QUERY, error))?;
    if result.rows_affected() != 1 {
        return Err(DatabaseError::NotFoundError {
            kind: "external validation hold",
            id: id.to_string(),
        });
    }
    Ok(())
}

/// Closes the locked Open attempt once; immutable terminal replay is handled before this write.
pub async fn close_attempt(
    txn: &mut PgConnection,
    id: ExternalValidationRequestId,
    state: AttemptState,
    kind: ClosureKind,
    details: Option<&str>,
    now: DateTime<Utc>,
) -> DatabaseResult<Attempt> {
    const QUERY: &str = "UPDATE external_validation_attempts
        SET state = $2, closure_kind = $3, details = $4, closed_at = $5
        WHERE request_id = $1 AND state = 'open'
        RETURNING request_id, hold_id, tenant_org_id, caller_idempotency_key, state,
                  allocation_state, validation_instance_id, opened_at, details, closed_at, closure_kind";
    sqlx::query_as(QUERY)
        .bind(id)
        .bind(state)
        .bind(kind)
        .bind(details)
        .bind(now)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(QUERY, error))
}

/// Records the authenticated Core caller in the same transaction as a successful transition.
pub async fn record_audit(
    txn: &mut PgConnection,
    id: ExternalValidationRequestId,
    action: AuditAction,
    actor: &str,
    now: DateTime<Utc>,
) -> DatabaseResult<()> {
    const QUERY: &str = "INSERT INTO external_validation_audit (request_id, action, actor, recorded_at) VALUES ($1, $2, $3, $4)";
    sqlx::query(QUERY)
        .bind(id)
        .bind(action)
        .bind(actor)
        .bind(now)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(QUERY, error))?;
    Ok(())
}

fn write_error(query: &str, error: sqlx::Error) -> DatabaseError {
    if error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
    {
        DatabaseError::AlreadyFoundError {
            kind: "external validation identity",
            id: "conflicting hold, attempt, or idempotency key".to_string(),
        }
    } else {
        DatabaseError::query(query, error)
    }
}
