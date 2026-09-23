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

//! Keeps [`metrics_endpoint::HealthController`] (and therefore `/ready`) in sync
//! with whether PostgreSQL is actually reachable. Without this, `/ready` only
//! ever reflects process liveness, so Kubernetes keeps routing traffic to a
//! replica whose database-backed requests are failing.

use std::time::Duration;

use carbide_instrument::emit;
use metrics_endpoint::HealthController;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// The dedicated probe pool's single connection answered read-only. Returned
/// from `after_connect`/`before_acquire` so sqlx discards the connection
/// instead of pooling it.
#[derive(Debug, thiserror::Error)]
#[error("readiness probe requires a writable database connection")]
struct ReadOnlyReadinessConnection;

/// How often the readiness probe re-checks PostgreSQL connectivity.
const READINESS_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How long a single readiness check waits for PostgreSQL to answer before it
/// counts as a failure. Short relative to `database_pool_acquire_timeout` so
/// `/ready` reflects a degraded pool well before request-serving code paths
/// start timing out.
const READINESS_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// A periodic PostgreSQL readiness check failed. Alertable: repeated
/// occurrences mean `/ready` is reporting the replica unready and Kubernetes
/// should be pulling it out of Service endpoints.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "database_readiness_check_failed",
    metric_name = "carbide_database_readiness_check_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "database readiness check failed",
    describe = "Number of periodic PostgreSQL readiness checks that failed, backing /ready"
)]
struct DatabaseReadinessCheckFailed {
    #[context]
    error: String,
}

/// Spawn the periodic PostgreSQL readiness check that keeps `health_controller`
/// in sync with the database's actual availability. Runs until `cancel_token`
/// is cancelled.
pub(crate) fn spawn_database_readiness_probe(
    join_set: &mut JoinSet<()>,
    db_pool: &PgPool,
    health_controller: HealthController,
    cancel_token: CancellationToken,
) -> eyre::Result<()> {
    join_set
        .build_task()
        .name("database_readiness_probe")
        .spawn(run_database_readiness_probe(
            dedicated_probe_pool(db_pool),
            health_controller,
            cancel_token,
            READINESS_CHECK_INTERVAL,
        ))?;
    Ok(())
}

/// A single dedicated connection for the readiness probe, separate from the
/// application's shared `db_pool`. A busy shared pool cannot delay or fail
/// this check, and because there is only ever one probe connection, the
/// check is a deliberate "is my endpoint writable" test rather than whichever
/// connection the shared pool happens to hand back -- important right after a
/// failover, when the shared pool can still hold connections to the demoted
/// node alongside fresh ones to the new primary.
///
/// `after_connect`/`before_acquire` reject a connection that answers
/// read-only, so sqlx discards it immediately instead of pooling it until
/// `max_lifetime`. Without this, a probe connection opened against a standby
/// stays in this single-connection pool -- healthy from sqlx's perspective --
/// for up to `max_lifetime` after routing is fixed, holding `/ready` not-ready
/// long after the endpoint recovers. Mirrors `create_work_lock_pool`
/// (`crates/api-db/src/work_lock_manager.rs`). Lazy: does not touch the
/// network until the first check runs.
fn dedicated_probe_pool(db_pool: &PgPool) -> PgPool {
    let options = db_pool.options();
    PgPoolOptions::new()
        .min_connections(1)
        .max_connections(1)
        .acquire_timeout(options.get_acquire_timeout())
        .idle_timeout(options.get_idle_timeout())
        .max_lifetime(options.get_max_lifetime())
        // The writability query below also proves the connection is responsive.
        .test_before_acquire(false)
        .after_connect(|db, _metadata| {
            Box::pin(async move { ensure_readiness_connection_is_writable(db).await })
        })
        .before_acquire(|db, _metadata| {
            Box::pin(async move {
                match ensure_readiness_connection_is_writable(db).await {
                    Ok(()) => Ok(true),
                    Err(_) => Ok(false),
                }
            })
        })
        .connect_lazy_with(db_pool.connect_options().as_ref().clone())
}

/// Rejects a read-only connection so the pool that owns it discards it.
/// Mirrors `ensure_work_lock_connection_is_writable`
/// (`crates/api-db/src/work_lock_manager.rs`).
async fn ensure_readiness_connection_is_writable(db: &mut PgConnection) -> sqlx::Result<()> {
    let read_only: bool =
        sqlx::query_scalar("SELECT current_setting('transaction_read_only')::bool")
            .fetch_one(db)
            .await?;

    if read_only {
        return Err(sqlx::Error::Configuration(Box::new(
            ReadOnlyReadinessConnection,
        )));
    }

    Ok(())
}

async fn run_database_readiness_probe(
    db_pool: PgPool,
    health_controller: HealthController,
    cancel_token: CancellationToken,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = cancel_token.cancelled() => return,
            _ = ticker.tick() => {}
        }

        let ready = check_database_readiness(&db_pool, READINESS_CHECK_TIMEOUT).await;
        if ready != health_controller.is_ready() {
            if ready {
                tracing::warn!("database readiness check recovered; marking Core ready");
            } else {
                tracing::warn!("database readiness check failing; marking Core not ready");
            }
        }
        health_controller.set_ready(ready);
    }
}

/// One PostgreSQL readiness check, bounded by `timeout`. `db_pool`'s
/// `before_acquire`/`after_connect` hooks (see `dedicated_probe_pool`) reject
/// a read-only connection before this query ever runs, so a
/// reachable-but-read-only endpoint -- e.g. the configured endpoint routing to
/// a standby after a failed promotion -- surfaces here as an acquire error,
/// same as an unreachable one, rather than a successful `SELECT`.
async fn check_database_readiness(db_pool: &PgPool, timeout: Duration) -> bool {
    let check = sqlx::query("SELECT 1").execute(db_pool);
    match tokio::time::timeout(timeout, check).await {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => {
            emit(DatabaseReadinessCheckFailed {
                error: error.to_string(),
            });
            false
        }
        Err(_elapsed) => {
            emit(DatabaseReadinessCheckFailed {
                error: "timed out".to_string(),
            });
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::time::{Duration, Instant};

    use sqlx::postgres::PgPoolOptions;
    use tokio_util::sync::CancellationToken;

    use super::*;

    /// A pool connected to the real database under test, via the same
    /// `DATABASE_URL` convention used by `work_lock_manager`'s
    /// `test_read_only_connection_is_replaced`.
    async fn connected_pool() -> PgPool {
        PgPoolOptions::new()
            .acquire_timeout(Duration::from_secs(1))
            .connect(&env::var("DATABASE_URL").unwrap())
            .await
            .unwrap()
    }

    /// A pool pointed at an unreachable address, with a short `acquire_timeout`
    /// so queries against it fail promptly. `connect_lazy` builds the pool
    /// without touching the network, matching the idiom used elsewhere in this
    /// repo (see `crates/machine-controller/src/handler/host_boot_config.rs`)
    /// to exercise a "database unavailable" path without a real outage.
    fn unreachable_pool() -> PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
            .expect("connect_lazy does not touch the network")
    }

    #[tokio::test]
    async fn check_database_readiness_fails_against_unreachable_pool() {
        let pool = unreachable_pool();

        assert!(!check_database_readiness(&pool, Duration::from_millis(200)).await);
    }

    /// Drives the probe loop -- not just the single check -- against an
    /// unreachable pool and asserts it flips a ready-by-default
    /// `HealthController` to not-ready within a bounded wait. This is the
    /// wiring `/ready` actually depends on: a correct `check_database_readiness`
    /// is not enough if the loop never calls `set_ready`.
    #[tokio::test]
    async fn probe_marks_controller_not_ready_on_persistent_failure() {
        let pool = unreachable_pool();
        let health_controller = HealthController::new();
        assert!(health_controller.is_ready(), "starts ready by default");
        let cancel_token = CancellationToken::new();

        let probe = tokio::spawn(run_database_readiness_probe(
            pool,
            health_controller.clone(),
            cancel_token.clone(),
            Duration::from_millis(20),
        ));

        let deadline = Instant::now() + Duration::from_secs(5);
        while health_controller.is_ready() {
            assert!(
                Instant::now() < deadline,
                "controller did not flip to not-ready within the deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        cancel_token.cancel();
        probe.await.expect("probe task does not panic");
    }

    /// Drives the probe loop against a real, reachable database and asserts
    /// it flips a not-ready `HealthController` to ready. The two tests above
    /// only exercise the failure direction; without this, a probe that never
    /// calls `set_ready(true)` again after an initial failure would still
    /// pass every other test in this module.
    #[tokio::test]
    async fn probe_marks_controller_ready_on_recovery() {
        let pool = dedicated_probe_pool(&connected_pool().await);
        let health_controller = HealthController::new();
        health_controller.set_ready(false);
        assert!(!health_controller.is_ready());
        let cancel_token = CancellationToken::new();

        let probe = tokio::spawn(run_database_readiness_probe(
            pool,
            health_controller.clone(),
            cancel_token.clone(),
            Duration::from_millis(20),
        ));

        let deadline = Instant::now() + Duration::from_secs(5);
        while !health_controller.is_ready() {
            assert!(
                Instant::now() < deadline,
                "controller did not flip to ready within the deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        cancel_token.cancel();
        probe.await.expect("probe task does not panic");
    }

    /// A connection that turns read-only after being handed out (e.g. its
    /// endpoint now routes to a standby) is discarded on release rather than
    /// pooled until `max_lifetime`, so the next acquire opens a fresh,
    /// writable connection. Mirrors
    /// `work_lock_manager::tests::test_read_only_connection_is_replaced`.
    #[tokio::test]
    async fn dedicated_probe_pool_replaces_read_only_connection() {
        let probe_pool = dedicated_probe_pool(&connected_pool().await);

        let mut db = probe_pool.acquire().await.unwrap();
        let original_backend_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *db)
            .await
            .unwrap();
        sqlx::query("SET default_transaction_read_only = on")
            .execute(&mut *db)
            .await
            .unwrap();
        drop(db);

        let mut db = probe_pool.acquire().await.expect(
            "sqlx should notice the read-only connection and close it, allowing a reconnect",
        );
        let (replacement_backend_pid, read_only): (i32, bool) = sqlx::query_as(
            "SELECT pg_backend_pid(), current_setting('transaction_read_only')::bool",
        )
        .fetch_one(&mut *db)
        .await
        .expect("read-only connection should be replaced");

        assert_ne!(replacement_backend_pid, original_backend_pid);
        assert!(!read_only);
    }
}
