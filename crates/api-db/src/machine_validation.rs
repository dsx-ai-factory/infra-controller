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
use carbide_uuid::machine_validation::MachineValidationId;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{MachineValidationContext, MachineValidationFilter};
use model::machine_validation::{
    MachineValidation, MachineValidationState, MachineValidationStatus,
};
use sqlx::PgConnection;

use super::{ColumnInfo, FilterableQueryBuilder, ObjectColumnFilter};
use crate::db_read::DbReader;
use crate::machine::MachineRowLockItem;
use crate::{DatabaseError, DatabaseResult};

impl MachineRowLockItem for MachineValidation {
    fn machine_id(&self) -> MachineId {
        self.machine_id
    }
}

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = MachineValidation;
    type ColumnType = MachineValidationId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

#[derive(Clone, Copy)]
pub struct MachineIdColumn;
impl<'a> ColumnInfo<'a> for MachineIdColumn {
    type TableType = MachineValidation;
    type ColumnType = MachineId;

    fn column_name(&self) -> &'static str {
        "machine_id"
    }
}

#[derive(Clone, Copy)]
pub struct StateColumn;
impl<'a> ColumnInfo<'a> for StateColumn {
    type TableType = MachineValidation;
    type ColumnType = String;

    fn column_name(&self) -> &'static str {
        "state"
    }
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = MachineValidation>>(
    txn: impl DbReader<'_>,
    filter: ObjectColumnFilter<'a, C>,
) -> Result<Vec<MachineValidation>, DatabaseError> {
    let mut query = FilterableQueryBuilder::new("SELECT * FROM machine_validation").filter(&filter);
    query.push(" ORDER BY start_time");

    let custom_results = query
        .build_query_as()
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("machine_validation find_by", e))?;

    Ok(custom_results)
}

/// Mark a run in progress only while it is still active.
///
/// Scout can poll after another request has completed the run. Keeping the
/// active predicate in this update makes that race harmless: a terminal run
/// remains terminal and is not dispatched again.
pub async fn mark_in_progress_if_active(
    txn: &mut PgConnection,
    id: &MachineValidationId,
) -> DatabaseResult<Option<MachineValidation>> {
    let query = "
        UPDATE machine_validation
        SET state=$2
        WHERE id=$1
        AND end_time IS NULL
        AND state IN ('Started', 'InProgress')
        RETURNING *";
    sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .bind(MachineValidationState::InProgress.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}
pub async fn update_end_time(
    txn: &mut PgConnection,
    id: &MachineValidationId,
    status: &MachineValidationStatus,
) -> DatabaseResult<()> {
    let query = "UPDATE machine_validation SET end_time=NOW(),state=$2 WHERE id=$1 RETURNING *";
    let _id = sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .bind(status.state.to_string())
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

pub fn is_active(validation: &MachineValidation) -> bool {
    validation.end_time.is_none()
        && validation.status.as_ref().is_some_and(|status| {
            matches!(
                status.state,
                MachineValidationState::Started | MachineValidationState::InProgress
            )
        })
}

pub async fn update_end_time_if_active(
    txn: &mut PgConnection,
    id: &MachineValidationId,
    status: &MachineValidationStatus,
) -> DatabaseResult<Option<MachineValidation>> {
    let query = "
        UPDATE machine_validation
        SET end_time=NOW(),state=$2
        WHERE id=$1
        AND end_time IS NULL
        AND state IN ('Started', 'InProgress')
        RETURNING *";
    sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .bind(status.state.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn mark_stale_if_active(
    txn: &mut PgConnection,
    id: &MachineValidationId,
    stale_run_timeout: std::time::Duration,
    now: chrono::DateTime<chrono::Utc>,
    status: &MachineValidationStatus,
) -> DatabaseResult<Option<MachineValidation>> {
    let stale_run_timeout_seconds = i64::try_from(stale_run_timeout.as_secs()).unwrap_or(i64::MAX);
    let query = "
        UPDATE machine_validation
        SET end_time=NOW(),state=$2
        WHERE id=$1
        AND end_time IS NULL
        AND state IN ('Started', 'InProgress')
        AND (
            (
                last_heartbeat_at IS NOT NULL
                AND last_heartbeat_at + ($3::bigint * INTERVAL '1 second') < $4
            )
            OR (
                last_heartbeat_at IS NULL
                AND start_time
                    + (GREATEST(duration_to_complete, 0) * INTERVAL '1 second')
                    + ($3::bigint * INTERVAL '1 second') < $4
            )
        )
        RETURNING *";
    sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .bind(status.state.to_string())
        .bind(stale_run_timeout_seconds)
        .bind(now)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn update_run(
    txn: &mut PgConnection,
    id: &MachineValidationId,
    total: i32,
    duration_to_complete: i64,
) -> DatabaseResult<()> {
    let query = "UPDATE machine_validation SET duration_to_complete=$2,total=$3,completed=0,state=$4 WHERE id=$1 AND end_time IS NULL AND state IN ('Started', 'InProgress') RETURNING *";
    let updated = sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .bind(duration_to_complete)
        .bind(total)
        .bind(MachineValidationState::InProgress.to_string())
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    if updated.is_none() {
        return Err(DatabaseError::InvalidArgument(format!(
            "Machine validation run {id} is not active"
        )));
    }
    Ok(())
}

pub async fn create_new_run(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    context: MachineValidationContext,
    filter: MachineValidationFilter,
) -> Result<MachineValidation, DatabaseError> {
    let id = MachineValidationId::from(uuid::Uuid::new_v4());
    let query = "
        INSERT INTO machine_validation (
            id,
            name,
            machine_id,
            filter,
            context,
            end_time,
            description,
            state
        )
        VALUES ($1, $2, $3, $4, $5, NULL, $6, $7)
        ON CONFLICT DO NOTHING
        RETURNING *";
    // TODO fetch total number of test and repopulate the status
    let status = MachineValidationStatus {
        state: MachineValidationState::Started,
        ..MachineValidationStatus::default()
    };
    let validation = sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .bind(format!("Test_{machine_id}"))
        .bind(machine_id)
        .bind(sqlx::types::Json(filter))
        .bind(context.as_ref())
        .bind(format!("Running validation on {machine_id}"))
        .bind(status.state.to_string())
        .fetch_one(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    crate::machine::update_machine_validation_id(machine_id, id, context, txn).await?;

    // Reset machine validation health report into initial state
    let health_report = health_report::HealthReport::empty(
        health_report::HealthReport::MACHINE_VALIDATION_SOURCE.to_string(),
    );
    crate::machine::update_machine_validation_health_report(txn, machine_id, &health_report)
        .await?;

    Ok(validation)
}

pub async fn find<DB>(
    txn: &mut DB,
    machine_id: &MachineId,
    include_history: bool,
) -> DatabaseResult<Vec<MachineValidation>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    if include_history {
        return find_by_machine_id(&mut *txn, machine_id).await;
    };
    let machine =
        match crate::machine::find_one(&mut *txn, machine_id, MachineSearchConfig::default()).await
        {
            Err(err) => {
                tracing::warn!(%machine_id, error = %err, "failed loading machine");
                return Err(DatabaseError::InvalidArgument(
                    "err loading machine".to_string(),
                ));
            }
            Ok(None) => {
                tracing::info!(%machine_id, "machine not found");
                return Err(DatabaseError::NotFoundError {
                    kind: "machine",
                    id: machine_id.to_string(),
                });
            }
            Ok(Some(m)) => m,
        };
    let discovery_machine_validation_id =
        machine.discovery_machine_validation_id.unwrap_or_default();
    let cleanup_machine_validation_id = machine.cleanup_machine_validation_id.unwrap_or_default();

    let on_demand_machine_validation_id =
        machine.on_demand_machine_validation_id.unwrap_or_default();
    find_by(
        &mut *txn,
        ObjectColumnFilter::List(
            IdColumn,
            &[
                cleanup_machine_validation_id,
                discovery_machine_validation_id,
                on_demand_machine_validation_id,
            ],
        ),
    )
    .await
}

pub async fn find_by_machine_id(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> DatabaseResult<Vec<MachineValidation>> {
    find_by(
        txn,
        ObjectColumnFilter::List(MachineIdColumn, std::slice::from_ref(machine_id)),
    )
    .await
}

pub async fn find_active(txn: impl DbReader<'_>) -> DatabaseResult<Vec<MachineValidation>> {
    let query = "
        SELECT * FROM machine_validation
        WHERE end_time IS NULL
        AND state IN ('Started', 'InProgress')
        ORDER BY start_time";

    sqlx::query_as::<_, MachineValidation>(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_active_machine_validation_by_machine_id(
    txn: impl DbReader<'_>,
    machine_id: &MachineId,
) -> DatabaseResult<MachineValidation> {
    let ret = find_by_machine_id(txn, machine_id).await?;
    for iter in ret {
        if is_active(&iter) {
            return Ok(iter);
        }
    }
    Err(DatabaseError::InvalidArgument(format!(
        "Not active machine validation in  {machine_id:?} "
    )))
}

pub async fn find_by_id(
    txn: impl DbReader<'_>,
    id: &MachineValidationId,
) -> DatabaseResult<MachineValidation> {
    let machine_validation = find_by(txn, ObjectColumnFilter::One(IdColumn, id)).await?;

    if !machine_validation.is_empty() {
        return Ok(machine_validation[0].clone());
    }
    Err(DatabaseError::InvalidArgument(format!(
        "Validaion Id not found  {id:?} "
    )))
}

pub async fn lock_by_id_no_key_update(
    txn: &mut PgConnection,
    id: &MachineValidationId,
) -> DatabaseResult<Option<MachineValidation>> {
    let query = "SELECT * FROM machine_validation WHERE id=$1 FOR NO KEY UPDATE";
    sqlx::query_as::<_, MachineValidation>(query)
        .bind(id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

pub async fn find_all(txn: impl DbReader<'_>) -> DatabaseResult<Vec<MachineValidation>> {
    find_by(txn, ObjectColumnFilter::<IdColumn>::All).await
}

pub async fn mark_machine_validation_complete(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    id: &MachineValidationId,
    status: MachineValidationStatus,
) -> DatabaseResult<bool> {
    let Some(_updated) = update_end_time_if_active(txn, id, &status).await? else {
        return Ok(false);
    };

    //Mark machine validation request to false
    crate::machine::set_machine_validation_request(txn, machine_id, false).await?;

    crate::machine::update_machine_validation_time(machine_id, txn).await?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn test_machine_id() -> MachineId {
        MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30").unwrap()
    }

    async fn insert_active_validation(
        txn: &mut PgConnection,
        start_time: chrono::DateTime<chrono::Utc>,
        duration_to_complete: i64,
        last_heartbeat_at: Option<chrono::DateTime<chrono::Utc>>,
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
            VALUES ($1, $2, $3, $4, NULL, $5, 1, 0, $6, $7, $8)";

        sqlx::query(QUERY)
            .bind(id)
            .bind(test_machine_id())
            .bind(start_time)
            .bind(format!("Test_{id}"))
            .bind("OnDemand")
            .bind(MachineValidationState::InProgress.to_string())
            .bind(duration_to_complete)
            .bind(last_heartbeat_at)
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::query(QUERY, e))?;

        Ok(id)
    }

    #[crate::sqlx_test]
    async fn mark_stale_if_active_uses_heartbeat_when_present(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let now = chrono::Utc::now();
        let stale_run_timeout = std::time::Duration::from_secs(60);
        let status = MachineValidationStatus {
            state: MachineValidationState::Failed,
            ..MachineValidationStatus::default()
        };

        let fresh_heartbeat = insert_active_validation(
            txn.as_mut(),
            now - chrono::Duration::minutes(10),
            1,
            Some(now - chrono::Duration::seconds(30)),
        )
        .await?;
        let stale_heartbeat = insert_active_validation(
            txn.as_mut(),
            now - chrono::Duration::seconds(30),
            1,
            Some(now - chrono::Duration::seconds(61)),
        )
        .await?;
        let stale_without_heartbeat =
            insert_active_validation(txn.as_mut(), now - chrono::Duration::seconds(120), 1, None)
                .await?;

        assert!(
            mark_stale_if_active(
                txn.as_mut(),
                &fresh_heartbeat,
                stale_run_timeout,
                now,
                &status,
            )
            .await?
            .is_none()
        );
        assert_eq!(
            mark_stale_if_active(
                txn.as_mut(),
                &stale_heartbeat,
                stale_run_timeout,
                now,
                &status,
            )
            .await?
            .map(|validation| validation.id),
            Some(stale_heartbeat)
        );
        assert_eq!(
            mark_stale_if_active(
                txn.as_mut(),
                &stale_without_heartbeat,
                stale_run_timeout,
                now,
                &status,
            )
            .await?
            .map(|validation| validation.id),
            Some(stale_without_heartbeat)
        );

        Ok(())
    }
}
