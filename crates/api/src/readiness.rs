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
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

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
/// node alongside fresh ones to the new primary. Mirrors
/// `create_work_lock_pool` (`crates/api-db/src/work_lock_manager.rs`). Lazy:
/// does not touch the network until the first check runs.
fn dedicated_probe_pool(db_pool: &PgPool) -> PgPool {
    let options = db_pool.options();
    PgPoolOptions::new()
        .min_connections(1)
        .max_connections(1)
        .acquire_timeout(options.get_acquire_timeout())
        .idle_timeout(options.get_idle_timeout())
        .max_lifetime(options.get_max_lifetime())
        .connect_lazy_with(db_pool.connect_options().as_ref().clone())
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

/// One PostgreSQL readiness check, bounded by `timeout`: the connection must
/// answer, and must not be read-only. A reachable-but-read-only connection
/// happens when the configured endpoint routes to a standby -- e.g. a failed
/// promotion or stale primary routing -- and `SELECT 1` alone would not catch
/// it, even though Core's write paths fail against it. Mirrors the same
/// `transaction_read_only` check `ensure_work_lock_connection_is_writable`
/// (`crates/api-db/src/work_lock_manager.rs`) uses to reject read-only pool
/// connections.
async fn check_database_readiness(db_pool: &PgPool, timeout: Duration) -> bool {
    let check =
        sqlx::query_scalar::<_, bool>("SELECT current_setting('transaction_read_only')::bool")
            .fetch_one(db_pool);
    match tokio::time::timeout(timeout, check).await {
        Ok(Ok(read_only)) => {
            if read_only {
                emit(DatabaseReadinessCheckFailed {
                    error: "connection is read-only".to_string(),
                });
            }
            !read_only
        }
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
    use std::time::{Duration, Instant};

    use tokio_util::sync::CancellationToken;

    use super::*;

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
}
