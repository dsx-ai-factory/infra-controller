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

use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
use db::{TransactionVending as _, WithTransaction as _};
use sqlx::PgPool;

use super::create_test_env;

async fn disconnect_transaction(pool: &PgPool, txn: &mut db::Transaction<'_>) {
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(txn.as_mut())
        .await
        .unwrap();
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1, 5000)")
        .bind(pid)
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(
        terminated,
        "the test transaction's backend must exit before rollback"
    );
}

#[crate::sqlx_test]
async fn transaction_rollback_failure_uses_owner_instrumentation(pool: PgPool) {
    const COUNTER: &str = "carbide_database_transaction_rollback_failures_total";
    let env = create_test_env(pool.clone()).await;
    let metrics = MetricsCapture::for_registry(env.test_meter.registry());

    let mut txn = pool.txn_begin().await.unwrap();
    disconnect_transaction(&pool, &mut txn).await;
    let ((), logs) = capture_logs_async(txn.rollback_or_log("pool cleanup")).await;
    assert!(
        logs.iter()
            .all(|log| log.metadata_name != "database_transaction_rollback_failed")
    );

    let control_pool = pool.clone();
    let (result, logs) = capture_logs_async(pool.with_txn(|txn| {
        Box::pin(async move {
            disconnect_transaction(&control_pool, txn).await;
            Err::<(), _>("pool closure error")
        })
    }))
    .await;
    assert_eq!(result.unwrap(), Err("pool closure error"));
    assert!(
        logs.iter()
            .all(|log| log.metadata_name != "database_transaction_rollback_failed")
    );

    let mut txn = env.api.txn_begin().await.unwrap();
    disconnect_transaction(&pool, &mut txn).await;
    txn.rollback_or_log("API cleanup").await;
    assert_eq!(
        metrics.counter_delta(COUNTER, &[("trigger", "explicit_cleanup")]),
        1.0
    );

    let result = env
        .api
        .with_txn(|txn| {
            Box::pin(async move {
                disconnect_transaction(&pool, txn).await;
                Err::<(), _>("original closure error")
            })
        })
        .await
        .unwrap();
    assert_eq!(result, Err("original closure error"));
    assert_eq!(
        metrics.counter_delta(COUNTER, &[("trigger", "closure_error")]),
        1.0
    );
}
