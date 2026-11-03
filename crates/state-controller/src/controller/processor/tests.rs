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

use std::sync::atomic::Ordering;

use carbide_utils::test_support::test_meter::TestMeter;
use config_version::ConfigVersion;
use tokio::sync::{Semaphore, mpsc, oneshot};

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
        object_ids_from_database: None,
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

#[tokio::test(start_paused = true)]
async fn completion_poll_records_outstanding_and_completed_task_counts() {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://unused/unused")
        .unwrap();
    pool.close().await;
    let mut processor = processor(pool);
    let meter = TestMeter::default();
    processor.metric_emitter = Some(ProcessorMetricsEmitter::new("test_objects", &meter.meter()));

    let (finished_tx, mut finished_rx) = mpsc::unbounded_channel();
    let mut releases = Vec::new();
    for index in 0..3 {
        let (release_tx, release_rx) = oneshot::channel();
        releases.push(release_tx);
        let finished_tx = finished_tx.clone();
        processor.object_tasks.spawn(async move {
            release_rx.await.unwrap();
            finished_tx.send(()).unwrap();
            ObjectHandlingTaskResult {
                object_id: index.to_string(),
                metrics: ObjectHandlerMetrics::default(),
            }
        });
    }

    // Release tasks explicitly so polling timeouts and batched completions do
    // not depend on wall-clock timing. The completion signal is sent immediately
    // before returning, with no intervening await on this single-thread runtime.
    for (name, release_count, expected_running, expected_completed) in [
        ("poll timeout preserves outstanding tasks", 0, 3, None),
        ("batch completion reduces gauge", 2, 1, Some(2)),
        ("another timeout preserves counts", 0, 1, Some(2)),
        ("final completion clears gauge", 1, 0, Some(3)),
        ("empty poll does not recount completions", 0, 0, Some(3)),
    ] {
        for _ in 0..release_count {
            releases.pop().unwrap().send(()).unwrap();
        }
        for _ in 0..release_count {
            finished_rx.recv().await.unwrap();
        }

        assert_eq!(
            processor
                .wait_and_process_object_handling_task_completions(Duration::from_secs(2), true)
                .await,
            release_count,
            "{name}",
        );
        assert_eq!(processor.object_tasks.len(), expected_running, "{name}");
        assert_eq!(
            meter.formatted_metric("test_objects_object_tasks_running"),
            Some(expected_running.to_string()),
            "{name}",
        );
        assert_eq!(
            meter.formatted_metric("test_objects_object_tasks_completed_total"),
            expected_completed.map(|count| count.to_string()),
            "{name}",
        );
        assert_eq!(
            meter.formatted_metric("test_objects_object_tasks_errored_total"),
            None,
            "{name}",
        );
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

/// Proves that renaming one persisted object must not make its old and new IDs
/// count as two objects in the retained aggregate metrics.
#[carbide_macros::sqlx_test]
async fn renamed_object_keeps_single_aggregate_count(pool: sqlx::PgPool) -> eyre::Result<()> {
    const ORIGINAL_ID: &str = "predicted-host";
    const RENAMED_ID: &str = "stable-host";

    // Setup: create one object and a controller whose aggregate metrics are
    // filtered by the replica-local database object-ID list.
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        ::db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
    let mut txn = pool.begin().await?;
    create_test_object(ORIGINAL_ID.to_string(), &mut txn).await;
    txn.commit().await?;

    let meter = TestMeter::default();
    let mut controller = crate::controller::StateController::<TestStateControllerIO>::builder()
        .database(pool.clone(), work_lock_manager_handle)
        .meter("test_objects", meter.meter())
        .filter_metrics_by_existing_object_ids()
        .processor_id("identity-change-test".to_string())
        .services(Arc::new(()))
        .build_for_manual_iterations(CancellationToken::new())?;

    // Action: publish the original database ID list, queue A, and process it
    // through the production task-result path.
    let enqueuer_result = controller.enqueuer.run_single_iteration().await;
    assert!(!enqueuer_result.skipped_iteration);
    assert_eq!(
        controller
            .processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let result = controller
        .processor
        .object_tasks
        .join_next()
        .await
        .unwrap()?;
    controller
        .processor
        .process_object_handling_task_result(result, false, Instant::now());
    controller.processor.finalize_completed_objects().await?;
    controller.processor.emit_metrics();

    // Assertion: the first completed identity establishes one aggregate object.
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );

    // Action: rename the same persisted object and give B a distinguishable
    // state before processing it under the still-current database list containing A.
    let mut txn = pool.begin().await?;
    sqlx::query("UPDATE test_objects SET id=$1, controller_state=$2::json WHERE id=$3")
        .bind(RENAMED_ID)
        .bind(sqlx::types::Json(TestObjectControllerState::B))
        .bind(ORIGINAL_ID)
        .execute(&mut *txn)
        .await?;
    db::queue_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
        &[RENAMED_ID.to_string()],
    )
    .await?;
    txn.commit().await?;
    assert_eq!(
        controller
            .processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let result = controller
        .processor
        .object_tasks
        .join_next()
        .await
        .unwrap()?;
    controller
        .processor
        .process_object_handling_task_result(result, false, Instant::now());
    controller.processor.finalize_completed_objects().await?;
    controller.processor.emit_metrics();

    // Assertion: pending B is retained, but only A's state contributes while
    // the latest database object-ID list still contains A.
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );
    assert_eq!(
        meter.formatted_metric("test_objects_per_state{fresh=\"true\",state=\"a\",substate=\"\"}"),
        Some("1".to_string())
    );
    assert_eq!(
        meter.formatted_metric("test_objects_per_state{fresh=\"true\",state=\"b\",substate=\"\"}"),
        None
    );

    // Action: read and publish the database list containing B, but deliberately
    // do not process the B task that this sweep queues.
    let enqueuer_result = controller.enqueuer.run_single_iteration().await;
    assert!(!enqueuer_result.skipped_iteration);
    controller.processor.emit_metrics();

    // Assertion: the published database ID list contains B and excludes A.
    let object_ids_from_database = controller
        .processor
        .object_ids_from_database
        .as_ref()
        .expect("database object-ID receiver should be configured")
        .borrow()
        .clone()
        .expect("successful read should publish database object IDs");
    assert!(object_ids_from_database.contains(RENAMED_ID));
    assert!(!object_ids_from_database.contains(ORIGINAL_ID));

    // Assertion: the previously retained B state contributes without another
    // B completion, while retained A no longer contributes.
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );
    assert_eq!(
        meter.formatted_metric("test_objects_per_state{fresh=\"true\",state=\"a\",substate=\"\"}"),
        None
    );
    assert_eq!(
        meter.formatted_metric("test_objects_per_state{fresh=\"true\",state=\"b\",substate=\"\"}"),
        Some("1".to_string())
    );

    // Action: remove the deliberately unprocessed B queue row, then process a
    // stale queued A after the database ID list contains B.
    let mut txn = pool.begin().await?;
    sqlx::query("DELETE FROM test_state_controller_queued_objects WHERE object_id=$1")
        .bind(RENAMED_ID)
        .execute(&mut *txn)
        .await?;
    db::queue_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
        &[ORIGINAL_ID.to_string()],
    )
    .await?;
    txn.commit().await?;
    assert_eq!(
        controller
            .processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let result = controller
        .processor
        .object_tasks
        .join_next()
        .await
        .unwrap()?;
    assert!(matches!(
        &result.metrics.common.error,
        Some(StateHandlerError::MissingData { .. })
    ));
    controller
        .processor
        .process_object_handling_task_result(result, false, Instant::now());
    controller.processor.finalize_completed_objects().await?;
    controller.processor.emit_metrics();

    // Assertion: a stale missing-object result cannot restore A to the total.
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );

    Ok(())
}

/// Verifies that a valid A task completing after the database ID list changes
/// to B cannot restore A to the aggregate, while A's measurement remains cached.
#[carbide_macros::sqlx_test]
async fn late_result_for_id_absent_from_database_does_not_contribute(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    const ORIGINAL_ID: &str = "predicted-host";
    const RENAMED_ID: &str = "stable-host";

    // Setup: publish a database ID list containing A and dispatch its task
    // without inserting the completed result into the processor cache yet.
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        ::db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
    let mut txn = pool.begin().await?;
    create_test_object(ORIGINAL_ID.to_string(), &mut txn).await;
    txn.commit().await?;
    let meter = TestMeter::default();
    let mut controller = crate::controller::StateController::<TestStateControllerIO>::builder()
        .database(pool.clone(), work_lock_manager_handle)
        .meter("test_objects", meter.meter())
        .filter_metrics_by_existing_object_ids()
        .processor_id("late-result-test".to_string())
        .services(Arc::new(()))
        .build_for_manual_iterations(CancellationToken::new())?;
    assert!(
        controller
            .enqueuer
            .run_single_iteration()
            .await
            .iteration
            .is_some()
    );
    assert_eq!(
        controller
            .processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let late_result = controller
        .processor
        .object_tasks
        .join_next()
        .await
        .unwrap()?;
    assert_eq!(late_result.object_id, ORIGINAL_ID);
    assert!(late_result.metrics.common.error.is_none());

    // Action: rename the persisted object and publish B before accepting the
    // already completed A result.
    let mut txn = pool.begin().await?;
    sqlx::query("UPDATE test_objects SET id=$1 WHERE id=$2")
        .bind(RENAMED_ID)
        .bind(ORIGINAL_ID)
        .execute(&mut *txn)
        .await?;
    txn.commit().await?;
    assert!(
        controller
            .enqueuer
            .run_single_iteration()
            .await
            .iteration
            .is_some()
    );

    // Action: process B first, then deliver the already completed A result.
    assert_eq!(
        controller
            .processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let current_result = controller
        .processor
        .object_tasks
        .join_next()
        .await
        .unwrap()?;
    assert_eq!(current_result.object_id, RENAMED_ID);
    controller
        .processor
        .process_object_handling_task_result(current_result, false, Instant::now());
    controller.processor.finalize_completed_objects().await?;
    controller
        .processor
        .process_object_handling_task_result(late_result, false, Instant::now());
    controller.processor.finalize_completed_objects().await?;
    controller.processor.emit_metrics();

    // Assertion: A remains cached for timing safety but only B contributes
    // once the complete database ID list contains B instead.
    assert!(
        controller
            .processor
            .object_metrics
            .contains_key(ORIGINAL_ID)
    );
    assert!(controller.processor.object_metrics.contains_key(RENAMED_ID));
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );

    Ok(())
}

/// A test handler that reports an ordinary processing error for an object whose
/// ID remains in the database, distinguishing handler failure from ID removal.
#[derive(Debug)]
struct AlwaysErrorHandler;

#[async_trait::async_trait]
impl StateHandler for AlwaysErrorHandler {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ContextObjects = TestStateControllerContextObjects;

    /// Returns a normal handler error so the test can prove that failures for
    /// IDs still in the database remain eligible for aggregate counting.
    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        _controller_state: &TestObjectControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<TestObjectControllerState>, StateHandlerError> {
        Err(StateHandlerError::GenericError(eyre::eyre!(
            "injected ordinary handler failure"
        )))
    }
}

/// Verifies that a failed database object-ID read preserves the last successful
/// list and the useful error measurement for an object still in it.
#[carbide_macros::sqlx_test]
async fn failed_object_id_read_preserves_existing_object_error_metrics(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    const OBJECT_ID: &str = "live-host";

    // Setup: publish one existing object ID and process an ordinary handler error.
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        ::db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
    let io = Arc::new(TestStateControllerIO::default());
    let mut txn = pool.begin().await?;
    create_test_object(OBJECT_ID.to_string(), &mut txn).await;
    txn.commit().await?;
    let meter = TestMeter::default();
    let mut controller = crate::controller::StateController::<TestStateControllerIO>::builder()
        .database(pool, work_lock_manager_handle)
        .meter("test_objects", meter.meter())
        .filter_metrics_by_existing_object_ids()
        .processor_id("failed-object-id-read-test".to_string())
        .services(Arc::new(()))
        .io(io.clone())
        .state_handler(Arc::new(AlwaysErrorHandler))
        .build_for_manual_iterations(CancellationToken::new())?;
    assert!(
        controller
            .enqueuer
            .run_single_iteration()
            .await
            .iteration
            .is_some()
    );
    let successful_object_ids_from_database = controller
        .processor
        .object_ids_from_database
        .as_ref()
        .expect("database object-ID receiver should be configured")
        .borrow()
        .clone()
        .expect("successful read should publish database object IDs");
    assert_eq!(successful_object_ids_from_database.len(), 1);
    assert!(successful_object_ids_from_database.contains(OBJECT_ID));
    assert_eq!(
        controller
            .processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let result = controller
        .processor
        .object_tasks
        .join_next()
        .await
        .unwrap()?;
    assert!(matches!(
        &result.metrics.common.error,
        Some(StateHandlerError::GenericError(_))
    ));
    controller
        .processor
        .process_object_handling_task_result(result, false, Instant::now());
    controller.processor.finalize_completed_objects().await?;
    controller.processor.emit_metrics();
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );

    // Action: inject a failed listing; it must not publish an empty replacement.
    io.fail_next_list_objects();
    let failed_iteration = controller.enqueuer.run_single_iteration().await;
    assert!(failed_iteration.iteration.is_none());
    assert!(!failed_iteration.skipped_iteration);
    controller.processor.emit_metrics();

    // Assertion: failure preserves the same nonempty list rather than
    // reverting to the startup fallback or publishing an empty list.
    let retained_object_ids_from_database = controller
        .processor
        .object_ids_from_database
        .as_ref()
        .expect("database object-ID receiver should be configured")
        .borrow()
        .clone()
        .expect("failed read should preserve the prior database object IDs");
    assert_eq!(
        retained_object_ids_from_database,
        successful_object_ids_from_database
    );

    // Assertion: the retained existing object's ordinary error metric survives.
    assert_eq!(
        meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
        Some("1".to_string())
    );

    Ok(())
}

/// Verifies that an enabled replica refreshes its database object-ID list even
/// when another replica holds the global queue-enqueuing lock.
#[carbide_macros::sqlx_test]
async fn non_lock_winner_refreshes_object_ids_from_database(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    const OBJECT_ID: &str = "live-host";

    // Setup: create a database object and hold the singleton iteration lock
    // from outside the controller under test.
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        ::db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;
    let mut txn = pool.begin().await?;
    create_test_object(OBJECT_ID.to_string(), &mut txn).await;
    txn.commit().await?;
    let locked_iteration = db::lock_and_start_iteration(
        &pool,
        &work_lock_manager_handle,
        TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
    )
    .await?;
    let mut controller = crate::controller::StateController::<TestStateControllerIO>::builder()
        .database(pool, work_lock_manager_handle)
        .filter_metrics_by_existing_object_ids()
        .processor_id("non-lock-winner-test".to_string())
        .services(Arc::new(()))
        .build_for_manual_iterations(CancellationToken::new())?;

    // Action: run an iteration that cannot acquire the queue lock.
    let iteration = controller.enqueuer.run_single_iteration().await;
    assert!(iteration.skipped_iteration);

    // Assertion: the pre-lock committed read was still published locally.
    let object_ids_from_database = controller
        .processor
        .object_ids_from_database
        .as_ref()
        .expect("database object-ID receiver should be configured")
        .borrow()
        .clone()
        .expect("successful read should publish database object IDs");
    assert_eq!(object_ids_from_database.len(), 1);
    assert!(object_ids_from_database.contains(OBJECT_ID));

    std::mem::drop(locked_iteration);
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

/// Returns an immediate-requeue Wait so processor tests can verify queue scheduling without a state
/// transition.
#[derive(Debug)]
struct RequeueingHandler;

#[async_trait::async_trait]
impl StateHandler for RequeueingHandler {
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
        Ok(StateHandlerOutcome::wait_and_requeue(
            "external progress requires another pass".to_string(),
        ))
    }
}

/// Requeues a committed Wait without advancing the controller version or writing state history,
/// proving queue scheduling remains independent of durable state transitions.
#[carbide_macros::sqlx_test]
async fn immediate_requeue_wait_preserves_state_version_and_history(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    // Create and queue one object so the processor owns the complete claim and finalization path.
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
    let original_version: ConfigVersion =
        sqlx::query_scalar("SELECT controller_state_version FROM test_objects WHERE id = 'host'")
            .fetch_one(&pool)
            .await?;

    // Run the handler through normal dispatch so requeue occurs only after its Wait outcome
    // commits.
    let mut processor = processor(pool.clone());
    let io = Arc::new(TestStateControllerIO::default());
    processor.io = Arc::clone(&io);
    processor.state_handler = Arc::new(RequeueingHandler);
    assert_eq!(
        processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let result = processor.object_tasks.join_next().await.unwrap()?;
    assert!(result.metrics.common.requeue_immediately);
    processor.process_object_handling_task_result(result, true, Instant::now());
    processor.finalize_completed_objects().await?;

    // Confirm scheduling changed only the queue while controller state and history remained
    // untouched.
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM test_state_controller_queued_objects")
            .fetch_one(&pool)
            .await?;
    let version: ConfigVersion =
        sqlx::query_scalar("SELECT controller_state_version FROM test_objects WHERE id = 'host'")
            .fetch_one(&pool)
            .await?;
    let outcome: sqlx::types::Json<PersistentStateHandlerOutcome> =
        sqlx::query_scalar("SELECT controller_state_outcome FROM test_objects WHERE id = 'host'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(queued, 1);
    assert_eq!(version, original_version);
    assert_eq!(io.history_writes.load(Ordering::SeqCst), 0);
    assert!(matches!(
        outcome.0,
        PersistentStateHandlerOutcome::Wait { ref reason, .. }
            if reason == "external progress requires another pass"
    ));
    Ok(())
}

/// Verifies a failed Wait-outcome commit cannot request immediate requeue, preventing queue
/// scheduling from escaping rolled-back controller state.
#[carbide_macros::sqlx_test]
async fn immediate_requeue_wait_requires_committed_outcome(pool: sqlx::PgPool) -> eyre::Result<()> {
    // Create and queue one object before installing the commit-time failure used by this case.
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

    // Reject the persisted Wait only at commit so the handler's outcome write itself succeeds.
    sqlx::raw_sql(
        "CREATE FUNCTION reject_wait_outcome_commit() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.controller_state_outcome->>'outcome' = 'wait' THEN
                RAISE EXCEPTION 'reject committed Wait outcome';
            END IF;
            RETURN NEW;
        END;
        $$;
        CREATE CONSTRAINT TRIGGER reject_wait_outcome_commit
            AFTER UPDATE ON test_objects
            DEFERRABLE INITIALLY DEFERRED FOR EACH ROW
            EXECUTE FUNCTION reject_wait_outcome_commit();",
    )
    .execute(&pool)
    .await?;

    // Run the immediate-requeue handler through the same transaction and queue-finalization path as
    // production.
    let mut processor = processor(pool.clone());
    processor.state_handler = Arc::new(RequeueingHandler);
    assert_eq!(
        processor
            .dequeue_and_dispatch_object_handling_tasks()
            .await?,
        1
    );
    let result = processor.object_tasks.join_next().await.unwrap()?;

    // Require the commit failure to suppress requeue before the processor finalizes the claim.
    assert!(matches!(
        &result.metrics.common.error,
        Some(StateHandlerError::TransactionError(_))
    ));
    assert!(!result.metrics.common.requeue_immediately);
    processor.process_object_handling_task_result(result, true, Instant::now());
    processor.finalize_completed_objects().await?;

    // Confirm queue finalization removes the claim while the rejected Wait outcome remains absent.
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM test_state_controller_queued_objects")
            .fetch_one(&pool)
            .await?;
    let outcome: Option<sqlx::types::Json<PersistentStateHandlerOutcome>> =
        sqlx::query_scalar("SELECT controller_state_outcome FROM test_objects WHERE id = 'host'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(queued, 0);
    assert!(outcome.is_none());
    Ok(())
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
