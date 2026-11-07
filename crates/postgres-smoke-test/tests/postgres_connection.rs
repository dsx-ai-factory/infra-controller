/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Minimal test: connect to Postgres using DATABASE_URL and run SELECT 1.
 * Use this to verify Postgres connectivity when running via cargo-docker-minimal
 * (e.g. DATABASE_URL is passed into the container and host.docker.internal works).
 *
 * Run: cargo make cargo-docker-minimal -- test -p postgres-smoke-test
 * (Export DATABASE_URL with host.docker.internal as host so the container can reach Postgres.)
 */

use std::str::FromStr;

fn connection_help() -> String {
    "Postgres connection failed. Check: (1) export DATABASE_URL before the command so it is \
     passed into the container; (2) use host.docker.internal as the host (not localhost) so \
     the container can reach Postgres on the host; (3) port matches where Postgres is listening \
     (e.g. 5432 or 30432); (4) Postgres is running and reachable."
        .to_string()
}

#[tokio::test]
async fn test_postgres_connection() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for this test");
    let opts = sqlx::postgres::PgConnectOptions::from_str(&url).expect("invalid DATABASE_URL");

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect_with(opts)
        .await
        .unwrap_or_else(|e| panic!("{} Error: {}", connection_help(), e));

    let row: (i32,) = sqlx::query_as("SELECT 1")
        .fetch_one(&pool)
        .await
        .expect("SELECT 1 failed");

    assert_eq!(row.0, 1);
    pool.close().await;
}
