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
use sqlx::PgConnection;

use crate::DatabaseError;

pub type IsFirstObservation = bool;

/// Mark the given version as the latest forge/carbide version.
///
/// Any other version (which is not already superseded) will be marked as superseded at the current
/// date/time. This should lead to exactly one non-superseded version at any time.
pub async fn observe_as_latest_version(
    txn: &mut PgConnection,
    version: &str,
) -> Result<IsFirstObservation, DatabaseError> {
    // Is this version already present?
    let id: Option<uuid::Uuid> = {
        let query = "SELECT id FROM forge_versions WHERE version = $1 LIMIT 1";
        sqlx::query_scalar(query)
            .bind(version)
            .fetch_optional(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
    };

    if id.is_some() {
        return Ok(false);
    }

    // No? Ok, then first mark all other versions as superseded, before inserting a new one.
    let superseded_count = {
        let query = "UPDATE forge_versions SET superseded = now() WHERE superseded IS NULL";
        sqlx::query(query)
            .execute(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?
            .rows_affected()
    };

    if superseded_count == 0 {
        tracing::warn!(
            version,
            "observed a new forge version, but it didn't supersede anything. might be first deployment."
        );
    } else if superseded_count > 1 {
        tracing::warn!(
            version,
            superseded_version_count = superseded_count,
            "observed a new forge version, superseded versions",
        );
    }

    {
        let query = "INSERT INTO forge_versions (version) VALUES ($1)";
        sqlx::query(query)
            .bind(version)
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
    }

    Ok(true)
}

/// Return the date the given version was superseded, if found.
pub async fn date_superseded(
    txn: &mut PgConnection,
    version: &str,
) -> Result<Option<DateTime<Utc>>, DatabaseError> {
    let query = "SELECT superseded FROM forge_versions WHERE version = $1";

    // double-option here because it's a nullable value *and* the query may not return a row.
    let result: Option<Option<DateTime<Utc>>> = sqlx::query_scalar(query)
        .bind(version)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(result.flatten())
}

/// Tests only: Create a mock observation at a given date, optionally marked as already superseded
pub async fn make_mock_observation(
    txn: &mut PgConnection,
    version: &str,
    superseded: Option<DateTime<Utc>>,
) -> Result<(), DatabaseError> {
    let query = "INSERT INTO forge_versions (version, superseded, first_seen) VALUES ($1, $2, $3)";
    sqlx::query(query)
        .bind(version)
        .bind(superseded)
        .bind(superseded.unwrap_or(Utc::now()))
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}
