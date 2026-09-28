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

use carbide_uuid::nvlink::NvLinkDomainId;
use health_report::{HealthReport, HealthReportApplyMode};
use model::health::HealthReportSources;
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::db_read::DbReader;

const TABLE_NAME: &str = "nvlink_domain_health_reports";

/// Finds the health report sources stored for an NVLink domain.
pub async fn find(
    txn: impl DbReader<'_>,
    domain_id: &NvLinkDomainId,
) -> Result<Option<HealthReportSources>, DatabaseError> {
    let query = "SELECT health_reports FROM nvlink_domain_health_reports WHERE id = $1";
    let health_reports = sqlx::query_scalar::<_, sqlx::types::Json<HealthReportSources>>(query)
        .bind(domain_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(health_reports.map(|json| json.0))
}

/// Lists NVLink domain IDs that have stored health reports.
pub async fn list_domain_ids(txn: impl DbReader<'_>) -> Result<Vec<NvLinkDomainId>, DatabaseError> {
    let query = "SELECT id FROM nvlink_domain_health_reports ORDER BY id";
    let ids = sqlx::query_scalar::<_, NvLinkDomainId>(query)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(ids)
}

/// Inserts or updates one health report source for an NVLink domain.
pub async fn insert_health_report(
    txn: &mut PgConnection,
    domain_id: &NvLinkDomainId,
    mode: HealthReportApplyMode,
    health_report: &HealthReport,
) -> Result<(), DatabaseError> {
    ensure_row(txn, domain_id).await?;

    crate::health_report::insert_health_report(txn, TABLE_NAME, domain_id, mode, health_report)
        .await
}

/// Removes one health report source from an NVLink domain.
pub async fn remove_health_report(
    txn: &mut PgConnection,
    domain_id: &NvLinkDomainId,
    mode: HealthReportApplyMode,
    source: &str,
) -> Result<(), DatabaseError> {
    crate::health_report::remove_health_report(txn, TABLE_NAME, domain_id, mode, source).await?;

    let query = r#"
        DELETE FROM nvlink_domain_health_reports
        WHERE id = $1
          AND health_reports IN (
              '{"merges": {}}'::jsonb,
              '{"merges": {}, "replace": null}'::jsonb
          )
        "#;

    sqlx::query(query)
        .bind(domain_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new("delete empty NVLink domain health reports", e))?;

    Ok(())
}

/// Creates the domain row before applying a JSON health-report update.
async fn ensure_row(
    txn: &mut PgConnection,
    domain_id: &NvLinkDomainId,
) -> Result<(), DatabaseError> {
    // Health reports can arrive before inventory creates an NVLink domain row,
    // so inserts lazily create the container row and then use the shared JSON
    // update path.
    let query =
        "INSERT INTO nvlink_domain_health_reports (id) VALUES ($1) ON CONFLICT (id) DO NOTHING";

    sqlx::query(query)
        .bind(domain_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new(query, e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sqlx::PgPool;

    use super::*;

    const CLEANUP_MIGRATION: &str =
        include_str!("../migrations/20260916170617_delete_empty_nvlink_domain_health_reports.sql");
    const DATABASE_DEFAULT_EMPTY_ID: &str = "00000000-0000-0000-0000-000000000001";
    const SERIALIZED_DEFAULT_EMPTY_ID: &str = "00000000-0000-0000-0000-000000000002";
    const MERGE_SOURCE_ID: &str = "00000000-0000-0000-0000-000000000003";
    const REPLACE_SOURCE_ID: &str = "00000000-0000-0000-0000-000000000004";
    const UNRECOGNIZED_DATA_ID: &str = "00000000-0000-0000-0000-000000000005";

    async fn insert_sources(pool: &PgPool, domain_id: &str, sources: HealthReportSources) {
        sqlx::query(
            "INSERT INTO nvlink_domain_health_reports (id, health_reports) VALUES ($1::uuid, $2)",
        )
        .bind(domain_id)
        .bind(sqlx::types::Json(sources))
        .execute(pool)
        .await
        .unwrap();
    }

    #[crate::sqlx_test]
    async fn cleanup_migration_removes_only_known_empty_rows(pool: PgPool) {
        // The harness already applied the cleanup to an empty database. Seed the
        // predecessor data shapes, then replay the shipped SQL to exercise its
        // data semantics.
        sqlx::query("INSERT INTO nvlink_domain_health_reports (id) VALUES ($1::uuid)")
            .bind(DATABASE_DEFAULT_EMPTY_ID)
            .execute(&pool)
            .await
            .unwrap();
        insert_sources(
            &pool,
            SERIALIZED_DEFAULT_EMPTY_ID,
            HealthReportSources::default(),
        )
        .await;

        let merge_report = HealthReport::empty("merge-source".to_string());
        let merge_sources = HealthReportSources {
            replace: None,
            merges: BTreeMap::from([(merge_report.source.clone(), merge_report)]),
        };
        insert_sources(&pool, MERGE_SOURCE_ID, merge_sources).await;

        let replace_sources = HealthReportSources {
            replace: Some(HealthReport::empty("replace-source".to_string())),
            merges: BTreeMap::new(),
        };
        insert_sources(&pool, REPLACE_SOURCE_ID, replace_sources).await;

        sqlx::query(
            "INSERT INTO nvlink_domain_health_reports (id, health_reports) VALUES ($1::uuid, $2)",
        )
        .bind(UNRECOGNIZED_DATA_ID)
        .bind(sqlx::types::Json(serde_json::json!({
            "merges": {},
            "replace": null,
            "future": { "owner": "external-health-service" }
        })))
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(CLEANUP_MIGRATION)
            .execute(&pool)
            .await
            .unwrap();

        let remaining_ids: Vec<String> =
            sqlx::query_scalar("SELECT id::text FROM nvlink_domain_health_reports ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            remaining_ids,
            vec![
                MERGE_SOURCE_ID.to_string(),
                REPLACE_SOURCE_ID.to_string(),
                UNRECOGNIZED_DATA_ID.to_string(),
            ]
        );
    }
}
