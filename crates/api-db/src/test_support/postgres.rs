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

/// `wait_for_blocked_query` is a test-specific helper that waits for a query
/// containing `query_fragment` to enter a lock wait behind `blocker_pid`. It
/// returns the blocked PostgreSQL backend PID. The fragment match is literal so
/// underscores in SQL identifiers cannot act as `LIKE` wildcards.
pub async fn wait_for_blocked_query(pool: &PgPool, blocker_pid: i32, query_fragment: &str) -> i32 {
    for _ in 0..300 {
        let blocked_pid: Option<i32> = sqlx::query_scalar(
            r#"
                SELECT activity.pid
                FROM pg_stat_activity AS activity
                WHERE activity.datname = current_database()
                  AND activity.wait_event_type = 'Lock'
                  AND $1 = ANY(pg_blocking_pids(activity.pid))
                  AND strpos(lower(activity.query), lower($2)) > 0
                ORDER BY activity.pid
                LIMIT 1
            "#,
        )
        .bind(blocker_pid)
        .bind(query_fragment)
        .fetch_optional(pool)
        .await
        .expect("database lock state should be readable");
        if let Some(blocked_pid) = blocked_pid {
            return blocked_pid;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    panic!("query containing {query_fragment:?} never waited for database lock");
}
