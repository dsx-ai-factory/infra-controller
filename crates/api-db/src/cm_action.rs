// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Durable CM log correlation. Pending requests keep their predecessor JSON;
//! readers associate them by their existing timestamp and resource identity.
//! Admitted controller state retains the action ID across retries and restarts.

use std::future::Future;

use carbide_uuid::machine::MachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use serde::{Deserialize, Serialize};
use sqlx::{Acquire, PgConnection, PgPool};
use trace_propagation::correlation::{self, TraceId};
use uuid::Uuid;

use crate::{DatabaseError, DatabaseResult};

/// The resource owning a maintenance action. Its deletion cascades to its actions.
#[derive(Clone, Copy)]
pub(crate) enum Target<'a> {
    /// Compute maintenance.
    Machine(&'a MachineId),
    /// Switch maintenance.
    Switch(&'a SwitchId),
    /// Power-shelf maintenance.
    PowerShelf(&'a PowerShelfId),
    /// Rack maintenance.
    Rack(&'a RackId),
}

impl Target<'_> {
    fn parts(self) -> (&'static str, &'static str, String, &'static str) {
        match self {
            Self::Machine(id) => (
                "machines",
                "machine_id",
                id.to_string(),
                "machine_maintenance_requested",
            ),
            Self::Switch(id) => (
                "switches",
                "switch_id",
                id.to_string(),
                "switch_maintenance_requested",
            ),
            Self::PowerShelf(id) => (
                "power_shelves",
                "power_shelf_id",
                id.to_string(),
                "power_shelf_maintenance_requested",
            ),
            Self::Rack(id) => (
                "racks",
                "rack_id",
                id.to_string(),
                "config->'maintenance_requested'",
            ),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Metadata {
    pub(crate) trace_id: TraceId,
}

/// Serialize firmware job replacement and retention, including pre-ingestion
/// devices which do not have an inventory row to lock.
pub(crate) async fn lock_firmware(
    txn: &mut PgConnection,
    bmc_mac: mac_address::MacAddress,
) -> DatabaseResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("cm_firmware:{bmc_mac}"))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query("cm_action::lock_firmware", error))?;
    Ok(())
}

pub(crate) async fn firmware(
    txn: &mut PgConnection,
    bmc_mac: mac_address::MacAddress,
    kind: crate::direct_dispatch_firmware_job::FirmwareJobKind,
    job_id: &str,
) -> DatabaseResult<Uuid> {
    // Re-saving an already tracked job must retain its original correlation,
    // even when a different status-poll request performs the save.
    sqlx::query_scalar("INSERT INTO cm_actions (id, bmc_mac, request_key, metadata) VALUES ($1, $2, $3, $4) ON CONFLICT (bmc_mac, request_key) WHERE bmc_mac IS NOT NULL DO UPDATE SET request_key = EXCLUDED.request_key RETURNING id")
        .bind(Uuid::new_v4()).bind(bmc_mac).bind(format!("{}:{job_id}", kind.as_str()))
        .bind(sqlx::types::Json(Metadata { trace_id: correlation::current_id().unwrap_or_else(TraceId::generate) }))
        .fetch_one(txn).await.map_err(|error| DatabaseError::query("cm_action::firmware", error))
}

pub(crate) async fn reconcile_firmware(
    txn: &mut PgConnection,
    bmc_mac: mac_address::MacAddress,
) -> DatabaseResult<()> {
    sqlx::query("UPDATE cm_actions a SET completed_at = statement_timestamp() WHERE bmc_mac = $1 AND completed_at IS NULL AND NOT EXISTS (SELECT 1 FROM direct_dispatch_firmware_update_jobs j WHERE j.action_id = a.id)")
        .bind(bmc_mac).execute(&mut *txn).await
        .map_err(|error| DatabaseError::query("cm_action::retire_firmware", error))?;
    sqlx::query("DELETE FROM cm_actions a WHERE bmc_mac = $1 AND completed_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM direct_dispatch_firmware_update_jobs j WHERE j.action_id = a.id) AND id NOT IN (SELECT id FROM cm_actions WHERE bmc_mac = $1 AND completed_at IS NOT NULL ORDER BY completed_at DESC, created_at DESC, id DESC LIMIT 20)")
        .bind(bmc_mac).execute(txn).await
        .map_err(|error| DatabaseError::query("cm_action::trim_firmware", error))?;
    Ok(())
}

/// Create one action in the request's transaction. Callers serialize against
/// other writers by locking the owning resource before publishing the request.
/// `requested_at` is the exact JSON timestamp, preserving nanosecond identity.
pub(crate) async fn create(
    txn: &mut PgConnection,
    target: Target<'_>,
    requested_at: &str,
    trace_id: TraceId,
) -> DatabaseResult<Uuid> {
    let (_, column, target_id, _) = target.parts();
    let query = format!(
        "INSERT INTO cm_actions (id, {column}, request_key, metadata) VALUES ($1, $2, $3, $4)"
    );
    let id = Uuid::new_v4();
    sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .bind(target_id)
        .bind(requested_at)
        .bind(sqlx::types::Json(Metadata { trace_id }))
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(&query, error))?;
    Ok(id)
}

/// Lock the resource until the surrounding transaction commits, including action
/// creation, replacement reconciliation, and history trimming.
pub(crate) async fn lock(txn: &mut PgConnection, target: Target<'_>) -> DatabaseResult<bool> {
    let (table, _, id, _) = target.parts();
    let query = format!("SELECT id FROM {table} WHERE id = $1 FOR UPDATE");
    sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .fetch_optional(txn)
        .await
        .map(|id| id.is_some())
        .map_err(|error| DatabaseError::query(&query, error))
}

/// Retire requests that were replaced or cleared, while retaining any request
/// already admitted by a controller. Keep the latest 20 completed records per
/// resource, plus every record still referenced by pending or admitted work.
pub(crate) async fn reconcile(txn: &mut PgConnection, target: Target<'_>) -> DatabaseResult<()> {
    let (table, column, id, pending) = target.parts();
    let admitted = if matches!(target, Target::Rack(_)) {
        "config->'maintenance_requested'->>'action_id'"
    } else {
        "controller_state->'request'->>'action_id'"
    };
    let live = format!(
        "EXISTS (SELECT 1 FROM {table} WHERE id = $1 AND ({pending}->>'requested_at' = a.request_key OR {admitted} = a.id::text OR controller_state->'request'->>'requested_at' = a.request_key))"
    );
    let retire = format!(
        "UPDATE cm_actions a SET completed_at = statement_timestamp() WHERE {column} = $1 AND completed_at IS NULL AND NOT ({live})"
    );
    sqlx::query(sqlx::AssertSqlSafe(retire.as_str()))
        .bind(&id)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(&retire, error))?;
    let trim = format!(
        "DELETE FROM cm_actions a WHERE {column} = $1 AND completed_at IS NOT NULL AND NOT ({live}) AND id NOT IN (SELECT id FROM cm_actions WHERE {column} = $1 AND completed_at IS NOT NULL ORDER BY completed_at DESC, created_at DESC, id DESC LIMIT 20)"
    );
    sqlx::query(sqlx::AssertSqlSafe(trim.as_str()))
        .bind(id)
        .execute(txn)
        .await
        .map_err(|error| DatabaseError::query(&trim, error))?;
    Ok(())
}

/// Mark the exact admitted action terminal, even if its pending request was
/// replaced. Completion and the controller transition share the caller's transaction.
pub(crate) async fn complete(
    txn: &mut PgConnection,
    action_id: Option<Uuid>,
) -> DatabaseResult<()> {
    if let Some(id) = action_id {
        sqlx::query("UPDATE cm_actions SET completed_at = statement_timestamp() WHERE id = $1 AND completed_at IS NULL")
            .bind(id).execute(txn).await
            .map_err(|error| DatabaseError::query("cm_action::complete", error))?;
    }
    Ok(())
}

/// Atomically publish a maintenance request and its log context. The resource
/// lock also serializes retention with other requests and completions.
pub(crate) async fn set_maintenance(
    connection: &mut PgConnection,
    target: Target<'_>,
    request: &impl Serialize,
) -> DatabaseResult<()> {
    let mut txn = connection
        .begin()
        .await
        .map_err(|error| DatabaseError::query("cm_action::begin", error))?;
    if !lock(&mut txn, target).await? {
        let (table, _, id, _) = target.parts();
        return Err(DatabaseError::NotFoundError { kind: table, id });
    }
    let (table, _, id, pending) = target.parts();
    let json = serde_json::to_value(request)
        .map_err(|error| DatabaseError::internal(error.to_string()))?;
    let requested_at = json
        .get("requested_at")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| DatabaseError::internal("maintenance request has no timestamp".into()))?;
    create(
        &mut txn,
        target,
        requested_at,
        correlation::current_id().unwrap_or_else(TraceId::generate),
    )
    .await?;
    let query = format!("UPDATE {table} SET {pending} = $2 WHERE id = $1");
    sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .bind(&json)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(&query, error))?;
    reconcile(&mut txn, target).await?;
    txn.commit()
        .await
        .map_err(|error| DatabaseError::query("cm_action::commit", error))?;
    Ok(())
}

/// Clear the exact completed request and finish its action. A replacement stays
/// pending; action identity never participates in predecessor JSON equality.
pub(crate) async fn clear_maintenance(
    connection: &mut PgConnection,
    target: Target<'_>,
    request: &impl Serialize,
    action_id: Option<Uuid>,
) -> DatabaseResult<crate::ConditionalWrite<(), crate::MaintenanceRequestNotCurrent>> {
    let mut txn = connection
        .begin()
        .await
        .map_err(|error| DatabaseError::query("cm_action::begin", error))?;
    if !lock(&mut txn, target).await? {
        txn.rollback()
            .await
            .map_err(|error| DatabaseError::query("cm_action::rollback", error))?;
        return Ok(crate::ConditionalWrite::NotApplied(
            crate::MaintenanceRequestNotCurrent,
        ));
    }
    let (table, _, id, pending) = target.parts();
    let mut json = serde_json::to_value(request)
        .map_err(|error| DatabaseError::internal(error.to_string()))?;
    if let Some(object) = json.as_object_mut() {
        object.remove("action_id");
    }
    let query = format!("UPDATE {table} SET {pending} = NULL WHERE id = $1 AND {pending} = $2");
    let cleared = sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
        .bind(id)
        .bind(json)
        .execute(&mut *txn)
        .await
        .map_err(|error| DatabaseError::query(&query, error))?
        .rows_affected()
        == 1;
    complete(&mut txn, action_id).await?;
    reconcile(&mut txn, target).await?;
    txn.commit()
        .await
        .map_err(|error| DatabaseError::query("cm_action::commit", error))?;
    Ok(if cleared {
        crate::ConditionalWrite::Applied(())
    } else {
        crate::ConditionalWrite::NotApplied(crate::MaintenanceRequestNotCurrent)
    })
}

/// Resolve the persisted trace ID. Legacy requests without an action remain
/// uncorrelated; missing or invalid referenced records return an error.
pub async fn trace_id(pool: &PgPool, action_id: Option<Uuid>) -> DatabaseResult<Option<TraceId>> {
    let Some(id) = action_id else { return Ok(None) };
    let metadata: sqlx::types::Json<Metadata> =
        sqlx::query_scalar("SELECT metadata FROM cm_actions WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .map_err(|error| DatabaseError::query("cm_action::trace_id", error))?;
    Ok(Some(metadata.0.trace_id))
}

/// Execute one attempt with its originating trace ID. Database lookup completes
/// before the future is polled, so external work holds no database connection.
pub async fn resume<F: Future>(
    pool: &PgPool,
    action_id: Option<Uuid>,
    future: F,
) -> DatabaseResult<F::Output> {
    let trace_id = trace_id(pool, action_id).await?;
    Ok(correlation::resume(trace_id, future).await)
}

#[cfg(test)]
mod tests {
    use model::switch::{SwitchControllerState, SwitchMaintenanceOperation};

    use super::*;

    #[crate::sqlx_test]
    async fn history_is_bounded_without_deleting_admitted_work(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = pool.acquire().await?;
        let switch = crate::test_support::switch::create_seeded_discovered(
            &mut connection,
            79,
            "action retention",
        )
        .await?;
        let id = switch.id;
        crate::switch::set_switch_maintenance_requested(
            &mut connection,
            id,
            "admitted",
            SwitchMaintenanceOperation::PowerOn,
        )
        .await?;
        let original = crate::switch::find_by_id(&mut connection, &id)
            .await?
            .unwrap()
            .switch_maintenance_requested
            .unwrap();
        let admitted = SwitchControllerState::maintenance_for_request(original.clone());
        sqlx::query("UPDATE switches SET controller_state = $1 WHERE id = $2")
            .bind(sqlx::types::Json(admitted))
            .bind(id)
            .execute(&mut *connection)
            .await?;
        for _ in 0..23 {
            crate::switch::set_switch_maintenance_requested(
                &mut connection,
                id,
                "replacement",
                SwitchMaintenanceOperation::PowerOn,
            )
            .await?;
        }
        let counts: (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE completed_at IS NULL), count(*) FILTER (WHERE completed_at IS NOT NULL) FROM cm_actions WHERE switch_id = $1")
            .bind(id).fetch_one(&mut *connection).await?;
        assert_eq!(counts, (2, 20));
        assert!(trace_id(&pool, original.action_id).await?.is_some());
        let metadata: serde_json::Value =
            sqlx::query_scalar("SELECT metadata FROM cm_actions WHERE id = $1")
                .bind(original.action_id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(metadata.as_object().unwrap().len(), 1);
        assert!(metadata.get("trace_id").is_some());

        // Completing the admitted operation never clears the newer pending request.
        assert!(matches!(
            crate::switch::clear_switch_maintenance_requested(&mut connection, id, &original)
                .await?,
            crate::ConditionalWrite::NotApplied(_)
        ));
        let done: bool =
            sqlx::query_scalar("SELECT completed_at IS NOT NULL FROM cm_actions WHERE id = $1")
                .bind(original.action_id)
                .fetch_one(&mut *connection)
                .await?;
        assert!(done);
        assert!(
            crate::switch::find_by_id(&mut connection, &id)
                .await?
                .unwrap()
                .switch_maintenance_requested
                .is_some()
        );

        sqlx::query("DELETE FROM switches WHERE id = $1")
            .bind(id)
            .execute(&mut *connection)
            .await?;
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cm_actions WHERE switch_id = $1")
                .bind(id)
                .fetch_one(&mut *connection)
                .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }

    #[crate::sqlx_test]
    async fn action_and_request_rollback_together(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let switch = {
            let mut connection = pool.acquire().await?;
            crate::test_support::switch::create_seeded_discovered(
                &mut connection,
                80,
                "action rollback",
            )
            .await?
        };
        let mut txn = pool.begin().await?;
        crate::switch::set_switch_maintenance_requested(
            &mut txn,
            switch.id,
            "rolled back",
            SwitchMaintenanceOperation::PowerOn,
        )
        .await?;
        txn.rollback().await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cm_actions WHERE switch_id = $1")
            .bind(switch.id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0);
        let mut connection = pool.acquire().await?;
        assert!(
            crate::switch::find_by_id(&mut connection, &switch.id)
                .await?
                .unwrap()
                .switch_maintenance_requested
                .is_none()
        );
        Ok(())
    }

    #[crate::sqlx_test]
    async fn firmware_actions_keep_origin_and_isolate_replacements(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use crate::direct_dispatch_firmware_job as jobs;
        let mac = "02:00:00:00:00:81".parse()?;
        let kind = jobs::FirmwareJobKind::FirmwareObject;
        let original = TraceId::generate();
        correlation::scope(original, jobs::save(&pool, mac, kind, "first")).await?;
        correlation::scope(
            TraceId::generate(),
            jobs::replace(&pool, mac, &[(kind, "first".into())]),
        )
        .await?;
        assert_eq!(
            jobs::trace_id(&pool, mac, kind, "first").await?,
            Some(original)
        );
        let replacement = TraceId::generate();
        correlation::scope(replacement, jobs::save(&pool, mac, kind, "next")).await?;
        assert_eq!(jobs::trace_id(&pool, mac, kind, "first").await?, None);
        assert_eq!(
            jobs::trace_id(&pool, mac, kind, "next").await?,
            Some(replacement)
        );
        jobs::finish(&pool, mac, kind, "first").await?;
        let completed: bool = sqlx::query_scalar("SELECT a.completed_at IS NOT NULL FROM cm_actions a JOIN direct_dispatch_firmware_update_jobs j ON j.action_id = a.id WHERE j.bmc_mac = $1").bind(mac).fetch_one(&pool).await?;
        assert!(
            !completed,
            "a stale completion cannot finish its replacement"
        );
        // A predecessor binary updates the job without knowing action_id.
        sqlx::query(
            "UPDATE direct_dispatch_firmware_update_jobs SET job_id = 'legacy' WHERE bmc_mac = $1",
        )
        .bind(mac)
        .execute(&pool)
        .await?;
        assert_eq!(jobs::trace_id(&pool, mac, kind, "legacy").await?, None);
        jobs::finish(&pool, mac, kind, "legacy").await?;
        let completed: bool = sqlx::query_scalar("SELECT a.completed_at IS NOT NULL FROM cm_actions a JOIN direct_dispatch_firmware_update_jobs j ON j.action_id = a.id WHERE j.bmc_mac = $1")
            .bind(mac).fetch_one(&pool).await?;
        assert!(
            !completed,
            "legacy replacement cannot finish an unrelated action"
        );
        correlation::scope(replacement, jobs::save(&pool, mac, kind, "next")).await?;
        jobs::finish(&pool, mac, kind, "next").await?;
        for index in 0..23 {
            jobs::save(&pool, mac, kind, &format!("job-{index}")).await?;
        }
        let counts: (i64, i64) = sqlx::query_as("SELECT count(*) FILTER (WHERE completed_at IS NULL), count(*) FILTER (WHERE completed_at IS NOT NULL) FROM cm_actions WHERE bmc_mac = $1").bind(mac).fetch_one(&pool).await?;
        assert_eq!(counts, (1, 20));
        jobs::delete(&pool, mac, kind).await?;
        assert_eq!(jobs::get(&pool, mac, kind).await?, None);
        Ok(())
    }
}
