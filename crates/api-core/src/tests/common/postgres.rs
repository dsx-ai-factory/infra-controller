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

use db::db_read::PgPoolReader;
use sqlx::PgPool;

pub(crate) async fn wait_for_blocked_query(
    pool: &PgPool,
    blocker_pid: i32,
    query_fragment: &str,
) -> i32 {
    db::test_support::postgres::wait_for_blocked_query(
        &mut PgPoolReader::from(pool.clone()),
        blocker_pid,
        query_fragment,
    )
    .await
}

pub(crate) async fn wait_for_blocked_queries(
    pool: &PgPool,
    blocker_pid: i32,
    query_fragment: &str,
    expected: usize,
) -> usize {
    db::test_support::postgres::wait_for_blocked_queries(
        &mut PgPoolReader::from(pool.clone()),
        blocker_pid,
        query_fragment,
        expected,
    )
    .await
}
