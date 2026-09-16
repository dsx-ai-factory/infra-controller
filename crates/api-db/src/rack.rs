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

use carbide_uuid::rack::{RackId, RackProfileId};
use config_version::ConfigVersion;
use health_report::{HealthReport, HealthReportApplyMode};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::metadata::Metadata;
use model::rack::{FirmwareUpgradeJob, NvosUpdateJob, Rack, RackConfig, RackState};
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{
    ColumnInfo, ConditionalWrite, ControllerStateNotCurrent, DatabaseError, DatabaseResult,
    FilterableQueryBuilder, ObjectColumnFilter,
};

#[cfg(test)]
mod test_metadata;

#[derive(Copy, Clone)]
pub struct IdColumn;
impl ColumnInfo<'_> for IdColumn {
    type TableType = Rack;
    type ColumnType = RackId;

    fn column_name(&self) -> &'static str {
        "id"
    }
}

pub async fn find_by<'a, C: ColumnInfo<'a, TableType = Rack>, DB>(
    conn: &mut DB,
    filter: ObjectColumnFilter<'a, C>,
) -> DatabaseResult<Vec<Rack>>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    let mut query = FilterableQueryBuilder::new("SELECT * FROM racks").filter(&filter);

    query
        .build_query_as()
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| DatabaseError::new(query.sql(), e))
}

/// Lock one active rack until the caller's transaction completes.
pub async fn lock_for_update(txn: &mut PgConnection, rack_id: &RackId) -> DatabaseResult<bool> {
    let query = "SELECT id FROM racks WHERE id = $1 AND deleted IS NULL FOR UPDATE";
    sqlx::query_as::<_, (RackId,)>(query)
        .bind(rack_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))
        .map(|row| row.is_some())
}

pub async fn find_ids(
    txn: impl DbReader<'_>,
    filter: model::rack::RackSearchFilter,
) -> Result<Vec<RackId>, DatabaseError> {
    let mut builder = sqlx::QueryBuilder::new("SELECT id FROM racks WHERE TRUE "); // The TRUE will be optimized away.

    if let Some(label) = filter.label {
        match (label.key.is_empty(), label.value) {
            // Label key is empty, label value is set.
            (true, Some(value)) => {
                builder.push(
                    " AND EXISTS (
                        SELECT 1
                        FROM jsonb_each_text(labels) AS kv
                        WHERE kv.value = ",
                );
                builder.push_bind(value);
                builder.push(")");
            }
            // Label key is empty, label value is not set.
            (true, None) => {
                return Err(DatabaseError::InvalidArgument(
                    "finding racks based on label needs either key or a value.".to_string(),
                ));
            }
            // Label key is not empty, label value is not set.
            (false, None) => {
                builder.push(" AND labels ->> ");
                builder.push_bind(label.key);
                builder.push(" IS NOT NULL");
            }
            // Label key is not empty, label value is set.
            (false, Some(value)) => {
                builder.push(" AND labels ->> ");
                builder.push_bind(label.key);
                builder.push(" = ");
                builder.push_bind(value);
            }
        }
    }

    let query = builder.build_query_as();
    query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new("rack::find_ids", e))
}

pub async fn create(
    txn: &mut PgConnection,
    rack_id: &RackId,
    rack_profile_id: Option<&RackProfileId>,
    config: &RackConfig,
    expected_metadata: Option<&Metadata>,
) -> DatabaseResult<Rack> {
    let controller_state = String::from("{\"state\":\"created\"}");
    let controller_state_outcome = String::from("{}");
    let default_metadata = Metadata::default();
    let src_metadata = expected_metadata.unwrap_or(&default_metadata);
    let name = match src_metadata.name.as_str() {
        "" => rack_id.to_string(),
        name => name.to_string(),
    };
    let version = ConfigVersion::initial();
    let query = "INSERT INTO racks(id, rack_profile_id, config, controller_state, controller_state_version, controller_state_outcome, name, description, labels, version)
            VALUES($1, $2, $3::json, $4::json, $5, $6::json, $7, $8, $9::jsonb, $10) RETURNING *";
    let rack: Rack = sqlx::query_as(query)
        .bind(rack_id)
        .bind(rack_profile_id)
        .bind(sqlx::types::Json(config))
        .bind(controller_state)
        .bind(version)
        .bind(controller_state_outcome)
        .bind(name)
        .bind(&src_metadata.description)
        .bind(sqlx::types::Json(&src_metadata.labels))
        .bind(version)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(rack)
}

// only update the config
pub async fn update(
    txn: &mut PgConnection,
    rack_id: &RackId,
    config: &RackConfig,
) -> DatabaseResult<Rack> {
    // `maintenance_termination_requested` is a latch owned by the rack
    // controller.
    // Once set during Maintenance, preserve the complete persisted config so
    // an older controller snapshot cannot clear either the latch or the scope
    // that the termination handler needs. A latch outside Maintenance violates
    // the API invariant, so clear it while applying the incoming config rather
    // than letting it poison the next maintenance request. A new termination
    // can still be latched when the persisted value is false.
    let query = r#"UPDATE racks
        SET config = CASE
            WHEN COALESCE((config->>'maintenance_termination_requested')::boolean, false)
                AND controller_state->>'state' = 'maintenance'
                THEN config
            WHEN COALESCE((config->>'maintenance_termination_requested')::boolean, false)
                THEN jsonb_set($1::jsonb, '{maintenance_termination_requested}', 'false'::jsonb)
            ELSE $1::jsonb
        END,
        updated = NOW()
        WHERE id = $2
        RETURNING *"#;
    let rack: Rack = sqlx::query_as(query)
        .bind(sqlx::types::Json(config))
        .bind(rack_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(rack)
}

/// Atomically consumes a pending maintenance-termination request and its scope.
///
/// Unlike [`update`], this is reserved for the termination handler after it has
/// completed the scoped cleanup. It updates only the two maintenance request
/// fields so a stale controller snapshot cannot overwrite unrelated config.
pub async fn consume_maintenance_termination_request(
    txn: &mut PgConnection,
    rack_id: &RackId,
) -> DatabaseResult<Rack> {
    let query = r#"UPDATE racks
        SET config = jsonb_set(
            jsonb_set(config, '{maintenance_requested}', 'null'::jsonb),
            '{maintenance_termination_requested}', 'false'::jsonb
        ),
        updated = NOW()
        WHERE id = $1
          AND COALESCE((config->>'maintenance_termination_requested')::boolean, false)
          AND controller_state->>'state' = 'maintenance'
        RETURNING *"#;
    let rack: Rack = sqlx::query_as(query)
        .bind(rack_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(rack)
}

/// `try_update_controller_state` writes the rack state and `new_version`
/// when the version matches `expected_version` and maintenance is not terminating.
///
/// A missing rack, changed version, or `maintenance_termination_requested`
/// latch while the persisted state is `Maintenance` returns
/// `NotApplied(ControllerStateNotCurrent)`. A latch outside `Maintenance` does
/// not block the write. Successful writes return `Applied(())` and remain in the
/// caller's transaction; database failures remain errors.
pub async fn try_update_controller_state(
    txn: &mut PgConnection,
    rack_id: &RackId,
    expected_version: ConfigVersion,
    new_version: ConfigVersion,
    new_state: &RackState,
) -> DatabaseResult<ConditionalWrite<(), ControllerStateNotCurrent>> {
    // A termination request is accepted only while the persisted rack state is
    // Maintenance. Scope the latch guard to that state so an invalid or stale
    // latch cannot freeze transitions in every other rack state.
    let query_result = sqlx::query_as::<_, Rack>(
            "UPDATE racks SET controller_state = $1, controller_state_version = $2 WHERE id = $3 AND controller_state_version = $4 AND NOT (COALESCE((config->>'maintenance_termination_requested')::boolean, false) AND controller_state->>'state' = 'maintenance') RETURNING *",
        )
            .bind(sqlx::types::Json(new_state))
            .bind(new_version)
            .bind(rack_id)
            .bind(expected_version)
            .fetch_optional(txn)
            .await
            .map_err(|e| DatabaseError::new("try_update_controller_state", e))?;

    Ok(match query_result {
        Some(_) => ConditionalWrite::Applied(()),
        None => ConditionalWrite::NotApplied(ControllerStateNotCurrent),
    })
}

pub async fn update_controller_state_outcome(
    txn: &mut PgConnection,
    rack_id: &RackId,
    outcome: PersistentStateHandlerOutcome,
) -> DatabaseResult<()> {
    sqlx::query("UPDATE racks SET controller_state_outcome = $1 WHERE id = $2")
        .bind(sqlx::types::Json(outcome))
        .bind(rack_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("update_controller_state_outcome", e))?;

    Ok(())
}

pub async fn mark_as_deleted(rack_id: &RackId, txn: &mut PgConnection) -> DatabaseResult<Rack> {
    let query = "UPDATE racks SET updated=NOW(), deleted=NOW() WHERE id=$1 RETURNING *";
    let updated_rack = sqlx::query_as(query)
        .bind(rack_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(updated_rack)
}

pub async fn update_firmware_upgrade_job(
    txn: &mut PgConnection,
    rack_id: &RackId,
    job: Option<&FirmwareUpgradeJob>,
) -> DatabaseResult<()> {
    let query =
        "UPDATE racks SET firmware_upgrade_job = $1, updated = NOW() WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, (RackId,)>(query)
        .bind(job.map(sqlx::types::Json))
        .bind(rack_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update_firmware_upgrade_job", e))?;
    Ok(())
}

pub async fn update_nvos_update_job(
    txn: &mut PgConnection,
    rack_id: &RackId,
    job: Option<&NvosUpdateJob>,
) -> DatabaseResult<()> {
    let query = "UPDATE racks SET nvos_update_job = $1, updated = NOW() WHERE id = $2 RETURNING id";
    sqlx::query_as::<_, (RackId,)>(query)
        .bind(job.map(sqlx::types::Json))
        .bind(rack_id)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new("update_nvos_update_job", e))?;
    Ok(())
}

pub async fn final_delete(txn: &mut PgConnection, rack_id: &RackId) -> DatabaseResult<()> {
    let query = "DELETE from racks WHERE id=$1";
    sqlx::query(query)
        .bind(rack_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

pub async fn insert_health_report(
    txn: &mut PgConnection,
    rack_id: &RackId,
    mode: HealthReportApplyMode,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    crate::health_report::insert_health_report(txn, "racks", rack_id, mode, health_report).await
}

pub async fn remove_health_report(
    txn: &mut PgConnection,
    rack_id: &RackId,
    mode: HealthReportApplyMode,
    source: &str,
) -> Result<(), DatabaseError> {
    crate::health_report::remove_health_report(txn, "racks", rack_id, mode, source).await
}

pub async fn update_metadata(
    txn: &mut PgConnection,
    rack_id: &RackId,
    expected_version: ConfigVersion,
    metadata: Metadata,
) -> Result<(), DatabaseError> {
    let next_version = expected_version.increment();

    let query = "UPDATE racks SET
            version=$1,
            name=$2, description=$3, labels=$4::jsonb
            WHERE id=$5 AND version=$6
            RETURNING id";

    let query_result: Result<(RackId,), _> = sqlx::query_as(query)
        .bind(next_version)
        .bind(&metadata.name)
        .bind(&metadata.description)
        .bind(sqlx::types::Json(&metadata.labels))
        .bind(rack_id)
        .bind(expected_version)
        .fetch_one(txn)
        .await;

    match query_result {
        Ok((_id,)) => Ok(()),
        Err(e) => Err(match e {
            sqlx::Error::RowNotFound => {
                DatabaseError::ConcurrentModificationError("rack", expected_version.to_string())
            }
            e => DatabaseError::query(query, e),
        }),
    }
}
