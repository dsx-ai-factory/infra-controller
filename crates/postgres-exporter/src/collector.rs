// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Read-only database health and bounded pg_stat_statements snapshots.

use std::time::Duration;

use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tokio_util::sync::CancellationToken;

use crate::metrics::{Metrics, Statement};

pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const INTERVAL: Duration = Duration::from_secs(30);

const SLOW_QUERIES: &str = "SELECT
    dbid::text || ':' || userid::text || ':' || queryid::text || ':' || toplevel::text AS statement_id,
    query, calls, total_exec_time / 1000 AS sec_total, mean_exec_time AS ms_avg
    FROM pg_stat_statements
    ORDER BY total_exec_time DESC, dbid, userid, queryid, toplevel
    LIMIT 20";

pub(super) async fn run(pool: &PgPool, metrics: &Metrics, cancel_token: CancellationToken) {
    let mut ticks = interval(INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while cancel_token
        .run_until_cancelled(ticks.tick())
        .await
        .is_some()
    {
        collect(pool, metrics, &cancel_token).await;
        collect_statements(pool, metrics, &cancel_token).await;
    }
}

async fn collect_statements(pool: &PgPool, metrics: &Metrics, cancel_token: &CancellationToken) {
    let Some(result) = cancel_token
        .run_until_cancelled(timeout(
            PROBE_TIMEOUT,
            sqlx::query_as::<_, Statement>(SLOW_QUERIES).fetch_all(pool),
        ))
        .await
    else {
        return;
    };
    match result {
        Ok(Ok(statements)) => metrics.record_statements(Some(statements)),
        Ok(Err(_)) => {
            metrics.record_statements(None);
            tracing::warn!(
                collector = "slow_queries",
                "PostgreSQL statement collection failed; check pg_stat_statements and role permissions"
            );
        }
        Err(_) => {
            metrics.record_statements(None);
            tracing::warn!(
                collector = "slow_queries",
                "PostgreSQL statement collection timed out"
            );
        }
    }
}

async fn collect(pool: &PgPool, metrics: &Metrics, cancel_token: &CancellationToken) {
    // The deadline includes pool acquisition, connection setup, and the SQL query.
    let Some(result) = cancel_token
        .run_until_cancelled(timeout(
            PROBE_TIMEOUT,
            sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(pool),
        ))
        .await
    else {
        return;
    };
    let success = matches!(result, Ok(Ok(1)));
    metrics.record(success);
    match result {
        Ok(Ok(1)) => {}
        // Deliberately omit database error text: it can include caller-supplied connection details.
        Ok(Err(_)) => tracing::warn!(collector = "hello_world", "PostgreSQL probe failed"),
        Err(_) => tracing::warn!(collector = "hello_world", "PostgreSQL probe timed out"),
        Ok(Ok(_)) => tracing::warn!(
            collector = "hello_world",
            "PostgreSQL probe returned an unexpected value"
        ),
    }
}

#[cfg(test)]
mod tests {
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

    use super::*;

    #[tokio::test]
    async fn statement_snapshot_order_limit_and_recovery() {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL is required"))
            .await
            .unwrap();
        // A session-local fixture exercises the shipped SQL without resetting shared statistics.
        sqlx::query(
            "CREATE TEMP TABLE pg_stat_statements (
            dbid oid, userid oid, queryid bigint, toplevel boolean,
            query text, calls bigint, total_exec_time double precision,
            mean_exec_time double precision)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO pg_stat_statements
            SELECT 1, n, 42, true, 'SELECT $1', n, n * 1000.0, 1000.0
            FROM generate_series(1, 25) AS n",
        )
        .execute(&pool)
        .await
        .unwrap();
        let setup =
            metrics_endpoint::new_metrics_setup("postgres-exporter", "test", false).unwrap();
        let metrics = Metrics::new(&setup.meter);
        let cancel = CancellationToken::new();
        collect_statements(&pool, &metrics, &cancel).await;
        let statements = sqlx::query_as::<_, Statement>(SLOW_QUERIES)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(statements.len(), 20);
        assert_eq!(statements.first().unwrap().calls, 25);
        assert_eq!(statements.last().unwrap().calls, 6);
        for (name, expected) in [
            ("nico_postgres_statement_calls", 25.0),
            ("nico_postgres_statement_exec_seconds", 25.0),
            ("nico_postgres_statement_mean_exec_milliseconds", 1000.0),
        ] {
            let family = setup
                .registry
                .gather()
                .into_iter()
                .find(|f| f.name() == name)
                .unwrap();
            // Identical SQL text from different roles must remain distinct series.
            assert_eq!(family.get_metric().len(), 20);
            let row = family
                .get_metric()
                .iter()
                .find(|m| {
                    m.get_label()
                        .iter()
                        .any(|l| l.name() == "statement_id" && l.value() == "1:25:42:true")
                })
                .unwrap();
            assert_eq!(row.get_gauge().value(), expected);
            assert!(
                row.get_label()
                    .iter()
                    .any(|l| l.name() == "query" && l.value() == "SELECT $1")
            );
        }
        // Force a SQL failure, then repair it and verify a fresh, smaller snapshot.
        sqlx::query("ALTER TABLE pg_stat_statements RENAME COLUMN calls TO broken_calls")
            .execute(&pool)
            .await
            .unwrap();
        collect_statements(&pool, &metrics, &cancel).await;
        assert!(
            !setup
                .registry
                .gather()
                .iter()
                .any(|f| f.name() == "nico_postgres_statement_calls")
        );
        let health = setup
            .registry
            .gather()
            .into_iter()
            .find(|f| f.name() == "nico_postgres_exporter_stat_statements_up")
            .unwrap();
        assert_eq!(health.get_metric()[0].get_gauge().value(), 0.0);
        sqlx::query("ALTER TABLE pg_stat_statements RENAME COLUMN broken_calls TO calls")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM pg_stat_statements WHERE userid != 25")
            .execute(&pool)
            .await
            .unwrap();
        collect_statements(&pool, &metrics, &cancel).await;
        let families = setup.registry.gather();
        let calls = families
            .iter()
            .find(|f| f.name() == "nico_postgres_statement_calls")
            .unwrap();
        assert_eq!(calls.get_metric().len(), 1);
        let health = families
            .iter()
            .find(|f| f.name() == "nico_postgres_exporter_stat_statements_up")
            .unwrap();
        assert_eq!(health.get_metric()[0].get_gauge().value(), 1.0);
        sqlx::query("TRUNCATE pg_stat_statements")
            .execute(&pool)
            .await
            .unwrap();
        collect_statements(&pool, &metrics, &cancel).await;
        let families = setup.registry.gather();
        assert!(
            !families
                .iter()
                .any(|f| f.name() == "nico_postgres_statement_calls")
        );
        let health = families
            .iter()
            .find(|f| f.name() == "nico_postgres_exporter_stat_statements_up")
            .unwrap();
        assert_eq!(health.get_metric()[0].get_gauge().value(), 1.0);
        pool.close().await;
    }

    /// Uses ordinary PostgreSQL, with no operator, migrations, or extensions.
    #[tokio::test]
    async fn database_failure_timeout_and_recovery() {
        let options: PgConnectOptions = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL is required for the PostgreSQL collector test")
            .parse()
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options.clone())
            .await
            .unwrap();
        let setup =
            metrics_endpoint::new_metrics_setup("postgres-exporter", "test", false).unwrap();
        let metrics = Metrics::new(&setup.meter);
        let value = |name: &str| {
            setup
                .registry
                .gather()
                .into_iter()
                .find(|family| family.name() == name)
                .unwrap()
                .get_metric()[0]
                .get_gauge()
                .value()
        };
        let up = "nico_postgres_exporter_database_up";
        let timestamp = "nico_postgres_exporter_last_success_timestamp_seconds";
        collect(&pool, &metrics, &CancellationToken::new()).await;
        assert_eq!(value(up), 1.0);
        let last_success = value(timestamp);
        assert!(last_success > 0.0);

        // Inject a real connection failure and remove the existing healthy connection.
        pool.set_connect_options(options.clone().host("127.0.0.1").port(1));
        pool.acquire().await.unwrap().close().await.unwrap();
        collect(&pool, &metrics, &CancellationToken::new()).await;
        assert_eq!(value(up), 0.0);
        assert_eq!(value(timestamp), last_success);
        pool.set_connect_options(options);
        collect(&pool, &metrics, &CancellationToken::new()).await;
        assert_eq!(value(up), 1.0);

        // Hold the only connection: the outer deadline must also bound pool acquisition.
        let held = pool.acquire().await.unwrap();
        let started = tokio::time::Instant::now();
        collect(&pool, &metrics, &CancellationToken::new()).await;
        assert!(started.elapsed() >= PROBE_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(8));
        assert_eq!(value(up), 0.0);
        drop(held);
        collect(&pool, &metrics, &CancellationToken::new()).await;
        assert_eq!(value(up), 1.0);
        pool.close().await;
    }
}
