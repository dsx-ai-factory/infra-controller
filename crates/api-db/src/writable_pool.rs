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
use sqlx::PgConnection;
use sqlx::postgres::PgPoolOptions;

#[derive(Debug, thiserror::Error)]
#[error("database connection is read-only")]
struct ReadOnlyConnection;

/// Build pool options that reject read-only connections on opening and before reuse.
///
/// The writability query replaces SQLx's liveness ping. Rejected connections are closed
/// and acquisition retries within the pool's acquire timeout. Other pool settings retain
/// SQLx's defaults and can be configured on the returned options.
pub fn writable_pool_options() -> PgPoolOptions {
    PgPoolOptions::new()
        // A demoted primary still answers pings; check writability on every checkout.
        .test_before_acquire(false)
        .after_connect(|db, _metadata| {
            Box::pin(async move { ensure_connection_is_writable(db).await })
        })
        .before_acquire(|db, _metadata| {
            Box::pin(async move { ensure_connection_is_writable(db).await.map(|()| true) })
        })
}

async fn ensure_connection_is_writable(db: &mut PgConnection) -> sqlx::Result<()> {
    // transaction_read_only also detects read-only sessions on a primary.
    let read_only = sqlx::query_scalar("SELECT current_setting('transaction_read_only')::bool")
        .fetch_one(db)
        .await?;

    if read_only {
        return Err(sqlx::Error::Configuration(Box::new(ReadOnlyConnection)));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn writable_pool_reuses_writable_connections_and_replaces_read_only_connections() {
        let pool = writable_pool_options()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(&std::env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();
        let original_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&pool)
            .await
            .unwrap();
        let mut connection = pool.acquire().await.unwrap();
        let reused_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(
            original_pid, reused_pid,
            "healthy connections must be reused"
        );

        // Simulate an idle connection becoming read-only after a primary is demoted.
        sqlx::query("SET default_transaction_read_only = on")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);

        let mut connection = pool.acquire().await.unwrap();
        let replacement_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_ne!(original_pid, replacement_pid);
        sqlx::query("CREATE TEMP TABLE writable_pool_probe (id integer)")
            .execute(&mut *connection)
            .await
            .expect("replacement connection must support writes");
    }

    #[tokio::test]
    async fn writable_pool_rejects_new_read_only_connections() {
        let options = std::env::var("DATABASE_URL")
            .unwrap()
            .parse::<sqlx::postgres::PgConnectOptions>()
            .unwrap()
            .options([("default_transaction_read_only", "on")]);
        let error = writable_pool_options()
            .acquire_timeout(Duration::from_millis(250))
            .connect_with(options)
            .await
            .expect_err("a read-only endpoint must never yield a usable connection");
        assert!(matches!(error, sqlx::Error::PoolTimedOut));
    }
}
