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

use health_report::{HealthReport, HealthReportApplyMode};
use sqlx::PgConnection;

use crate::DatabaseError;

/// Insert a health report into the `health_reports` JSONB column of the
/// given table.
///
/// The `id` parameter is bound as `$2` and must match the `id`
/// column of `table_name`.
pub async fn insert_health_report<Id>(
    txn: &mut PgConnection,
    table_name: &str,
    id: &Id,
    mode: HealthReportApplyMode,
    health_report: &HealthReport,
) -> Result<(), DatabaseError>
where
    for<'e> Id: sqlx::Encode<'e, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Sync,
{
    // NOTE: SQL injection risk is known here: this helper intentionally preserves the existing
    // health-report SQL shape where table_name is an internal constant at call sites, but the JSONB
    // path still includes the report source in the SQL text. We'll want to replace this with a
    // bound text[] path or central source validation before accepting broader inputs.
    let path = match mode {
        HealthReportApplyMode::Merge => vec!["merges".to_string(), health_report.source.clone()],
        HealthReportApplyMode::Replace => vec!["replace".to_string()],
    };

    let mut query = sqlx::QueryBuilder::new("UPDATE ");
    query.push(table_name);
    query.push(
        " SET health_reports = jsonb_set(
            coalesce(health_reports, '{\"merges\": {}}'::jsonb),
            ",
    );
    query.push_bind(path);
    query.push(", ");
    query.push_bind(sqlx::types::Json(health_report));
    query.push(") WHERE id = ");
    query.push_bind(id);
    query.push(" RETURNING id");

    query
        .build()
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(format!("insert {table_name} health report"), e))?;

    Ok(())
}

/// Remove a health report from the `health_reports` JSONB column of the
/// given table.
pub async fn remove_health_report<Id>(
    txn: &mut PgConnection,
    table_name: &str,
    id: &Id,
    mode: HealthReportApplyMode,
    source: &str,
) -> Result<(), DatabaseError>
where
    for<'e> Id: sqlx::Encode<'e, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Sync,
{
    let path = match mode {
        HealthReportApplyMode::Merge => vec!["merges".to_string(), source.to_string()],
        HealthReportApplyMode::Replace => vec!["replace".to_string()],
    };

    let mut query = sqlx::QueryBuilder::new("UPDATE ");
    query.push(table_name);
    query.push(" SET health_reports = (health_reports #- ");
    query.push_bind(path);
    query.push(") WHERE id = ");
    query.push_bind(id);
    query.push(" RETURNING id");

    query
        .build()
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::new(format!("remove {table_name} health report"), e))?;

    Ok(())
}
