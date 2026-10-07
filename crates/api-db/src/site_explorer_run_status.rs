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

use chrono::{DateTime, Utc};
use model::site_explorer::SiteExplorerLastRun;
use sqlx::{FromRow, PgConnection};

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

const LAST_RUN_ID: i16 = 1;

#[derive(Debug, FromRow)]
struct DbSiteExplorerLastRun {
    started_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    success: bool,
    error: Option<String>,
    failure_category: Option<String>,
    endpoint_explorations: i64,
    endpoint_explorations_success: i64,
    endpoint_explorations_failed: i64,
    last_successful_finished_at: Option<DateTime<Utc>>,
    last_failed_finished_at: Option<DateTime<Utc>>,
}

impl From<DbSiteExplorerLastRun> for SiteExplorerLastRun {
    fn from(run: DbSiteExplorerLastRun) -> Self {
        Self {
            started_at: run.started_at,
            finished_at: run.finished_at,
            success: run.success,
            error: run.error,
            failure_category: run.failure_category,
            endpoint_explorations: run.endpoint_explorations,
            endpoint_explorations_success: run.endpoint_explorations_success,
            endpoint_explorations_failed: run.endpoint_explorations_failed,
            last_successful_finished_at: run.last_successful_finished_at,
            last_failed_finished_at: run.last_failed_finished_at,
        }
    }
}

/// Fetches metadata for the latest site explorer run.
pub async fn fetch(db: impl DbReader<'_>) -> DatabaseResult<Option<SiteExplorerLastRun>> {
    let query = "SELECT started_at, finished_at, success, error, failure_category, endpoint_explorations, endpoint_explorations_success, endpoint_explorations_failed, last_successful_finished_at, last_failed_finished_at
    FROM site_explorer_run_status
    WHERE id = $1";

    sqlx::query_as::<_, DbSiteExplorerLastRun>(query)
        .bind(LAST_RUN_ID)
        .fetch_optional(db)
        .await
        .map(|run| run.map(Into::into))
        .map_err(|e| DatabaseError::query(query, e))
}

/// Replaces metadata for the latest site explorer run.
pub async fn upsert(txn: &mut PgConnection, last_run: &SiteExplorerLastRun) -> DatabaseResult<()> {
    let query = "INSERT INTO site_explorer_run_status (
    id,
    started_at,
    finished_at,
    success,
    error,
    failure_category,
    endpoint_explorations,
    endpoint_explorations_success,
    endpoint_explorations_failed,
    last_successful_finished_at,
    last_failed_finished_at
)
VALUES (
    $1, $2, $3, $4, $5, $6, $7, $8, $9,
    CASE WHEN $4 THEN $3 ELSE $10 END,
    CASE WHEN NOT $4 THEN $3 ELSE $11 END
)
ON CONFLICT (id) DO UPDATE SET
    started_at = EXCLUDED.started_at,
    finished_at = EXCLUDED.finished_at,
    success = EXCLUDED.success,
    error = EXCLUDED.error,
    failure_category = EXCLUDED.failure_category,
    endpoint_explorations = EXCLUDED.endpoint_explorations,
    endpoint_explorations_success = EXCLUDED.endpoint_explorations_success,
    endpoint_explorations_failed = EXCLUDED.endpoint_explorations_failed,
    last_successful_finished_at = CASE
        WHEN EXCLUDED.success THEN EXCLUDED.finished_at
        ELSE COALESCE(
            EXCLUDED.last_successful_finished_at,
            site_explorer_run_status.last_successful_finished_at
        )
    END,
    last_failed_finished_at = CASE
        WHEN NOT EXCLUDED.success THEN EXCLUDED.finished_at
        ELSE COALESCE(
            EXCLUDED.last_failed_finished_at,
            site_explorer_run_status.last_failed_finished_at
        )
    END";

    sqlx::query(query)
        .bind(LAST_RUN_ID)
        .bind(last_run.started_at)
        .bind(last_run.finished_at)
        .bind(last_run.success)
        .bind(&last_run.error)
        .bind(&last_run.failure_category)
        .bind(last_run.endpoint_explorations)
        .bind(last_run.endpoint_explorations_success)
        .bind(last_run.endpoint_explorations_failed)
        .bind(last_run.last_successful_finished_at)
        .bind(last_run.last_failed_finished_at)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}
