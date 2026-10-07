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
use std::hash::Hasher;

use chrono::{DateTime, Utc};
use model::health::HealthHistoryRecord;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, Row};

use crate::DatabaseError;

/// History of health for a single Object
#[derive(Debug, Clone)]
struct DbHealthHistoryRecord {
    /// The ID of the object associated with the health record
    pub object_id: String,

    /// The observed health of the object
    pub health: health_report::HealthReport,

    /// The time when the health was observed
    pub time: DateTime<Utc>,
}

impl<'r> FromRow<'r, PgRow> for DbHealthHistoryRecord {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(DbHealthHistoryRecord {
            object_id: row.try_get("object_id")?,
            health: row
                .try_get::<sqlx::types::Json<health_report::HealthReport>, _>("health")?
                .0,
            time: row.try_get("time")?,
        })
    }
}

impl From<DbHealthHistoryRecord> for model::health::HealthHistoryRecord {
    fn from(record: DbHealthHistoryRecord) -> Self {
        Self {
            health: record.health,
            time: record.time,
        }
    }
}

/// Identifies the table that is used to store health history
#[derive(Debug, Copy, Clone)]
pub enum HealthHistoryTableId {
    Machine,
    Switch,
    PowerShelf,
    Rack,
}

impl HealthHistoryTableId {
    pub fn sql_table(self) -> &'static str {
        match self {
            HealthHistoryTableId::Machine => "machine_health_history",
            HealthHistoryTableId::Switch => "switch_health_history",
            HealthHistoryTableId::PowerShelf => "power_shelf_health_history",
            HealthHistoryTableId::Rack => "rack_health_history",
        }
    }
}

/// Retrieve the health history for a list of Objects
///
/// It returns a [HashMap][std::collections::HashMap] keyed by the object ID and
/// the history of health that has been observed by the object, starting with the
/// newest.
pub async fn find_by_object_ids(
    txn: &mut PgConnection,
    table_id: HealthHistoryTableId,
    ids: &[impl std::fmt::Display],
    // Only include records between the given start and end time
    start_time: Option<DateTime<Utc>>,
    end_time: Option<DateTime<Utc>>,
) -> Result<std::collections::HashMap<String, Vec<HealthHistoryRecord>>, DatabaseError> {
    let mut qb = sqlx::QueryBuilder::new("SELECT object_id, health, time FROM ");
    qb.push(table_id.sql_table());
    qb.push(" WHERE object_id IN");

    qb.push(" (");
    let mut separated = qb.separated(", ");
    for id in ids {
        separated.push_bind(id.to_string());
    }
    qb.push(")");

    if let Some(start_time) = start_time {
        qb.push(" AND time >= ");
        qb.push_bind(start_time);
    }
    if let Some(end_time) = end_time {
        qb.push(" AND time <= ");
        qb.push_bind(end_time);
    }

    qb.push(" ORDER BY id DESC");

    let query = qb.build_query_as();
    let query_results: Vec<DbHealthHistoryRecord> = query
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query("find_health_history", e))?;

    let mut histories = std::collections::HashMap::new();
    for result in query_results.into_iter() {
        let records: &mut Vec<HealthHistoryRecord> =
            histories.entry(result.object_id.clone()).or_default();
        records.push(result.into());
    }
    Ok(histories)
}

/// Compile-time SQL template for [`persist`]. The table name is spliced into a
/// single shared body (so the Machine/Switch variants can't drift), and every
/// expansion is a `&'static str` with no per-call allocation, which matters
/// because `persist` is on a hot path.
macro_rules! persist_query {
    ($table:literal) => {
        concat!(
            "WITH new_history_record as(
            SELECT $1 as object_id,
            $2::jsonb as health,
            $3 as health_hash,
            $4 as time
        ),
        last_history_record as(
            SELECT health_hash FROM ",
            $table,
            "
            WHERE object_id = $1
            ORDER BY id DESC
            LIMIT 1
        )
        INSERT INTO ",
            $table,
            " (object_id, health, health_hash, time)
        SELECT * FROM new_history_record
        WHERE NOT EXISTS (SELECT health_hash FROM last_history_record WHERE last_history_record.health_hash = new_history_record.health_hash);"
        )
    };
}

/// Store a new health history record for an object
pub async fn persist(
    txn: &mut PgConnection,
    table_id: HealthHistoryTableId,
    object_id: &impl std::fmt::Display,
    health: &health_report::HealthReport,
) -> Result<(), DatabaseError> {
    // Calculate a hash value of the Report, that we can compare to the latest
    // health value written.
    // If the report did not change, skip the insert.
    // This behavior is achieved by using a sub-query to extract the last written
    // hash for an object, and comparing it to the most recent hash.
    // Note: Since it uses a hash, there is a minor chance of not writing an
    // entry even if health changed.
    let mut hasher = rustc_hash::FxHasher::default();
    health.hash_without_timestamps(&mut hasher);
    let health_hash = format!("{:#x}", hasher.finish());

    let query = match table_id {
        HealthHistoryTableId::Machine => persist_query!("machine_health_history"),
        HealthHistoryTableId::Switch => persist_query!("switch_health_history"),
        HealthHistoryTableId::PowerShelf => persist_query!("power_shelf_health_history"),
        HealthHistoryTableId::Rack => persist_query!("rack_health_history"),
    };
    let _query_result = sqlx::query(query)
        .bind(object_id.to_string())
        .bind(sqlx::types::Json(health))
        .bind(health_hash)
        .bind(chrono::Utc::now())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(())
}

/// Renames all health entries using one Object ID into using another Object ID
pub async fn update_object_ids(
    txn: &mut PgConnection,
    table_id: HealthHistoryTableId,
    old_object_id: &impl std::fmt::Display,
    new_object_id: &impl std::fmt::Display,
) -> Result<(), DatabaseError> {
    // `sqlx::query` requires a `&'static str` (SqlSafeStr), so build each arm at
    // compile time via `concat!` rather than allocating with `format!`.
    macro_rules! update_object_ids_query {
        ($table:literal) => {
            concat!("UPDATE ", $table, " SET object_id=$1 WHERE object_id=$2")
        };
    }
    let query = match table_id {
        HealthHistoryTableId::Machine => update_object_ids_query!("machine_health_history"),
        HealthHistoryTableId::Switch => update_object_ids_query!("switch_health_history"),
        HealthHistoryTableId::PowerShelf => {
            update_object_ids_query!("power_shelf_health_history")
        }
        HealthHistoryTableId::Rack => update_object_ids_query!("rack_health_history"),
    };
    sqlx::query(query)
        .bind(new_object_id.to_string())
        .bind(old_object_id.to_string())
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::{HealthHistoryTableId, find_by_object_ids, persist, update_object_ids};

    /// Builds a health report carrying a single alert, so that distinct
    /// `alert_id`s hash to distinct values (the source is not part of the hash).
    fn health_with_alert(alert_id: &str) -> health_report::HealthReport {
        let mut report = health_report::HealthReport::empty("switch-aggregate-health".to_string());
        report.alerts.push(health_report::HealthProbeAlert {
            id: alert_id.parse().unwrap(),
            target: None,
            in_alert_since: None,
            message: format!("{alert_id} message"),
            tenant_message: None,
            classifications: vec![],
        });
        report
    }

    async fn count_records(
        conn: &mut sqlx::PgConnection,
        table_id: HealthHistoryTableId,
        object_id: &str,
    ) -> usize {
        find_by_object_ids(conn, table_id, &[object_id], None, None)
            .await
            .unwrap()
            .get(object_id)
            .map(Vec::len)
            .unwrap_or(0)
    }

    /// Verify that `persist_query!` must reproduce the sql statement
    /// that was previously hand-written for the machine table.
    #[test]
    fn persist_query_matches_original_machine_literal() {
        let original = "WITH new_history_record as(
            SELECT $1 as object_id,
            $2::jsonb as health,
            $3 as health_hash,
            $4 as time
        ),
        last_history_record as(
            SELECT health_hash FROM machine_health_history
            WHERE object_id = $1
            ORDER BY id DESC
            LIMIT 1
        )
        INSERT INTO machine_health_history (object_id, health, health_hash, time)
        SELECT * FROM new_history_record
        WHERE NOT EXISTS (SELECT health_hash FROM last_history_record WHERE last_history_record.health_hash = new_history_record.health_hash);";
        assert_eq!(persist_query!("machine_health_history"), original);
    }

    /// Verify the produced sql statement for the switch table.
    #[test]
    fn persist_query_matches_original_switch_literal() {
        let original = "WITH new_history_record as(
            SELECT $1 as object_id,
            $2::jsonb as health,
            $3 as health_hash,
            $4 as time
        ),
        last_history_record as(
            SELECT health_hash FROM switch_health_history
            WHERE object_id = $1
            ORDER BY id DESC
            LIMIT 1
        )
        INSERT INTO switch_health_history (object_id, health, health_hash, time)
        SELECT * FROM new_history_record
        WHERE NOT EXISTS (SELECT health_hash FROM last_history_record WHERE last_history_record.health_hash = new_history_record.health_hash);";
        assert_eq!(persist_query!("switch_health_history"), original);
    }

    /// Verify the produced sql statement for the power shelf table.
    #[test]
    fn persist_query_matches_original_power_shelf_literal() {
        let original = "WITH new_history_record as(
            SELECT $1 as object_id,
            $2::jsonb as health,
            $3 as health_hash,
            $4 as time
        ),
        last_history_record as(
            SELECT health_hash FROM power_shelf_health_history
            WHERE object_id = $1
            ORDER BY id DESC
            LIMIT 1
        )
        INSERT INTO power_shelf_health_history (object_id, health, health_hash, time)
        SELECT * FROM new_history_record
        WHERE NOT EXISTS (SELECT health_hash FROM last_history_record WHERE last_history_record.health_hash = new_history_record.health_hash);";
        assert_eq!(persist_query!("power_shelf_health_history"), original);
    }

    /// Verify the produced sql statement for the rack table.
    #[test]
    fn persist_query_matches_original_rack_literal() {
        let original = "WITH new_history_record as(
            SELECT $1 as object_id,
            $2::jsonb as health,
            $3 as health_hash,
            $4 as time
        ),
        last_history_record as(
            SELECT health_hash FROM rack_health_history
            WHERE object_id = $1
            ORDER BY id DESC
            LIMIT 1
        )
        INSERT INTO rack_health_history (object_id, health, health_hash, time)
        SELECT * FROM new_history_record
        WHERE NOT EXISTS (SELECT health_hash FROM last_history_record WHERE last_history_record.health_hash = new_history_record.health_hash);";
        assert_eq!(persist_query!("rack_health_history"), original);
    }

    #[crate::sqlx_test]
    async fn switch_health_history_dedup_and_retention(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let object_id = "switch-health-history-test";

        // First observation is recorded.
        persist(
            &mut conn,
            HealthHistoryTableId::Switch,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, object_id).await,
            1
        );

        // Re-observing identical content is deduplicated by health_hash.
        persist(
            &mut conn,
            HealthHistoryTableId::Switch,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, object_id).await,
            1
        );

        // A timestamp-only change is still the same content, so no new row.
        let mut timestamp_only = health_with_alert("AlertA");
        timestamp_only.observed_at = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
        persist(
            &mut conn,
            HealthHistoryTableId::Switch,
            &object_id,
            &timestamp_only,
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, object_id).await,
            1
        );

        // Changed content produces a new row.
        persist(
            &mut conn,
            HealthHistoryTableId::Switch,
            &object_id,
            &health_with_alert("AlertB"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, object_id).await,
            2
        );

        // The retention trigger keeps only the most recent 250 rows per object.
        const EXPECTED_LIMIT: usize = 250;
        for i in 0..EXPECTED_LIMIT + 10 {
            persist(
                &mut conn,
                HealthHistoryTableId::Switch,
                &object_id,
                &health_with_alert(&format!("Alert{i}")),
            )
            .await?;
        }
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, object_id).await,
            EXPECTED_LIMIT
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn switch_health_history_update_object_ids(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let old_object_id = "switch-old-id";
        let new_object_id = "switch-new-id";

        persist(
            &mut conn,
            HealthHistoryTableId::Switch,
            &old_object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, old_object_id).await,
            1
        );

        update_object_ids(
            &mut conn,
            HealthHistoryTableId::Switch,
            &old_object_id,
            &new_object_id,
        )
        .await?;

        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, old_object_id).await,
            0
        );
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Switch, new_object_id).await,
            1
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn power_shelf_health_history_dedup_and_retention(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let object_id = "power-shelf-health-history-test";

        // First observation is recorded.
        persist(
            &mut conn,
            HealthHistoryTableId::PowerShelf,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, object_id).await,
            1
        );

        // Re-observing identical content is deduplicated by health_hash.
        persist(
            &mut conn,
            HealthHistoryTableId::PowerShelf,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, object_id).await,
            1
        );

        // A timestamp-only change is still the same content, so no new row.
        let mut timestamp_only = health_with_alert("AlertA");
        timestamp_only.observed_at = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
        persist(
            &mut conn,
            HealthHistoryTableId::PowerShelf,
            &object_id,
            &timestamp_only,
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, object_id).await,
            1
        );

        // Changed content produces a new row.
        persist(
            &mut conn,
            HealthHistoryTableId::PowerShelf,
            &object_id,
            &health_with_alert("AlertB"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, object_id).await,
            2
        );

        // The retention trigger keeps only the most recent 250 rows per object.
        const EXPECTED_LIMIT: usize = 250;
        for i in 0..EXPECTED_LIMIT + 10 {
            persist(
                &mut conn,
                HealthHistoryTableId::PowerShelf,
                &object_id,
                &health_with_alert(&format!("Alert{i}")),
            )
            .await?;
        }
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, object_id).await,
            EXPECTED_LIMIT
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn power_shelf_health_history_update_object_ids(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let old_object_id = "power-shelf-old-id";
        let new_object_id = "power-shelf-new-id";

        persist(
            &mut conn,
            HealthHistoryTableId::PowerShelf,
            &old_object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, old_object_id).await,
            1
        );

        update_object_ids(
            &mut conn,
            HealthHistoryTableId::PowerShelf,
            &old_object_id,
            &new_object_id,
        )
        .await?;

        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, old_object_id).await,
            0
        );
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::PowerShelf, new_object_id).await,
            1
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn rack_health_history_dedup_and_retention(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let object_id = "rack-health-history-test";

        // First observation is recorded.
        persist(
            &mut conn,
            HealthHistoryTableId::Rack,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, object_id).await,
            1
        );

        // Re-observing identical content is deduplicated by health_hash.
        persist(
            &mut conn,
            HealthHistoryTableId::Rack,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, object_id).await,
            1
        );

        // A timestamp-only change is still the same content, so no new row.
        let mut timestamp_only = health_with_alert("AlertA");
        timestamp_only.observed_at = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
        persist(
            &mut conn,
            HealthHistoryTableId::Rack,
            &object_id,
            &timestamp_only,
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, object_id).await,
            1
        );

        // Changed content produces a new row.
        persist(
            &mut conn,
            HealthHistoryTableId::Rack,
            &object_id,
            &health_with_alert("AlertB"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, object_id).await,
            2
        );

        // The retention trigger keeps only the most recent 250 rows per object.
        const EXPECTED_LIMIT: usize = 250;
        for i in 0..EXPECTED_LIMIT + 10 {
            persist(
                &mut conn,
                HealthHistoryTableId::Rack,
                &object_id,
                &health_with_alert(&format!("Alert{i}")),
            )
            .await?;
        }
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, object_id).await,
            EXPECTED_LIMIT
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn rack_health_history_update_object_ids(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let old_object_id = "rack-old-id";
        let new_object_id = "rack-new-id";

        persist(
            &mut conn,
            HealthHistoryTableId::Rack,
            &old_object_id,
            &health_with_alert("AlertA"),
        )
        .await?;
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, old_object_id).await,
            1
        );

        update_object_ids(
            &mut conn,
            HealthHistoryTableId::Rack,
            &old_object_id,
            &new_object_id,
        )
        .await?;

        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, old_object_id).await,
            0
        );
        assert_eq!(
            count_records(&mut conn, HealthHistoryTableId::Rack, new_object_id).await,
            1
        );

        Ok(())
    }

    /// `find_by_object_ids` filters by the inclusive `[start_time, end_time]`
    /// window. The filter SQL is table-agnostic, so exercising one table proves
    /// it for all `HealthHistoryTableId` variants.
    #[crate::sqlx_test]
    async fn find_by_object_ids_filters_by_time_range(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut conn = pool.acquire().await?;
        let object_id = "health-history-range-test";
        let ids = [object_id];

        // Two records with distinct content (so neither is deduplicated) written
        // at distinct times, with a boundary captured between them.
        let before = chrono::Utc::now();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        persist(
            &mut conn,
            HealthHistoryTableId::Machine,
            &object_id,
            &health_with_alert("AlertA"),
        )
        .await?;

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let between = chrono::Utc::now();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        persist(
            &mut conn,
            HealthHistoryTableId::Machine,
            &object_id,
            &health_with_alert("AlertB"),
        )
        .await?;

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let after = chrono::Utc::now();

        // The full window returns both records.
        let all = find_by_object_ids(
            &mut conn,
            HealthHistoryTableId::Machine,
            &ids,
            Some(before),
            Some(after),
        )
        .await?;
        assert_eq!(all.get(object_id).map(Vec::len).unwrap_or(0), 2);

        // An end_time before the second write excludes it, leaving only AlertA.
        let first_only = find_by_object_ids(
            &mut conn,
            HealthHistoryTableId::Machine,
            &ids,
            None,
            Some(between),
        )
        .await?
        .remove(object_id)
        .unwrap_or_default();
        assert_eq!(first_only.len(), 1);
        assert_eq!(
            first_only[0].health.alerts[0].message.as_str(),
            "AlertA message"
        );

        // A start_time after the first write excludes it, leaving only AlertB.
        let second_only = find_by_object_ids(
            &mut conn,
            HealthHistoryTableId::Machine,
            &ids,
            Some(between),
            None,
        )
        .await?
        .remove(object_id)
        .unwrap_or_default();
        assert_eq!(second_only.len(), 1);
        assert_eq!(
            second_only[0].health.alerts[0].message.as_str(),
            "AlertB message"
        );

        // A window entirely after both writes returns nothing.
        let future = find_by_object_ids(
            &mut conn,
            HealthHistoryTableId::Machine,
            &ids,
            Some(after + chrono::Duration::hours(1)),
            None,
        )
        .await?;
        assert_eq!(future.get(object_id).map(Vec::len).unwrap_or(0), 0);

        Ok(())
    }
}
