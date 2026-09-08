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

use sqlx::PgTransaction;

use crate::{DatabaseError, DatabaseResult};

/// `lock_checks` serializes participating transactions that could create an
/// overlap between a `VpcPrefix` and another stored prefix.
///
/// Callers acquire it before any resource lock, then read, validate, and write
/// in the same transaction. Otherwise, two requests can each see no overlap
/// and both commit. PostgreSQL releases the lock on commit or rollback.
pub async fn lock_checks(txn: &mut PgTransaction<'_>) -> DatabaseResult<()> {
    let query = "SELECT pg_advisory_xact_lock(\
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
