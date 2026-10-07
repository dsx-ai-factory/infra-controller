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

use model::nmxc::NvlinkNmxcEndpoint;
use sqlx::PgConnection;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

pub async fn find_by_chassis_serial(
    txn: impl DbReader<'_>,
    chassis_serial: &str,
) -> DatabaseResult<Option<NvlinkNmxcEndpoint>> {
    const Q: &str =
        "SELECT chassis_serial, endpoint FROM nvlink_nmxc_endpoints WHERE chassis_serial = $1";
    sqlx::query_as(Q)
        .bind(chassis_serial)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new(Q, e))
}

pub async fn find_by_chassis_serials(
    txn: impl DbReader<'_>,
    chassis_serials: &[&str],
) -> DatabaseResult<Vec<NvlinkNmxcEndpoint>> {
    const Q: &str =
        "SELECT chassis_serial, endpoint FROM nvlink_nmxc_endpoints WHERE chassis_serial = ANY($1)";
    sqlx::query_as(Q)
        .bind(chassis_serials)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new(Q, e))
}

pub async fn find_all(txn: impl DbReader<'_>) -> DatabaseResult<Vec<NvlinkNmxcEndpoint>> {
    const Q: &str =
        "SELECT chassis_serial, endpoint FROM nvlink_nmxc_endpoints ORDER BY chassis_serial";
    sqlx::query_as(Q)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::new(Q, e))
}

pub async fn create(
    txn: &mut PgConnection,
    chassis_serial: &str,
    endpoint: &str,
) -> DatabaseResult<NvlinkNmxcEndpoint> {
    const Q: &str = "INSERT INTO nvlink_nmxc_endpoints (chassis_serial, endpoint) VALUES ($1, $2) RETURNING chassis_serial, endpoint";
    sqlx::query_as(Q)
        .bind(chassis_serial)
        .bind(endpoint)
        .fetch_one(txn)
        .await
        .map_err(|e| {
            if e.as_database_error()
                .is_some_and(|e| e.is_unique_violation())
            {
                DatabaseError::AlreadyFoundError {
                    kind: "nvlink_nmxc_endpoints",
                    id: chassis_serial.to_string(),
                }
            } else {
                DatabaseError::new(Q, e)
            }
        })
}

/// Deletes the row for `chassis_serial`. Returns `true` if a row was removed.
pub async fn delete(txn: &mut PgConnection, chassis_serial: &str) -> DatabaseResult<bool> {
    const Q: &str = "DELETE FROM nvlink_nmxc_endpoints WHERE chassis_serial = $1";
    let res = sqlx::query(Q)
        .bind(chassis_serial)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::new(Q, e))?;
    Ok(res.rows_affected() > 0)
}

/// Sets `endpoint` for `chassis_serial`. Returns the updated row if it existed.
pub async fn update(
    txn: &mut PgConnection,
    chassis_serial: &str,
    endpoint: &str,
) -> DatabaseResult<Option<NvlinkNmxcEndpoint>> {
    const Q: &str = "UPDATE nvlink_nmxc_endpoints SET endpoint = $1 WHERE chassis_serial = $2 RETURNING chassis_serial, endpoint";
    sqlx::query_as(Q)
        .bind(endpoint)
        .bind(chassis_serial)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::new(Q, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[crate::sqlx_test]
    async fn find_by_chassis_serials_returns_matching_rows(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        create(txn.as_mut(), "SN-A", "https://a.example:9370")
            .await
            .unwrap();
        create(txn.as_mut(), "SN-B", "https://b.example:9370")
            .await
            .unwrap();
        create(txn.as_mut(), "SN-C", "https://c.example:9370")
            .await
            .unwrap();

        let mut rows = find_by_chassis_serials(txn.as_mut(), &["SN-A", "SN-C"])
            .await
            .unwrap();
        rows.sort_by(|a, b| a.chassis_serial.cmp(&b.chassis_serial));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].chassis_serial, "SN-A");
        assert_eq!(rows[0].endpoint, "https://a.example:9370");
        assert_eq!(rows[1].chassis_serial, "SN-C");
        assert_eq!(rows[1].endpoint, "https://c.example:9370");

        txn.rollback().await.unwrap();
    }

    #[crate::sqlx_test]
    async fn find_by_chassis_serials_unknown_serials_are_excluded(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        create(txn.as_mut(), "SN-KNOWN", "https://known.example:9370")
            .await
            .unwrap();

        let rows = find_by_chassis_serials(txn.as_mut(), &["SN-KNOWN", "SN-MISSING"])
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].chassis_serial, "SN-KNOWN");

        txn.rollback().await.unwrap();
    }

    #[crate::sqlx_test]
    async fn find_by_chassis_serials_empty_slice_returns_empty(pool: sqlx::PgPool) {
        let mut txn = pool.begin().await.unwrap();
        create(txn.as_mut(), "SN-X", "https://x.example:9370")
            .await
            .unwrap();

        let rows = find_by_chassis_serials(txn.as_mut(), &[]).await.unwrap();
        assert!(rows.is_empty());

        txn.rollback().await.unwrap();
    }
}
