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

//! Database coordination for tenant prefix overlap checks.
//!
//! Retained-prefix membership matches [`crate::vpc_peering::get_retained_prefixes_by_vpcs`].
//! Keep the direct-segment filters aligned. `NOT MATERIALIZED` lets both CTE
//! references use the underlying prefix indexes.

use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use sqlx::PgTransaction;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// `find_duplicate_vpc_ids` finds both VPCs in each retained overlap involving
/// a tenant-managed `VpcPrefix`. `include_legacy` also selects overlaps between
/// operator-managed or rootless prefixes for an explicitly enabled site's audit.
/// Deleting rows remain included. Callers hold the overlap lock while using
/// these results to validate routing.
pub async fn find_duplicate_vpc_ids(
    txn: impl DbReader<'_>,
    include_legacy: bool,
) -> DatabaseResult<Vec<VpcId>> {
    let query = "WITH prefixes AS NOT MATERIALIZED (
            SELECT vp.vpc_id, vp.prefix,
                COALESCE(sp.authority = 'tenant_managed', false) AS tenant_managed
            FROM network_vpc_prefixes vp
            LEFT JOIN site_prefixes sp ON sp.id = vp.site_prefix_id
            UNION ALL
            SELECT ns.vpc_id, np.prefix, false FROM network_prefixes np
            JOIN network_segments ns ON ns.id = np.segment_id
            WHERE np.vpc_prefix_id IS NULL AND ns.vpc_id IS NOT NULL
        )
        SELECT DISTINCT a.vpc_id FROM prefixes a
        WHERE EXISTS (SELECT 1 FROM prefixes b
            WHERE a.vpc_id <> b.vpc_id AND a.prefix && b.prefix
                AND ($1 OR a.tenant_managed OR b.tenant_managed))
        ORDER BY a.vpc_id";
    sqlx::query_scalar(query)
        .bind(include_legacy)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `vpcs_use_duplicate_space` checks the supplied sources for retained overlaps
/// involving a tenant-managed `VpcPrefix` on either VPC. Unrelated tenant roots
/// do not activate checks for legacy overlaps. Callers hold the overlap lock;
/// deleting rows remain included so disabling admission cannot bypass isolation.
pub async fn vpcs_use_duplicate_space(
    txn: impl DbReader<'_>,
    vpc_ids: &[VpcId],
) -> DatabaseResult<bool> {
    let query = "WITH prefixes AS NOT MATERIALIZED (
            SELECT vp.vpc_id, vp.prefix,
                COALESCE(sp.authority = 'tenant_managed', false) AS tenant_managed
            FROM network_vpc_prefixes vp
            LEFT JOIN site_prefixes sp ON sp.id = vp.site_prefix_id
            UNION ALL
            SELECT ns.vpc_id, np.prefix, false FROM network_prefixes np
            JOIN network_segments ns ON ns.id = np.segment_id
            WHERE np.vpc_prefix_id IS NULL AND ns.vpc_id IS NOT NULL
        )
        SELECT EXISTS (SELECT 1 FROM prefixes a
            WHERE a.vpc_id = ANY($1) AND EXISTS (SELECT 1 FROM prefixes b
                WHERE a.vpc_id <> b.vpc_id AND a.prefix && b.prefix
                    AND (a.tenant_managed OR b.tenant_managed)))";
    sqlx::query_scalar(query)
        .bind(vpc_ids)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `has_direct_prefix_overlap` detects retained overlaps involving a direct
/// segment prefix and any supplied VPC. An empty selection returns false.
/// Only VPC prefixes can establish isolated tenant reuse.
pub async fn has_direct_prefix_overlap(
    txn: impl DbReader<'_>,
    vpc_ids: &[VpcId],
) -> DatabaseResult<bool> {
    let query = "WITH prefixes AS NOT MATERIALIZED (
            SELECT vpc_id, prefix, id AS vpc_prefix_id FROM network_vpc_prefixes
            UNION ALL
            SELECT ns.vpc_id, np.prefix, NULL::uuid FROM network_prefixes np
            JOIN network_segments ns ON ns.id = np.segment_id
            WHERE np.vpc_prefix_id IS NULL AND ns.vpc_id IS NOT NULL
        )
        SELECT EXISTS (SELECT 1 FROM prefixes a
            WHERE a.vpc_prefix_id IS NULL AND EXISTS (SELECT 1 FROM prefixes b
                WHERE a.vpc_id <> b.vpc_id AND a.prefix && b.prefix
                    AND (a.vpc_id = ANY($1) OR b.vpc_id = ANY($1))))";
    sqlx::query_scalar(query)
        .bind(vpc_ids)
        .fetch_one(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `find_overlapping_vpc_prefix_pairs` returns each retained cross-VPC overlap
/// involving a supplied VPC once, ordered by both prefix IDs. Empty selections
/// return no pairs. Same-VPC parent/child relationships are not routing collisions.
pub async fn find_overlapping_vpc_prefix_pairs(
    txn: impl DbReader<'_>,
    vpc_ids: &[VpcId],
) -> DatabaseResult<Vec<(VpcPrefixId, VpcPrefixId)>> {
    let query = "SELECT a.id, b.id FROM network_vpc_prefixes a
        JOIN network_vpc_prefixes b ON a.prefix && b.prefix
        WHERE a.id < b.id AND a.vpc_id <> b.vpc_id
            AND (a.vpc_id = ANY($1) OR b.vpc_id = ANY($1)) ORDER BY a.id, b.id";
    sqlx::query_as(query)
        .bind(vpc_ids)
        .fetch_all(txn)
        .await
        .map_err(|error| DatabaseError::query(query, error))
}

/// `lock_checks` serializes participating transactions that could create an
/// overlap between a `VpcPrefix` and another stored prefix, or add a SitePrefix
/// to the site's isolation rules.
///
/// Callers acquire it before any resource lock, then read, validate, and write
/// in the same transaction. Otherwise, two requests can each see no overlap or
/// enough rule capacity and both commit. PostgreSQL releases the lock on commit
/// or rollback.
pub async fn lock_checks(txn: &mut PgTransaction<'_>) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock(\
            hashtextextended('tenant_prefix_overlap:checks', 0))";
    sqlx::query(query)
        .execute(&mut **txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// `lock_config` keeps routing writers out while configuration is checked and
/// rendered, but allows unrelated DPU requests to run together. Take it before
/// resource locks, just like [`lock_checks`]; never upgrade it to a writer lock.
pub async fn lock_config(txn: &mut PgTransaction<'_>) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock_shared(\
            hashtextextended('tenant_prefix_overlap:checks', 0))";
    sqlx::query(query)
        .execute(&mut **txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::PgPool;

    use super::*;

    #[crate::sqlx_test]
    async fn config_readers_share_the_lock_and_exclude_writers(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut first = pool.begin().await?;
        lock_config(&mut first).await?;
        let mut second = pool.begin().await?;
        tokio::time::timeout(Duration::from_secs(5), lock_config(&mut second)).await??;
        let mut writer = pool.begin().await?;
        let query = "SELECT pg_try_advisory_xact_lock(\
            hashtextextended('tenant_prefix_overlap:checks', 0))";
        assert!(
            !sqlx::query_scalar::<_, bool>(query)
                .fetch_one(&mut *writer)
                .await?
        );
        first.rollback().await?;
        assert!(
            !sqlx::query_scalar::<_, bool>(query)
                .fetch_one(&mut *writer)
                .await?
        );
        second.commit().await?;
        tokio::time::timeout(Duration::from_secs(5), lock_checks(&mut writer)).await??;
        writer.commit().await?;
        Ok(())
    }

    /// Test-specific function that checks rollback releases the overlap lock.
    #[crate::sqlx_test]
    async fn rollback_releases_overlap_checks_lock(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut holder = pool.begin().await?;
        lock_checks(&mut holder).await?;
        let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *holder)
            .await?;

        let mut waiter = pool.begin().await?;
        let waiter_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *waiter)
            .await?;

        let wait_for_lock = async {
            tokio::time::timeout(Duration::from_secs(5), lock_checks(&mut waiter)).await??;
            waiter.commit().await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };
        let release_lock = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let blocked_by_holder: bool =
                        sqlx::query_scalar("SELECT $1 = ANY(pg_blocking_pids($2))")
                            .bind(holder_pid)
                            .bind(waiter_pid)
                            .fetch_one(&pool)
                            .await?;
                    if blocked_by_holder {
                        return Ok::<(), sqlx::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            holder.rollback().await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        };

        let (waiter_result, holder_result) = tokio::join!(wait_for_lock, release_lock);
        waiter_result?;
        holder_result?;
        Ok(())
    }
}
