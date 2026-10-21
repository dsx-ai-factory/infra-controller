// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use carbide_utils::metrics::SharedMetricsHolder;
use db::resource_pool::remote::cleanup_backlog;
use opentelemetry::metrics::Meter;
use sqlx::PgPool;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::transaction::{DATABASE_WAIT_TIMEOUT, database_transaction};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const FRESH_PERIOD: Duration = Duration::from_secs(30);
const METRIC_NAME: &str = "carbide_resource_pool_cleanup_pending";

/// `start_cleanup_metrics` samples the site's outstanding remote cleanup every
/// five seconds, independently of provider calls. Register it once per database
/// and meter, rather than once per provider coordinator: it counts all saved
/// `CancelRequested` and `ReleaseRequested` allocations. A provider reply alone
/// does not remove work from the count; Core must persist terminal confirmation.
///
/// Each query has a five-second limit and observes `cancel`. No count is emitted
/// before the first successful query. Failed queries preserve the last count,
/// with `fresh=false` once that sample is thirty seconds old. The caller owns
/// the task set and joins the sampler after cancelling it. Returns an error if
/// the sampler task cannot be spawned.
pub fn start_cleanup_metrics(
    pool: PgPool,
    meter: Meter,
    tasks: &mut JoinSet<()>,
    cancel: CancellationToken,
) -> std::io::Result<()> {
    let metrics = SharedMetricsHolder::with_fresh_period(FRESH_PERIOD);
    let observed = metrics.clone();
    meter
        .i64_observable_gauge(METRIC_NAME)
        .with_description("Number of remote allocations awaiting confirmed cancellation or release")
        .with_callback(move |observer| {
            observed.if_available(|count, attrs| observer.observe(*count, attrs));
        })
        .build();

    tasks
        .build_task()
        .name("resource_pool_cleanup_metrics")
        .spawn(async move {
            let mut interval = tokio::time::interval(SAMPLE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while cancel.run_until_cancelled(interval.tick()).await.is_some() {
                let Some(sample) = cancel
                    .run_until_cancelled(tokio::time::timeout(DATABASE_WAIT_TIMEOUT, async {
                        let mut txn = database_transaction(&pool).await?;
                        let count = cleanup_backlog(&mut txn).await?;
                        txn.commit().await?;
                        Ok::<_, db::DatabaseError>(count)
                    }))
                    .await
                else {
                    return;
                };
                match sample {
                    Ok(Ok(count)) => metrics.update(count),
                    Ok(Err(error)) => {
                        tracing::warn!(error = %error, "Could not read remote cleanup backlog");
                    }
                    Err(_) => {
                        tracing::warn!("Timed out reading remote cleanup backlog");
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_utils::test_support::test_meter::TestMeter;
    use chrono::{DateTime, Utc};
    use db::ConditionalWrite;
    use db::resource_pool::{binding, remote};
    use db::test_support::postgres::wait_for_blocked_query;
    use model::resource_pool::allocation::{AllocationState, AssignmentMode, ResourcePoolValue};
    use model::resource_pool::remote::{
        AllocationAssignment, AllocationResult, AllocationSelection, CleanupConfirmation,
        CompletionAuthority, RemoteAllocationParameters,
    };
    use model::resource_pool::{
        ResourcePoolBackendKind, ResourcePoolSelection, ResourcePoolValueDomain,
    };
    use sqlx::types::Json;
    use uuid::Uuid;

    use super::*;

    async fn wait_for_count(meter: &TestMeter, count: i64) {
        let expected = format!("{{fresh=\"true\"}} {count}");
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                if meter.formatted_metric(METRIC_NAME).as_deref() == Some(expected.as_str()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("cleanup sampler should publish the persisted count");
    }

    #[carbide_macros::sqlx_test]
    async fn sampler_counts_cleanup_across_authorities_until_terminal_confirmation(
        pool: PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut cleanup = Vec::new();
        let site_id = Uuid::new_v4();
        for (name, authority, states) in [
            (
                "cleanup-a",
                "authority-a",
                [AllocationState::Pending, AllocationState::CancelRequested],
            ),
            (
                "cleanup-b",
                "authority-b",
                [AllocationState::Attached, AllocationState::ReleaseRequested],
            ),
        ] {
            let mut txn = pool.begin().await?;
            let binding = binding::bind_or_verify(
                &mut txn,
                &ResourcePoolSelection {
                    pool_name: name.into(),
                    value_domain: ResourcePoolValueDomain::Integer,
                    backend_name: Some(authority.into()),
                    backend_kind: ResourcePoolBackendKind::Grpc,
                    authority_id: authority.into(),
                    remote_pool: "vnis".into(),
                },
            )
            .await?;
            let parameters = RemoteAllocationParameters {
                site_id,
                authority_id: authority.into(),
                pool_id: "vnis".into(),
                slot: "vni".into(),
                generation: 0,
                eligibility_revision: "initial".into(),
                selection: AllocationSelection::Automatic,
                completion_authority: CompletionAuthority::Controller {
                    desire_id: Uuid::new_v4(),
                },
            };
            for (offset, state) in states.into_iter().enumerate() {
                let request_id = Uuid::new_v4();
                let result = AllocationResult::Allocated(AllocationAssignment {
                    allocation_id: Uuid::new_v4().to_string(),
                    value: ResourcePoolValue::Integer(100 + offset as i64),
                    assignment_mode: AssignmentMode::Automatic,
                    route_target_policy: None,
                });
                sqlx::query(
                    "INSERT INTO resource_pool_allocations
                        (allocation_id, binding_id, pool_name, owner_type, owner_id,
                         state, remote_request, result)
                     VALUES ($1, $2, $3, 'vpc', $4, $5, $6, $7)",
                )
                .bind(request_id)
                .bind(binding.binding_id)
                .bind(name)
                .bind(Uuid::new_v4().to_string())
                .bind(state)
                .bind(Json(&parameters))
                .bind(Json(result))
                .execute(&mut *txn)
                .await?;
                match state {
                    AllocationState::CancelRequested => {
                        cleanup.push((request_id, CleanupConfirmation::Cancelled));
                    }
                    AllocationState::ReleaseRequested => {
                        cleanup.push((request_id, CleanupConfirmation::Released));
                    }
                    _ => {}
                }
            }
            txn.commit().await?;
        }

        // Block the first sample so an invented initial zero cannot pass unnoticed.
        let mut blocker = pool.begin().await?;
        sqlx::query("LOCK TABLE resource_pool_allocations IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await?;
        let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let meter = TestMeter::default();
        let mut tasks = JoinSet::new();
        let cancel = CancellationToken::new();
        start_cleanup_metrics(pool.clone(), meter.meter(), &mut tasks, cancel.clone())?;
        wait_for_blocked_query(
            &pool,
            blocker_pid,
            "SELECT count(*) FROM resource_pool_allocations",
        )
        .await;
        assert_eq!(meter.formatted_metric(METRIC_NAME), None);
        blocker.commit().await?;

        wait_for_count(&meter, 2).await;
        assert!(
            meter
                .export_metrics()
                .contains(&format!("# TYPE {METRIC_NAME} gauge"))
        );

        let mut blocker = pool.begin().await?;
        sqlx::query("LOCK TABLE resource_pool_allocations IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await?;
        let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        let blocked_pid = tokio::time::timeout(
            Duration::from_secs(12),
            wait_for_blocked_query(
                &pool,
                blocker_pid,
                "SELECT count(*) FROM resource_pool_allocations",
            ),
        )
        .await?;
        let query_started: DateTime<Utc> =
            sqlx::query_scalar("SELECT query_start FROM pg_stat_activity WHERE pid = $1")
                .bind(blocked_pid)
                .fetch_one(&pool)
                .await?;
        // Keep the lock until the timed-out statement has stopped and the
        // sampler has processed its failure by starting another attempt.
        tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let still_running: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity
                        WHERE pid = $1 AND query_start = $2 AND state = 'active')",
                )
                .bind(blocked_pid)
                .bind(query_started)
                .fetch_one(&pool)
                .await?;
                if !still_running {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            wait_for_blocked_query(
                &pool,
                blocker_pid,
                "SELECT count(*) FROM resource_pool_allocations",
            )
            .await;
            Ok::<_, sqlx::Error>(())
        })
        .await??;
        assert_eq!(
            meter.formatted_metric(METRIC_NAME).as_deref(),
            Some("{fresh=\"true\"} 2"),
            "a failed sample must preserve the last successful count"
        );

        // `SharedMetricsHolder` uses `std::time::Instant`, so advancing Tokio's
        // clock cannot age the sample. Keep the queries failing for another
        // freshness period before checking the retained count.
        tokio::time::sleep(FRESH_PERIOD).await;
        assert_eq!(
            meter.formatted_metric(METRIC_NAME).as_deref(),
            Some("{fresh=\"false\"} 2"),
            "failed samples must preserve the count after it becomes stale"
        );
        blocker.rollback().await?;

        let mut txn = pool.begin().await?;
        for (request_id, confirmation) in cleanup {
            assert_eq!(
                remote::confirm_cleanup(&mut txn, request_id, confirmation).await?,
                ConditionalWrite::Applied(())
            );
        }
        txn.commit().await?;
        // Recovery must replace the stale series with one fresh count.
        wait_for_count(&meter, 0).await;

        let mut blocker = pool.begin().await?;
        sqlx::query("LOCK TABLE resource_pool_allocations IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await?;
        let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *blocker)
            .await?;
        tokio::time::timeout(
            Duration::from_secs(12),
            wait_for_blocked_query(
                &pool,
                blocker_pid,
                "SELECT count(*) FROM resource_pool_allocations",
            ),
        )
        .await?;
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(result) = tasks.join_next().await {
                result.expect("cleanup sampler should stop without panicking");
            }
        })
        .await?;
        blocker.rollback().await?;
        Ok(())
    }
}
