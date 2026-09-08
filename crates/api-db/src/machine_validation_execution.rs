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

use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::{
    MachineValidationAttemptId, MachineValidationId, MachineValidationRunItemId,
};
use chrono::{DateTime, Utc};
use model::machine_validation::{
    MachineValidationAttempt, MachineValidationAttemptState, MachineValidationResult,
    MachineValidationRunItem, MachineValidationRunItemState, MachineValidationTest,
};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult, machine_validation_suites};

const DEFAULT_TIMEOUT_SECONDS: i64 = 7200;
// M1 persists Scout's existing sequential result stream as a single attempt per test.
// Retry-aware events will need to carry attempt identity before this can vary.
const INITIAL_ATTEMPT_NUMBER: i32 = 1;
const SUMMARY_LIMIT: usize = 4096;

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct StaleMachineValidationAttempt {
    pub machine_id: MachineId,
    pub validation_id: MachineValidationId,
    pub run_item_id: MachineValidationRunItemId,
    pub attempt_id: MachineValidationAttemptId,
    pub test_id: String,
    pub display_name: String,
    pub timeout_seconds: i64,
    pub started_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

impl crate::machine::MachineRowLockItem for StaleMachineValidationAttempt {
    fn machine_id(&self) -> MachineId {
        self.machine_id
    }
}

pub async fn materialize_run_plan(
    txn: &mut PgConnection,
    run_id: &MachineValidationId,
    context: &str,
    selected_tests: &[MachineValidationTest],
) -> DatabaseResult<()> {
    for (order_index, test) in selected_tests.iter().enumerate() {
        let order_index = i32::try_from(order_index).map_err(|_| {
            DatabaseError::InvalidArgument(
                "machine validation run has too many selected tests".to_string(),
            )
        })?;
        let run_item_id =
            upsert_run_item_from_test(txn, run_id, context, test, order_index).await?;
        upsert_pending_attempt(txn, &run_item_id, test).await?;
    }

    Ok(())
}

pub async fn find_run_items_by_run_id(
    txn: impl DbReader<'_>,
    run_id: &MachineValidationId,
) -> DatabaseResult<Vec<MachineValidationRunItem>> {
    const QUERY: &str = "
        SELECT
            run_item.*,
            current_attempt.id AS current_attempt_id
        FROM machine_validation_run_items run_item
        LEFT JOIN LATERAL (
            SELECT id
            FROM machine_validation_attempts attempt
            WHERE attempt.run_item_id=run_item.id
            ORDER BY attempt.attempt_number DESC
            LIMIT 1
        ) current_attempt ON true
        WHERE run_item.run_id=$1
        ORDER BY run_item.order_index, run_item.display_name";

    sqlx::query_as::<_, MachineValidationRunItem>(QUERY)
        .bind(run_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

pub async fn find_run_item_ids_by_run_id(
    txn: impl DbReader<'_>,
    run_id: &MachineValidationId,
) -> DatabaseResult<Vec<MachineValidationRunItemId>> {
    const QUERY: &str = "
        SELECT id
        FROM machine_validation_run_items
        WHERE run_id=$1
        ORDER BY order_index, display_name";

    sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
        .bind(run_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

pub async fn find_run_items_by_ids(
    txn: impl DbReader<'_>,
    ids: &[MachineValidationRunItemId],
) -> DatabaseResult<Vec<MachineValidationRunItem>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    const QUERY: &str = "
        SELECT
            run_item.*,
            current_attempt.id AS current_attempt_id
        FROM machine_validation_run_items run_item
        LEFT JOIN LATERAL (
            SELECT id
            FROM machine_validation_attempts attempt
            WHERE attempt.run_item_id=run_item.id
            ORDER BY attempt.attempt_number DESC
            LIMIT 1
        ) current_attempt ON true
        WHERE run_item.id=ANY($1)
        ORDER BY run_item.order_index, run_item.display_name";

    sqlx::query_as::<_, MachineValidationRunItem>(QUERY)
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

pub async fn find_attempt_by_id(
    txn: impl DbReader<'_>,
    id: &MachineValidationAttemptId,
) -> DatabaseResult<MachineValidationAttempt> {
    const QUERY: &str = "SELECT * FROM machine_validation_attempts WHERE id=$1";

    sqlx::query_as::<_, MachineValidationAttempt>(QUERY)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?
        .ok_or_else(|| DatabaseError::NotFoundError {
            kind: "machine_validation_attempt",
            id: id.to_string(),
        })
}

pub async fn find_attempts_by_run_item_id(
    txn: impl DbReader<'_>,
    run_item_id: &MachineValidationRunItemId,
) -> DatabaseResult<Vec<MachineValidationAttempt>> {
    const QUERY: &str = "
        SELECT * FROM machine_validation_attempts
        WHERE run_item_id=$1
        ORDER BY attempt_number";

    sqlx::query_as::<_, MachineValidationAttempt>(QUERY)
        .bind(run_item_id)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

pub async fn record_result(
    txn: &mut PgConnection,
    result: &MachineValidationResult,
) -> DatabaseResult<bool> {
    let run_item_id = upsert_run_item_from_result(txn, result).await?;
    let state = state_from_result(result);
    let stdout_summary = truncate_summary(&result.stdout);
    let stderr_summary = truncate_summary(&result.stderr);
    let failure_classification =
        (state == MachineValidationAttemptState::Failed).then(|| "CommandFailed".to_string());

    let updated_first_terminal = update_pending_attempt_from_result(
        txn,
        &run_item_id,
        result,
        &state,
        stdout_summary.as_deref(),
        stderr_summary.as_deref(),
        failure_classification.as_deref(),
    )
    .await?;

    let first_terminal = if updated_first_terminal {
        true
    } else {
        insert_terminal_attempt_from_result(
            txn,
            &run_item_id,
            result,
            &state,
            stdout_summary.as_deref(),
            stderr_summary.as_deref(),
            failure_classification.as_deref(),
        )
        .await?
    };

    if first_terminal {
        update_run_item_from_result(
            txn,
            &run_item_id,
            result,
            &state,
            stdout_summary.as_deref(),
            stderr_summary.as_deref(),
        )
        .await?;
    }

    Ok(first_terminal)
}

pub async fn record_heartbeat(
    txn: &mut PgConnection,
    validation_id: &MachineValidationId,
    run_item_id: Option<&MachineValidationRunItemId>,
    attempt_id: Option<&MachineValidationAttemptId>,
    test_id: Option<&str>,
    observed_at: DateTime<Utc>,
) -> DatabaseResult<bool> {
    let targets_run_item = run_item_id.is_some() || attempt_id.is_some() || test_id.is_some();
    let Some(run_item_id) =
        resolve_run_item_for_heartbeat(txn, validation_id, run_item_id, attempt_id, test_id)
            .await?
    else {
        return if targets_run_item {
            Ok(false)
        } else {
            update_run_heartbeat(txn, validation_id, observed_at).await
        };
    };

    if !update_run_heartbeat(txn, validation_id, observed_at).await? {
        return Ok(false);
    }

    if !update_run_item_heartbeat(txn, validation_id, &run_item_id, observed_at).await? {
        return Ok(false);
    }

    update_attempt_heartbeat(txn, &run_item_id, attempt_id, observed_at).await
}

pub async fn find_stale_active_attempts(
    txn: impl DbReader<'_>,
    stale_run_timeout: std::time::Duration,
    now: DateTime<Utc>,
) -> DatabaseResult<Vec<StaleMachineValidationAttempt>> {
    let stale_run_timeout_seconds = i64::try_from(stale_run_timeout.as_secs()).unwrap_or(i64::MAX);
    const QUERY: &str = "
        WITH active_attempts AS (
            SELECT
                validation.machine_id,
                run_item.run_id AS validation_id,
                run_item.id AS run_item_id,
                attempt.id AS attempt_id,
                run_item.test_id,
                run_item.display_name,
                run_item.timeout_seconds,
                attempt.started_at,
                attempt.last_heartbeat_at,
                COALESCE(
                    attempt.last_heartbeat_at,
                    attempt.started_at,
                    run_item.last_heartbeat_at
                ) AS heartbeat_reference
            FROM machine_validation_attempts attempt
            JOIN machine_validation_run_items run_item
                ON run_item.id=attempt.run_item_id
            JOIN machine_validation validation
                ON validation.id=run_item.run_id
            WHERE validation.end_time IS NULL
                AND validation.state IN ('Started', 'InProgress')
                AND run_item.state='Running'
                AND attempt.state='Running'
        )
        SELECT
            machine_id,
            validation_id,
            run_item_id,
            attempt_id,
            test_id,
            display_name,
            timeout_seconds,
            started_at,
            last_heartbeat_at
        FROM active_attempts
        WHERE (
            heartbeat_reference IS NOT NULL
            AND heartbeat_reference + ($1::bigint * INTERVAL '1 second') < $2
        ) OR (
            started_at IS NOT NULL
            AND started_at
                + (GREATEST(timeout_seconds, 0) * INTERVAL '1 second')
                + ($1::bigint * INTERVAL '1 second') < $2
        )
        ORDER BY validation_id, run_item_id";

    sqlx::query_as::<_, StaleMachineValidationAttempt>(QUERY)
        .bind(stale_run_timeout_seconds)
        .bind(now)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

pub async fn mark_attempt_stale_if_active(
    txn: &mut PgConnection,
    attempt_id: &MachineValidationAttemptId,
    now: DateTime<Utc>,
    failure_reason: &str,
) -> DatabaseResult<Option<MachineValidationId>> {
    const QUERY: &str = "
        WITH updated_attempt AS (
            UPDATE machine_validation_attempts
            SET
                state='Failed',
                failure_classification='StaleHeartbeat',
                ended_at=$2,
                stderr_summary=COALESCE(stderr_summary, $3)
            WHERE id=$1
                AND state='Running'
            RETURNING run_item_id
        ),
        updated_run_item AS (
            UPDATE machine_validation_run_items
            SET
                state='Failed',
                ended_at=$2,
                failure_reason=$3
            WHERE id=(SELECT run_item_id FROM updated_attempt)
                AND state='Running'
            RETURNING run_id
        )
        SELECT run_id FROM updated_run_item";

    sqlx::query_scalar::<_, MachineValidationId>(QUERY)
        .bind(attempt_id)
        .bind(now)
        .bind(truncate_summary(failure_reason).unwrap_or_default())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

async fn update_run_heartbeat(
    txn: &mut PgConnection,
    validation_id: &MachineValidationId,
    observed_at: DateTime<Utc>,
) -> DatabaseResult<bool> {
    const QUERY: &str = "
        UPDATE machine_validation
        SET
            last_heartbeat_at=$2,
            state='InProgress'
        WHERE id=$1
            AND end_time IS NULL
            AND state IN ('Started', 'InProgress')
        RETURNING id";

    let updated = sqlx::query_scalar::<_, MachineValidationId>(QUERY)
        .bind(validation_id)
        .bind(observed_at)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;
    Ok(updated.is_some())
}

async fn resolve_run_item_for_heartbeat(
    txn: &mut PgConnection,
    validation_id: &MachineValidationId,
    run_item_id: Option<&MachineValidationRunItemId>,
    attempt_id: Option<&MachineValidationAttemptId>,
    test_id: Option<&str>,
) -> DatabaseResult<Option<MachineValidationRunItemId>> {
    if let Some(attempt_id) = attempt_id {
        const QUERY: &str = "
            SELECT run_item.id
            FROM machine_validation_run_items run_item
            JOIN machine_validation_attempts attempt
                ON attempt.run_item_id=run_item.id
            WHERE run_item.run_id=$1
                AND attempt.id=$2";

        return sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
            .bind(validation_id)
            .bind(attempt_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(QUERY, e));
    }

    if let Some(run_item_id) = run_item_id {
        const QUERY: &str = "
            SELECT id
            FROM machine_validation_run_items
            WHERE run_id=$1
                AND id=$2";

        return sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
            .bind(validation_id)
            .bind(run_item_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(QUERY, e));
    }

    if let Some(test_id) = test_id {
        const QUERY: &str = "
            SELECT id
            FROM machine_validation_run_items
            WHERE run_id=$1
                AND test_id=$2";

        return sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
            .bind(validation_id)
            .bind(test_id)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(QUERY, e));
    }

    Ok(None)
}

async fn update_run_item_heartbeat(
    txn: &mut PgConnection,
    validation_id: &MachineValidationId,
    run_item_id: &MachineValidationRunItemId,
    observed_at: DateTime<Utc>,
) -> DatabaseResult<bool> {
    const QUERY: &str = "
        UPDATE machine_validation_run_items
        SET
            state='Running',
            attempt=GREATEST(attempt, $3),
            started_at=COALESCE(started_at, $4),
            last_heartbeat_at=$4
        WHERE id=$1
            AND run_id=$2
            AND state IN ('Pending', 'Running')
        RETURNING id";

    let updated = sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
        .bind(run_item_id)
        .bind(validation_id)
        .bind(INITIAL_ATTEMPT_NUMBER)
        .bind(observed_at)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;
    Ok(updated.is_some())
}

async fn update_attempt_heartbeat(
    txn: &mut PgConnection,
    run_item_id: &MachineValidationRunItemId,
    attempt_id: Option<&MachineValidationAttemptId>,
    observed_at: DateTime<Utc>,
) -> DatabaseResult<bool> {
    let updated = match attempt_id {
        Some(attempt_id) => {
            const QUERY: &str = "
                UPDATE machine_validation_attempts
                SET
                    state='Running',
                    started_at=COALESCE(started_at, $3),
                    last_heartbeat_at=$3
                WHERE run_item_id=$1
                    AND id=$2
                    AND state IN ('Pending', 'Running')
                RETURNING id";

            sqlx::query_scalar::<_, MachineValidationAttemptId>(QUERY)
                .bind(run_item_id)
                .bind(attempt_id)
                .bind(observed_at)
                .fetch_optional(&mut *txn)
                .await
                .map_err(|e| DatabaseError::query(QUERY, e))?
        }
        None => {
            const QUERY: &str = "
                WITH selected_attempt AS (
                    SELECT id
                    FROM machine_validation_attempts
                    WHERE run_item_id=$1
                    ORDER BY attempt_number DESC
                    LIMIT 1
                )
                UPDATE machine_validation_attempts
                SET
                    state='Running',
                    started_at=COALESCE(started_at, $2),
                    last_heartbeat_at=$2
                WHERE id=(SELECT id FROM selected_attempt)
                    AND state IN ('Pending', 'Running')
                RETURNING id";

            sqlx::query_scalar::<_, MachineValidationAttemptId>(QUERY)
                .bind(run_item_id)
                .bind(observed_at)
                .fetch_optional(&mut *txn)
                .await
                .map_err(|e| DatabaseError::query(QUERY, e))?
        }
    };

    Ok(updated.is_some())
}

async fn upsert_run_item_from_test(
    txn: &mut PgConnection,
    run_id: &MachineValidationId,
    context: &str,
    test: &MachineValidationTest,
    order_index: i32,
) -> DatabaseResult<MachineValidationRunItemId> {
    const QUERY: &str = "
        WITH upserted AS (
            INSERT INTO machine_validation_run_items (
                id,
                run_id,
                test_id,
                test_version,
                display_name,
                context,
                component,
                state,
                order_index,
                attempt,
                max_attempts,
                timeout_seconds
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 0, 1, $10)
            ON CONFLICT (run_id, test_id) DO UPDATE
            SET
                test_version=EXCLUDED.test_version,
                display_name=EXCLUDED.display_name,
                context=EXCLUDED.context,
                component=EXCLUDED.component,
                order_index=EXCLUDED.order_index,
                max_attempts=EXCLUDED.max_attempts,
                timeout_seconds=EXCLUDED.timeout_seconds
            WHERE machine_validation_run_items.state IN ('Pending', 'Running')
            RETURNING id
        )
        SELECT id FROM upserted
        UNION ALL
        SELECT id
        FROM machine_validation_run_items
        WHERE run_id=$2 AND test_id=$3
        LIMIT 1";

    let id = MachineValidationRunItemId::new();
    sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
        .bind(id)
        .bind(run_id)
        .bind(&test.test_id)
        .bind(test.version.version_string())
        .bind(&test.name)
        .bind(context)
        .bind(test.components.first())
        .bind(MachineValidationRunItemState::Pending.to_string())
        .bind(order_index)
        .bind(test.timeout.unwrap_or(DEFAULT_TIMEOUT_SECONDS))
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

async fn upsert_pending_attempt(
    txn: &mut PgConnection,
    run_item_id: &MachineValidationRunItemId,
    test: &MachineValidationTest,
) -> DatabaseResult<()> {
    const QUERY: &str = "
        INSERT INTO machine_validation_attempts (
            id,
            run_item_id,
            attempt_number,
            state,
            command,
            args,
            container_image,
            execute_in_host
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (run_item_id, attempt_number) DO UPDATE
        SET
            command=EXCLUDED.command,
            args=EXCLUDED.args,
            container_image=EXCLUDED.container_image,
            execute_in_host=EXCLUDED.execute_in_host
        WHERE machine_validation_attempts.state IN ('Pending', 'Running')";

    sqlx::query(QUERY)
        .bind(MachineValidationAttemptId::new())
        .bind(run_item_id)
        .bind(INITIAL_ATTEMPT_NUMBER)
        .bind(MachineValidationAttemptState::Pending.to_string())
        .bind(&test.command)
        .bind(&test.args)
        .bind(test.img_name.as_ref())
        .bind(test.execute_in_host)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;
    Ok(())
}

async fn upsert_run_item_from_result(
    txn: &mut PgConnection,
    result: &MachineValidationResult,
) -> DatabaseResult<MachineValidationRunItemId> {
    const QUERY: &str = "
        WITH upserted AS (
            INSERT INTO machine_validation_run_items (
                id,
                run_id,
                test_id,
                display_name,
                context,
                state,
                order_index,
                attempt,
                max_attempts,
                timeout_seconds
            )
            VALUES (
                $1,
                $2,
                $3,
                $4,
                $5,
                $6,
                COALESCE((SELECT MAX(order_index) + 1 FROM machine_validation_run_items WHERE run_id=$2), 0),
                0,
                1,
                $7
            )
            ON CONFLICT (run_id, test_id) DO UPDATE
            SET
                display_name=EXCLUDED.display_name,
                context=EXCLUDED.context
            WHERE machine_validation_run_items.state IN ('Pending', 'Running')
            RETURNING id
        )
        SELECT id FROM upserted
        UNION ALL
        SELECT id
        FROM machine_validation_run_items
        WHERE run_id=$2 AND test_id=$3
        LIMIT 1";

    sqlx::query_scalar::<_, MachineValidationRunItemId>(QUERY)
        .bind(MachineValidationRunItemId::new())
        .bind(result.validation_id)
        .bind(result_test_id(result))
        .bind(&result.name)
        .bind(&result.context)
        .bind(MachineValidationRunItemState::Pending.to_string())
        .bind(DEFAULT_TIMEOUT_SECONDS)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))
}

async fn update_pending_attempt_from_result(
    txn: &mut PgConnection,
    run_item_id: &MachineValidationRunItemId,
    result: &MachineValidationResult,
    state: &MachineValidationAttemptState,
    stdout_summary: Option<&str>,
    stderr_summary: Option<&str>,
    failure_classification: Option<&str>,
) -> DatabaseResult<bool> {
    const QUERY: &str = "
        UPDATE machine_validation_attempts
        SET
            state=$3,
            command=$4,
            args=$5,
            exit_code=$6,
            failure_classification=$7,
            started_at=$8,
            ended_at=$9,
            last_heartbeat_at=$9,
            stdout_summary=$10,
            stderr_summary=$11
        WHERE run_item_id=$1
        AND attempt_number=$2
        AND state IN ('Pending', 'Running')
        RETURNING id";

    let updated = sqlx::query_scalar::<_, MachineValidationAttemptId>(QUERY)
        .bind(run_item_id)
        .bind(INITIAL_ATTEMPT_NUMBER)
        .bind(state.to_string())
        .bind(&result.command)
        .bind(&result.args)
        .bind(result.exit_code)
        .bind(failure_classification)
        .bind(result.start_time)
        .bind(result.end_time)
        .bind(stdout_summary)
        .bind(stderr_summary)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;
    Ok(updated.is_some())
}

async fn insert_terminal_attempt_from_result(
    txn: &mut PgConnection,
    run_item_id: &MachineValidationRunItemId,
    result: &MachineValidationResult,
    state: &MachineValidationAttemptState,
    stdout_summary: Option<&str>,
    stderr_summary: Option<&str>,
    failure_classification: Option<&str>,
) -> DatabaseResult<bool> {
    const QUERY: &str = "
        INSERT INTO machine_validation_attempts (
            id,
            run_item_id,
            attempt_number,
            state,
            command,
            args,
            exit_code,
            failure_classification,
            started_at,
            ended_at,
            last_heartbeat_at,
            stdout_summary,
            stderr_summary
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $10, $11, $12)
        ON CONFLICT (run_item_id, attempt_number) DO NOTHING
        RETURNING id";

    let inserted = sqlx::query_scalar::<_, MachineValidationAttemptId>(QUERY)
        .bind(MachineValidationAttemptId::new())
        .bind(run_item_id)
        .bind(INITIAL_ATTEMPT_NUMBER)
        .bind(state.to_string())
        .bind(&result.command)
        .bind(&result.args)
        .bind(result.exit_code)
        .bind(failure_classification)
        .bind(result.start_time)
        .bind(result.end_time)
        .bind(stdout_summary)
        .bind(stderr_summary)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;
    Ok(inserted.is_some())
}

async fn update_run_item_from_result(
    txn: &mut PgConnection,
    run_item_id: &MachineValidationRunItemId,
    result: &MachineValidationResult,
    state: &MachineValidationAttemptState,
    stdout_summary: Option<&str>,
    stderr_summary: Option<&str>,
) -> DatabaseResult<()> {
    const QUERY: &str = "
        UPDATE machine_validation_run_items
        SET
            state=$2,
            attempt=$3,
            started_at=$4,
            ended_at=$5,
            last_heartbeat_at=$5,
            skip_reason=$6,
            failure_reason=$7
        WHERE id=$1";

    let skip_reason = (*state == MachineValidationAttemptState::Skipped)
        .then(|| stdout_summary.or(stderr_summary).unwrap_or_default());
    let failure_reason = (*state == MachineValidationAttemptState::Failed)
        .then(|| stderr_summary.or(stdout_summary).unwrap_or_default());

    sqlx::query(QUERY)
        .bind(run_item_id)
        .bind(run_item_state(state).to_string())
        .bind(INITIAL_ATTEMPT_NUMBER)
        .bind(result.start_time)
        .bind(result.end_time)
        .bind(skip_reason)
        .bind(failure_reason)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(QUERY, e))?;
    Ok(())
}

fn result_test_id(result: &MachineValidationResult) -> String {
    result
        .test_id
        .clone()
        .unwrap_or_else(|| machine_validation_suites::generate_test_id(&result.name))
}

fn state_from_result(result: &MachineValidationResult) -> MachineValidationAttemptState {
    if result.exit_code == 0 && result.stdout.trim_start().starts_with("Skipped") {
        MachineValidationAttemptState::Skipped
    } else if result.exit_code == 0 {
        MachineValidationAttemptState::Success
    } else {
        MachineValidationAttemptState::Failed
    }
}

fn run_item_state(state: &MachineValidationAttemptState) -> MachineValidationRunItemState {
    match state {
        MachineValidationAttemptState::Pending => MachineValidationRunItemState::Pending,
        MachineValidationAttemptState::Running => MachineValidationRunItemState::Running,
        MachineValidationAttemptState::Success => MachineValidationRunItemState::Success,
        MachineValidationAttemptState::Skipped => MachineValidationRunItemState::Skipped,
        MachineValidationAttemptState::Failed => MachineValidationRunItemState::Failed,
    }
}

fn truncate_summary(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.chars().take(SUMMARY_LIMIT).collect())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;

    use super::*;

    fn test_machine_id() -> MachineId {
        MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30").unwrap()
    }

    async fn insert_active_validation(
        txn: &mut PgConnection,
        now: chrono::DateTime<chrono::Utc>,
    ) -> DatabaseResult<MachineValidationId> {
        let id = MachineValidationId::new();
        const QUERY: &str = "
            INSERT INTO machine_validation (
                id,
                machine_id,
                start_time,
                name,
                end_time,
                context,
                total,
                completed,
                state,
                duration_to_complete,
                last_heartbeat_at
            )
            VALUES ($1, $2, $3, $4, NULL, $5, 1, 0, $6, 0, NULL)";

        sqlx::query(QUERY)
            .bind(id)
            .bind(test_machine_id())
            .bind(now)
            .bind(format!("Test_{id}"))
            .bind("OnDemand")
            .bind("InProgress")
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::query(QUERY, e))?;

        Ok(id)
    }

    async fn insert_attempt(
        txn: &mut PgConnection,
        validation_id: &MachineValidationId,
        test_id: &str,
        order_index: i32,
        state: MachineValidationAttemptState,
        started_at: Option<chrono::DateTime<chrono::Utc>>,
        last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> DatabaseResult<MachineValidationAttemptId> {
        let run_item_id = MachineValidationRunItemId::new();
        let attempt_id = MachineValidationAttemptId::new();
        const RUN_ITEM_QUERY: &str = "
            INSERT INTO machine_validation_run_items (
                id,
                run_id,
                test_id,
                display_name,
                context,
                state,
                order_index,
                attempt,
                max_attempts,
                timeout_seconds,
                started_at,
                last_heartbeat_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, 1, 1, 1, $8, $9)";

        sqlx::query(RUN_ITEM_QUERY)
            .bind(run_item_id)
            .bind(validation_id)
            .bind(test_id)
            .bind(test_id)
            .bind("OnDemand")
            .bind(MachineValidationRunItemState::Running.to_string())
            .bind(order_index)
            .bind(started_at)
            .bind(last_heartbeat_at)
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(RUN_ITEM_QUERY, e))?;

        const ATTEMPT_QUERY: &str = "
            INSERT INTO machine_validation_attempts (
                id,
                run_item_id,
                attempt_number,
                state,
                command,
                args,
                started_at,
                last_heartbeat_at
            )
            VALUES ($1, $2, 1, $3, $4, $5, $6, $7)";

        sqlx::query(ATTEMPT_QUERY)
            .bind(attempt_id)
            .bind(run_item_id)
            .bind(state.to_string())
            .bind("echo")
            .bind("ok")
            .bind(started_at)
            .bind(last_heartbeat_at)
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::query(ATTEMPT_QUERY, e))?;

        Ok(attempt_id)
    }

    #[crate::sqlx_test]
    async fn find_stale_active_attempts_respects_heartbeat_and_duration_fallback(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let now = chrono::Utc::now();
        let validation_id = insert_active_validation(txn.as_mut(), now).await?;

        insert_attempt(
            txn.as_mut(),
            &validation_id,
            "stale-heartbeat",
            0,
            MachineValidationAttemptState::Running,
            Some(now - chrono::Duration::seconds(10)),
            Some(now - chrono::Duration::seconds(61)),
        )
        .await?;
        let _fresh_heartbeat_attempt = insert_attempt(
            txn.as_mut(),
            &validation_id,
            "fresh-heartbeat",
            1,
            MachineValidationAttemptState::Running,
            Some(now - chrono::Duration::seconds(10)),
            Some(now - chrono::Duration::seconds(30)),
        )
        .await?;
        insert_attempt(
            txn.as_mut(),
            &validation_id,
            "legacy-stale",
            2,
            MachineValidationAttemptState::Running,
            Some(now - chrono::Duration::seconds(120)),
            None,
        )
        .await?;
        let _terminal_attempt = insert_attempt(
            txn.as_mut(),
            &validation_id,
            "terminal",
            3,
            MachineValidationAttemptState::Failed,
            Some(now - chrono::Duration::seconds(120)),
            Some(now - chrono::Duration::seconds(120)),
        )
        .await?;

        let stale_attempts =
            find_stale_active_attempts(txn.as_mut(), std::time::Duration::from_secs(60), now)
                .await?;
        let stale_test_ids = stale_attempts
            .iter()
            .map(|attempt| attempt.test_id.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(
            stale_test_ids,
            BTreeSet::from(["legacy-stale", "stale-heartbeat"])
        );

        Ok(())
    }
}
