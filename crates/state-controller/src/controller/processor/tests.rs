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

use tokio::sync::Semaphore;

use super::*;
use crate::state_handler::NoopStateHandler;
use crate::tests::{
    TestObject, TestObjectControllerState, TestStateControllerContextObjects,
    TestStateControllerIO, create_test_object, create_test_state_controller_tables,
};

fn processor(pool: sqlx::PgPool) -> StateProcessor<TestStateControllerIO> {
    StateProcessor {
        pool,
        handler_services: Arc::new(()),
        io: Arc::new(TestStateControllerIO::default()),
        state_handler: Arc::new(NoopStateHandler::default()),
        metric_emitter: None,
        metric_holder: Arc::new(MetricHolder::new(None, "test", Duration::from_secs(300))),
        per_object_state: None,
        object_metrics: HashMap::new(),
        cancel_token: CancellationToken::new(),
        iteration_config: IterationConfig::default(),
        object_tasks: JoinSet::new(),
        completed_objects: HashSet::new(),
        requeue_objects: HashSet::new(),
        last_log_time: Instant::now(),
        last_metric_emission_time: Instant::now(),
        stats_since_last_log: StatsSinceLastLog::default(),
        processor_id: "deadline-test".to_string(),
        state_change_emitter: Arc::new(StateChangeEmitter::default()),
    }
}

#[carbide_macros::sqlx_test]
async fn delayed_claim_commit_is_not_dispatched_and_recovers_after_expiry(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut txn = pool.begin().await?;
    create_test_object("host".to_string(), &mut txn).await;
    db::queue_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
        &["host".to_string()],
    )
    .await?;
    txn.commit().await?;

    // Hold COMMIT on the server, after the claim UPDATE has returned. Dropping
    // the waiting Rust future cannot undo a COMMIT the server later completes.
    sqlx::raw_sql(
        "CREATE FUNCTION delay_claim_commit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.processed_by IS DISTINCT FROM OLD.processed_by THEN
                PERFORM pg_advisory_xact_lock(6778);
            END IF;
            RETURN NEW;
        END;
        $$;
        CREATE CONSTRAINT TRIGGER delay_claim_commit
            AFTER UPDATE ON test_state_controller_queued_objects
            DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
            EXECUTE FUNCTION delay_claim_commit();",
    )
    .execute(&pool)
    .await?;
    let mut blocker = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(6778)")
        .execute(&mut *blocker)
        .await?;

    let mut processor = processor(pool.clone());
    let budget = processor.iteration_config.max_object_handling_time;
    let mut claim = tokio::spawn(async move {
        let result = processor.dequeue_and_dispatch_object_handling_tasks().await;
        (processor, result)
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let commit_waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype = 'advisory'
                    AND objid = 6778 AND NOT granted
                    AND database = (SELECT oid FROM pg_database WHERE datname = current_database()))",
            )
            .fetch_one(&pool)
            .await?;
            if commit_waiting {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    tokio::time::pause();
    tokio::time::advance(budget * 4).await;
    let completion = tokio::time::timeout(Duration::from_secs(1), &mut claim).await;
    tokio::time::resume();
    blocker.commit().await?;
    let (mut processor, result) = completion??;
    assert!(matches!(result, Err(IterationError::QueueClaimTimeout)));
    assert!(processor.object_tasks.is_empty());
    assert_eq!(processor.stats_since_last_log.num_dispatched_tasks, 0);

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let owner: Option<String> = sqlx::query_scalar(
                "SELECT processed_by FROM test_state_controller_queued_objects WHERE object_id = 'host'",
            )
            .fetch_one(&pool)
            .await?;
            if owner.as_deref() == Some(processor.processor_id.as_str()) {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert_eq!(
        processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        0
    );

    // PostgreSQL uses wall time, not Tokio's paused clock. Age the committed
    // reservation explicitly to exercise the existing abandonment predicate.
    sqlx::query(
        "UPDATE test_state_controller_queued_objects
            SET processing_started_at = now() - $1::interval WHERE object_id = 'host'",
    )
    .bind(budget * 4)
    .execute(&pool)
    .await?;
    assert_eq!(
        processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    assert_eq!(processor.object_tasks.len(), 1);
    let result = processor.object_tasks.join_next().await.unwrap()?;
    assert_eq!(result.object_id, "host");
    assert!(result.metrics.common.error.is_none());
    processor.process_object_handling_task_result(result, true, Instant::now());
    processor.finalize_completed_objects().await?;

    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM test_state_controller_queued_objects")
            .fetch_one(&pool)
            .await?;
    let result: sqlx::types::Json<PersistentStateHandlerOutcome> =
        sqlx::query_scalar("SELECT controller_state_outcome FROM test_objects WHERE id = 'host'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(queued, 0);
    assert!(matches!(
        result.0,
        PersistentStateHandlerOutcome::DoNothing { .. }
    ));
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn expired_task_does_not_start_object_io() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://unused/unused")
        .unwrap();
    pool.close().await;
    let mut processor = processor(pool);
    let deadline = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_secs(1)).await;
    processor.dispatch_object_handling_task("host".to_string(), deadline);
    let result = processor.object_tasks.join_next().await.unwrap().unwrap();

    // Pool acquisition would fail immediately with PoolClosed, not Timeout.
    assert!(matches!(
        result.metrics.common.error,
        Some(StateHandlerError::Timeout { .. })
    ));
    assert!(result.metrics.common.initial_state.is_none());
}

#[derive(Debug)]
struct WaitingHandler {
    started: Semaphore,
}

#[async_trait::async_trait]
impl StateHandler for WaitingHandler {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        _controller_state: &TestObjectControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<TestObjectControllerState>, StateHandlerError> {
        self.started.add_permits(1);
        std::future::pending().await
    }
}

#[carbide_macros::sqlx_test]
async fn dispatched_handler_keeps_the_claim_deadline(pool: sqlx::PgPool) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut txn = pool.begin().await?;
    create_test_object("host".to_string(), &mut txn).await;
    txn.commit().await?;
    let mut processor = processor(pool);
    let handler = Arc::new(WaitingHandler {
        started: Semaphore::new(0),
    });
    processor.state_handler = handler.clone();

    // Five seconds remain from the claim's original 180-second budget.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    processor.dispatch_object_handling_task("host".to_string(), deadline);
    tokio::time::timeout(Duration::from_secs(5), handler.started.acquire())
        .await??
        .forget();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(6)).await;
    let result = processor.object_tasks.join_next().await.unwrap()?;
    let completed_at = tokio::time::Instant::now();
    tokio::time::resume();

    assert!(matches!(
        result.metrics.common.error,
        Some(StateHandlerError::Timeout { .. })
    ));
    assert_eq!(
        result.metrics.common.initial_state,
        Some(TestObjectControllerState::A)
    );
    assert!(completed_at < deadline + Duration::from_secs(2));
    Ok(())
}
