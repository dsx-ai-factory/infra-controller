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

//! PostgreSQL coordination helpers for database and downstream tests.

use sqlx::PgPool;

/// Backend PIDs of the sessions in a lock wait behind `blocker_pid` whose
/// query contains `query_fragment`, lowest first. The fragment match is
/// literal so underscores in SQL identifiers cannot act as `LIKE` wildcards.
async fn blocked_query_pids(pool: &PgPool, blocker_pid: i32, query_fragment: &str) -> Vec<i32> {
    sqlx::query_scalar(
        r#"
            SELECT activity.pid
            FROM pg_stat_activity AS activity
            WHERE activity.datname = current_database()
              AND activity.wait_event_type = 'Lock'
              AND $1 = ANY(pg_blocking_pids(activity.pid))
              AND strpos(lower(activity.query), lower($2)) > 0
            ORDER BY activity.pid
        "#,
    )
    .bind(blocker_pid)
    .bind(query_fragment)
    .fetch_all(pool)
    .await
    .expect("database lock state should be readable")
}

/// Waits until at least `expected` sessions are in a lock wait behind
/// `blocker_pid` on a query containing `query_fragment` and returns their
/// backend PIDs, lowest first.
async fn wait_for_blocked_query_pids(
    pool: &PgPool,
    blocker_pid: i32,
    query_fragment: &str,
    expected: usize,
) -> Vec<i32> {
    for _ in 0..300 {
        let blocked = blocked_query_pids(pool, blocker_pid, query_fragment).await;
        if blocked.len() >= expected {
            return blocked;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    panic!(
        "fewer than {expected} queries containing {query_fragment:?} waited for the database lock"
    );
}

/// Waits for a query containing `query_fragment` to enter a lock wait behind
/// `blocker_pid` and returns the blocked PostgreSQL backend PID.
pub async fn wait_for_blocked_query(pool: &PgPool, blocker_pid: i32, query_fragment: &str) -> i32 {
    wait_for_blocked_query_pids(pool, blocker_pid, query_fragment, 1).await[0]
}

/// Waits until at least `expected` sessions are in a lock wait behind
/// `blocker_pid` on a query containing `query_fragment` and returns how many
/// are.
pub async fn wait_for_blocked_queries(
    pool: &PgPool,
    blocker_pid: i32,
    query_fragment: &str,
    expected: usize,
) -> usize {
    wait_for_blocked_query_pids(pool, blocker_pid, query_fragment, expected)
        .await
        .len()
}

/// Sessions currently in a lock wait behind `blocker_pid` on a query
/// containing `query_fragment`.
pub async fn count_blocked_queries(pool: &PgPool, blocker_pid: i32, query_fragment: &str) -> usize {
    blocked_query_pids(pool, blocker_pid, query_fragment)
        .await
        .len()
}
