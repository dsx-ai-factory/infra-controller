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

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_utils::test_support::test_meter::TestMeter;
use config_version::{ConfigVersion, Versioned};
use db::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use futures::{FutureExt, StreamExt};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use serde::{self, Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgConnection, Row};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::CheckApplied;
use crate::config::IterationConfig;
use crate::controller::{self, Enqueuer, QueuedObject, StateController};
use crate::db_write_batch::WriteOpFn;
use crate::io::StateControllerIO;
use crate::metrics::NoopMetricsEmitter;
use crate::state_change_emitter::{StateChangeEmitterBuilder, StateChangeEvent, StateChangeHook};
use crate::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerContextObjects, StateHandlerError,
    StateHandlerOutcome,
};

#[carbide_macros::sqlx_test]
async fn test_start_iteration(pool: sqlx::PgPool) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    // First iteration can acquire the lock
    let result = controller::db::lock_and_start_iteration(
        &pool,
        &work_lock_manager_handle,
        TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
    )
    .await
    .unwrap();
    assert_eq!(result.iteration_data.id.0, 1);

    // Second lock will fail
    assert!(
        controller::db::lock_and_start_iteration(
            &pool,
            &work_lock_manager_handle,
            TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME
        )
        .await
        .is_err()
    );

    // Release the lock
    std::mem::drop(result);

    let result = controller::db::lock_and_start_iteration(
        &pool,
        &work_lock_manager_handle,
        TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
    )
    .await
    .unwrap();
    assert_eq!(result.iteration_data.id.0, 2);

    Ok(())
}

#[carbide_macros::sqlx_test]
async fn test_delete_outdated_iterations(pool: sqlx::PgPool) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    // If we insert up to 10 iterations, all of them shoudl be visible
    for i in 1..=10 {
        let result = controller::db::lock_and_start_iteration(
            &pool,
            &work_lock_manager_handle,
            TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
        )
        .await
        .unwrap();
        assert_eq!(result.iteration_data.id.0, i);

        let mut txn = pool.begin().await?;
        let mut results = controller::db::fetch_iterations(
            &mut txn,
            TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), i as usize);
        results.reverse();
        for j in 0..i {
            assert_eq!(results[j as usize].id.0, j + 1);
        }

        txn.commit().await.unwrap();
    }

    // Once we are above 10, we retain the latest 10 iterations
    for i in 11..=20 {
        let result = controller::db::lock_and_start_iteration(
            &pool,
            &work_lock_manager_handle,
            TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
        )
        .await
        .unwrap();
        assert_eq!(result.iteration_data.id.0, i);

        let mut txn = pool.begin().await?;
        let mut results = controller::db::fetch_iterations(
            &mut txn,
            TestStateControllerIO::DB_ITERATION_ID_TABLE_NAME,
            None,
        )
        .await
        .unwrap();
        assert_eq!(results.len(), 10);
        results.reverse();
        for j in 0..10 {
            assert_eq!(results[j as usize].id.0, i - 9 + j);
        }

        txn.commit().await.unwrap();
    }

    Ok(())
}

#[derive(Debug, Default)]
struct TestStateControllerIO {
    history_writes: AtomicUsize,
}

#[derive(Debug, Clone)]
struct TestObject {
    id: String,
    controller_state: Versioned<TestObjectControllerState>,
}

impl<'r> FromRow<'r, PgRow> for TestObject {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let controller_state: sqlx::types::Json<TestObjectControllerState> =
            row.try_get("controller_state")?;
        Ok(TestObject {
            id: row.try_get("id")?,
            controller_state: Versioned::new(
                controller_state.0,
                row.try_get("controller_state_version")?,
            ),
        })
    }
}

/// State of a IB subnet as tracked by the controller
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
enum TestObjectControllerState {
    A,
    B,
    C,
}

struct TestStateControllerContextObjects {}

impl StateHandlerContextObjects for TestStateControllerContextObjects {
    type Services = ();
    type ObjectMetrics = ();
}

#[derive(Debug, Default)]
struct PanicInListObjectsStateControllerIO;

async fn create_test_state_controller_tables(pool: &sqlx::PgPool) {
    let mut txn = pool.begin().await.unwrap();

    sqlx::query(
        "CREATE TABLE test_objects(
        id             varchar NOT NULL,
        controller_state         jsonb       NOT NULL,
        controller_state_version VARCHAR(64) NOT NULL,
        controller_state_outcome JSONB
    );",
    )
    .execute(&mut *txn)
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE test_state_controller_lock(
        id uuid DEFAULT gen_random_uuid() NOT NULL
    );",
    )
    .execute(&mut *txn)
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE test_state_controller_iteration_ids(
        id BIGSERIAL PRIMARY KEY,
        started_at TIMESTAMP WITH TIME ZONE DEFAULT NOW()
    );",
    )
    .execute(&mut *txn)
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE test_state_controller_queued_objects(
        object_id VARCHAR PRIMARY KEY,
        processed_by TEXT NULL,
        processing_started_at timestamptz NOT NULL DEFAULT NOW()
    );",
    )
    .execute(&mut *txn)
    .await
    .unwrap();

    txn.commit().await.unwrap();
}

async fn create_test_object(id: String, txn: &mut PgConnection) -> TestObject {
    let version: ConfigVersion = ConfigVersion::initial();
    let state = TestObjectControllerState::A;

    let query = "INSERT INTO test_objects(id, controller_state, controller_state_version)
        VALUES($1, $2::json, $3)
        RETURNING *";
    sqlx::query_as(query)
        .bind(id)
        .bind(sqlx::types::Json(state))
        .bind(version)
        .fetch_one(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
        .unwrap()
}

#[async_trait::async_trait]
impl StateControllerIO for TestStateControllerIO {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = TestStateControllerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "test_state_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "test_state_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "test_state_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        let query = "SELECT id FROM test_objects";
        let mut results = Vec::new();
        let mut segment_id_stream = sqlx::query_scalar(query).fetch(txn);
        while let Some(maybe_id) = segment_id_stream.next().await {
            let id = maybe_id.map_err(|e| DatabaseError::query(query, e))?;
            results.push(id);
        }

        Ok(results)
    }

    /// Loads a state snapshot from the database
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        let query = "SELECT * FROM test_objects where id = $1";
        let object = sqlx::query_as::<_, TestObject>(query)
            .bind(object_id)
            .fetch_optional(txn)
            .await
            .map_err(|e| DatabaseError::new("select", e))?;

        return Ok(object);
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        Ok(state.controller_state.clone())
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<bool, DatabaseError> {
        let query = "UPDATE test_objects SET controller_state_version=$1, controller_state=$2::json
            where id=$3 AND controller_state_version=$4 returning id";
        let result = sqlx::query_scalar::<_, String>(query)
            .bind(new_version)
            .bind(sqlx::types::Json(new_state))
            .bind(object_id)
            .bind(old_version)
            .fetch_optional(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;

        Ok(result.is_some())
    }

    async fn persist_state_history(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        _new_version: ConfigVersion,
        _new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        self.history_writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        let query = "UPDATE test_objects SET controller_state_outcome=$1::json WHERE id=$2";
        sqlx::query(query)
            .bind(sqlx::types::Json(outcome))
            .bind(object_id)
            .execute(txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
        Ok(())
    }

    fn metric_state_names(state: &TestObjectControllerState) -> (&'static str, &'static str) {
        match state {
            TestObjectControllerState::A => ("a", ""),
            TestObjectControllerState::B => ("b", ""),
            TestObjectControllerState::C => ("c", ""),
        }
    }

    fn state_sla(
        &self,
        _state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        StateSla {
            sla: None,
            time_in_state_above_sla: false,
        }
    }
}

#[async_trait::async_trait]
impl StateControllerIO for PanicInListObjectsStateControllerIO {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = TestStateControllerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "test_state_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "test_state_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "test_state_controller";

    async fn list_objects(
        &self,
        _txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        panic!("test panic from list_objects");
    }

    async fn load_object_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        unreachable!("load_object_state should never be called in this test")
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        _state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        unreachable!("load_controller_state should never be called in this test")
    }

    async fn persist_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        _old_version: ConfigVersion,
        _new_version: ConfigVersion,
        _new_state: &Self::ControllerState,
    ) -> Result<bool, DatabaseError> {
        unreachable!("persist_controller_state should never be called in this test")
    }

    async fn persist_state_history(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        _new_version: ConfigVersion,
        _new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        unreachable!("persist_state_history should never be called in this test")
    }

    async fn persist_outcome(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        _outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        unreachable!("persist_outcome should never be called in this test")
    }

    fn metric_state_names(state: &TestObjectControllerState) -> (&'static str, &'static str) {
        TestStateControllerIO::metric_state_names(state)
    }

    fn state_sla(
        &self,
        _state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        StateSla {
            sla: None,
            time_in_state_above_sla: false,
        }
    }
}

#[carbide_macros::sqlx_test]
async fn test_state_controller_handle_set_wait_all_propagates_panic(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    StateController::<PanicInListObjectsStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(10),
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool.clone(), work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .state_handler(Arc::new(TestTransitionStateHandler))
        .build_and_spawn(&mut join_set, cancel_token.clone())?;

    let wait_result = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::spawn(async move { join_set.join_all().await }),
    )
    .await
    .expect("timed out waiting for wait_all to return");

    assert!(wait_result.expect_err("wait_all should panic").is_panic());

    Ok(())
}

#[derive(Debug, Default, Clone)]
struct TestConcurrencyStateHandler {
    /// The total count for the handler
    count: Arc<AtomicUsize>,
    /// We count for every object ID how often the handler was called
    counts_per_id: Arc<Mutex<HashMap<String, usize>>>,
}

#[derive(Debug)]
struct DrainingStateHandler {
    started: Arc<Semaphore>,
    release: Arc<Semaphore>,
    completed: Arc<AtomicUsize>,
}

#[derive(Debug)]
struct TimeoutDuringDrainStateHandler {
    started: Arc<Semaphore>,
}

#[async_trait::async_trait]
impl StateHandler for TimeoutDuringDrainStateHandler {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        _controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        self.started.add_permits(1);
        std::future::pending().await
    }
}

#[async_trait::async_trait]
impl StateHandler for DrainingStateHandler {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &String,
        _state: &mut TestObject,
        _controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        self.started.add_permits(1);
        self.release
            .acquire()
            .await
            .expect("Release semaphore should remain open")
            .forget();
        self.completed.fetch_add(1, Ordering::SeqCst);

        if object_id == "transition" {
            Ok(StateHandlerOutcome::transition(
                TestObjectControllerState::B,
            ))
        } else {
            Ok(StateHandlerOutcome::do_nothing())
        }
    }
}

#[carbide_macros::sqlx_test]
async fn test_state_controller_drains_claimed_work_on_shutdown(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    let mut txn = pool.begin().await?;
    create_test_object("stationary".to_string(), &mut txn).await;
    create_test_object("transition".to_string(), &mut txn).await;
    txn.commit().await?;

    let started = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let processor_id = uuid::Uuid::new_v4().to_string();
    let handler = Arc::new(DrainingStateHandler {
        started: started.clone(),
        release: release.clone(),
        completed: completed.clone(),
    });

    StateController::<TestStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_secs(60),
            processor_dispatch_interval: Duration::from_millis(10),
            max_object_handling_time: Duration::from_secs(5),
            max_concurrency: 2,
            ..Default::default()
        })
        .database(pool.clone(), work_lock_manager_handle.clone())
        .processor_id(processor_id.clone())
        .services(Arc::new(()))
        .state_handler(handler)
        .build_and_spawn(&mut join_set, cancel_token.clone())?;
    drop(work_lock_manager_handle);

    tokio::time::timeout(Duration::from_secs(5), started.acquire_many(2))
        .await
        .expect("State handlers did not start")?
        .forget();

    let mut txn = pool.begin().await?;
    let mut queued = controller::db::fetch_queued_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
    )
    .await?;
    txn.commit().await?;
    queued.sort_by(|a, b| a.object_id.cmp(&b.object_id));
    assert_eq!(queued.len(), 2);
    assert!(
        queued
            .iter()
            .all(|object| object.processed_by.as_deref() == Some(processor_id.as_str()))
    );

    // Queue another object while the processor is at capacity. Cancellation
    // must prevent it from claiming this work after the two running handlers
    // complete.
    let mut txn = pool.begin().await?;
    create_test_object("pending".to_string(), &mut txn).await;
    txn.commit().await?;
    Enqueuer::<TestStateControllerIO>::new(pool.clone())
        .enqueue_object(&"pending".to_string())
        .await?;

    cancel_token.cancel();
    let mut shutdown = tokio::spawn(async move { join_set.join_all().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut shutdown)
            .await
            .is_err(),
        "Controller shut down before claimed work completed"
    );

    release.add_permits(2);
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("Controller did not finish draining")
        .expect("Controller task panicked");
    assert_eq!(completed.load(Ordering::SeqCst), 2);

    let mut txn = pool.begin().await?;
    let mut queued = controller::db::fetch_queued_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
    )
    .await?;
    let transition_state: sqlx::types::Json<TestObjectControllerState> =
        sqlx::query_scalar("SELECT controller_state FROM test_objects WHERE id = 'transition'")
            .fetch_one(&mut *txn)
            .await?;
    txn.commit().await?;
    queued.sort_by(|a, b| a.object_id.cmp(&b.object_id));

    assert_eq!(
        queued,
        vec![
            QueuedObject {
                object_id: "pending".to_string(),
                processed_by: None,
            },
            QueuedObject {
                object_id: "transition".to_string(),
                processed_by: None,
            },
        ]
    );
    assert_eq!(transition_state.0, TestObjectControllerState::B);

    Ok(())
}

#[carbide_macros::sqlx_test]
async fn test_state_controller_handler_timeout_bounds_shutdown_drain(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    let mut txn = pool.begin().await?;
    create_test_object("timeout".to_string(), &mut txn).await;
    txn.commit().await?;

    let started = Arc::new(Semaphore::new(0));
    StateController::<TestStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_secs(60),
            processor_dispatch_interval: Duration::from_millis(10),
            max_object_handling_time: Duration::from_millis(100),
            max_concurrency: 1,
            ..Default::default()
        })
        .database(pool.clone(), work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .state_handler(Arc::new(TimeoutDuringDrainStateHandler {
            started: started.clone(),
        }))
        .build_and_spawn(&mut join_set, cancel_token.clone())?;
    drop(work_lock_manager_handle);

    tokio::time::timeout(Duration::from_secs(5), started.acquire())
        .await
        .expect("State handler did not start")?
        .forget();
    cancel_token.cancel();

    tokio::time::timeout(Duration::from_secs(5), join_set.join_all())
        .await
        .expect("Handler timeout did not bound shutdown drain");

    let mut txn = pool.begin().await?;
    let queued = controller::db::fetch_queued_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
    )
    .await?;
    txn.commit().await?;
    assert!(queued.is_empty());

    Ok(())
}

#[async_trait::async_trait]
impl StateHandler for TestConcurrencyStateHandler {
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ObjectId = String;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &String,
        state: &mut TestObject,
        _controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        assert_eq!(state.id, *object_id);
        self.count.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = self.counts_per_id.lock().unwrap();
            *guard.entry(object_id.to_string()).or_default() += 1;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(StateHandlerOutcome::do_nothing())
    }
}

#[carbide_macros::sqlx_test]
async fn test_multiple_state_controllers_schedule_object_only_once(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    let num_objects = 4;
    let mut object_ids = Vec::new();
    let mut txn = pool.begin().await.unwrap();
    for idx in 0..num_objects {
        let obj = create_test_object(idx.to_string(), &mut txn).await;
        object_ids.push(obj.id);
    }
    txn.commit().await.unwrap();

    let state_handler = Arc::new(TestConcurrencyStateHandler::default());
    const ITERATION_TIME: Duration = Duration::from_millis(100);
    const TEST_TIME: Duration = Duration::from_secs(10);
    let expected_iterations = (TEST_TIME.as_millis() / ITERATION_TIME.as_millis()) as f64;
    let expected_total_count = expected_iterations * object_ids.len() as f64;

    // We build multiple state controllers. But since only one should act at a time,
    // the count should still not increase
    for _ in 0..10 {
        StateController::<TestStateControllerIO>::builder()
            .iteration_config(IterationConfig {
                iteration_time: ITERATION_TIME,
                processor_dispatch_interval: std::time::Duration::from_millis(10),
                ..Default::default()
            })
            .database(pool.clone(), work_lock_manager_handle.clone())
            .processor_id(uuid::Uuid::new_v4().to_string())
            .services(Arc::new(()))
            .state_handler(state_handler.clone())
            .build_and_spawn(&mut join_set, cancel_token.clone())
            .unwrap();
    }

    std::mem::drop(work_lock_manager_handle); // Won't actually cancel until all controllers are dropped

    tokio::time::sleep(TEST_TIME).await;
    cancel_token.cancel();
    tokio::time::timeout(Duration::from_secs(10), join_set.join_all())
        .await
        .expect("Tasks did not complete after a timeout");

    let count = state_handler.count.load(Ordering::SeqCst) as f64;
    assert!(
        count >= 0.60 * expected_total_count && count <= 1.25 * expected_total_count,
        "Expected count of {expected_total_count}, but got {count}"
    );

    for object_id in object_ids {
        let guard = state_handler.counts_per_id.lock().unwrap();
        let count = guard
            .get(&object_id.to_string())
            .copied()
            .unwrap_or_default() as f64;

        assert!(
            count >= 0.60 * expected_iterations && count <= 1.25 * expected_iterations,
            "Expected individual count of {expected_iterations}, but got {count} for {object_id}"
        );
    }

    Ok(())
}

/// A state handler that transitions from A -> B -> C
#[derive(Debug, Default, Clone)]
struct TestTransitionStateHandler;

#[async_trait::async_trait]
impl StateHandler for TestTransitionStateHandler {
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ObjectId = String;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        match controller_state {
            TestObjectControllerState::A => Ok(StateHandlerOutcome::transition(
                TestObjectControllerState::B,
            )),
            TestObjectControllerState::B => Ok(StateHandlerOutcome::transition(
                TestObjectControllerState::C,
            )),
            TestObjectControllerState::C => Ok(StateHandlerOutcome::do_nothing()),
        }
    }
}

/// A state handler that transitions from A -> B -> A
#[derive(Debug, Default, Clone)]
struct CyclicTransitionStateHandler;

#[async_trait::async_trait]
impl StateHandler for CyclicTransitionStateHandler {
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ObjectId = String;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        match controller_state {
            TestObjectControllerState::A => Ok(StateHandlerOutcome::transition(
                TestObjectControllerState::B,
            )),
            TestObjectControllerState::B => Ok(StateHandlerOutcome::transition(
                TestObjectControllerState::A,
            )),
            TestObjectControllerState::C => Err(StateHandlerError::InvalidState("C".to_string())),
        }
    }
}

/// Tests whether the amount of emitted metrics is stable
/// The test as checked in is mostly a smoke test
/// To get better test coverage, extend `TEST_TIME` to 3 or more minutes.
#[carbide_macros::sqlx_test]
async fn test_state_handler_metrics_are_stable(pool: sqlx::PgPool) -> eyre::Result<()> {
    let test_meter = TestMeter::default();

    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    let num_objects = 100;
    let mut object_ids = Vec::new();
    let mut txn = pool.begin().await.unwrap();
    for idx in 0..num_objects {
        let obj = create_test_object(idx.to_string(), &mut txn).await;
        object_ids.push(obj.id);
    }
    txn.commit().await.unwrap();

    let state_handler = Arc::new(CyclicTransitionStateHandler);
    const ITERATION_TIME: Duration = Duration::from_millis(100);
    const TEST_TIME: Duration = Duration::from_secs(10);
    let start_time = std::time::Instant::now();

    StateController::<TestStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: ITERATION_TIME,
            processor_dispatch_interval: std::time::Duration::from_millis(10),
            metric_emission_interval: std::time::Duration::from_millis(10),
            max_concurrency: num_objects,
            ..Default::default()
        })
        .meter("test_objects", test_meter.meter())
        .database(pool.clone(), work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .state_handler(state_handler.clone())
        .build_and_spawn(&mut join_set, cancel_token.clone())
        .unwrap();

    // Check metrics periodically. We always expect to see 100 objects
    while start_time.elapsed() < TEST_TIME {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        assert_eq!(
            test_meter.formatted_metric("test_objects_total{fresh=\"true\"}"),
            Some(num_objects.to_string()),
            "Test failed after {}s",
            start_time.elapsed().as_secs_f32()
        );
    }
    cancel_token.cancel();
    std::mem::drop(work_lock_manager_handle);
    tokio::time::timeout(Duration::from_secs(10), join_set.join_all())
        .await
        .expect("Tasks did not complete after a timeout");

    Ok(())
}

/// Captured state change data for test verification.
#[derive(Debug, Clone)]
struct CapturedStateChange {
    object_id: String,
    previous_state: Option<TestObjectControllerState>,
    new_state: TestObjectControllerState,
}

/// A hook that sends events through a channel for deterministic test verification
struct ChannelHook {
    sender: tokio::sync::mpsc::UnboundedSender<CapturedStateChange>,
}

impl ChannelHook {
    fn new() -> (
        Self,
        tokio::sync::mpsc::UnboundedReceiver<CapturedStateChange>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self { sender }, receiver)
    }
}

impl StateChangeHook<String, TestObjectControllerState> for ChannelHook {
    fn on_state_changed(&self, event: &StateChangeEvent<'_, String, TestObjectControllerState>) {
        let captured = CapturedStateChange {
            object_id: event.object_id.clone(),
            previous_state: event.previous_state.cloned(),
            new_state: event.new_state.clone(),
        };
        let _ = self.sender.send(captured);
    }
}

#[carbide_macros::sqlx_test]
async fn test_state_change_emitter_emits_events_on_transitions(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    // Create a single test object in state A
    let mut txn = pool.begin().await?;
    let obj = create_test_object("test-obj-1".to_string(), &mut txn).await;
    txn.commit().await?;

    // Create a channel hook to receive events deterministically
    let (hook, mut receiver) = ChannelHook::new();

    // Build the emitter with our channel hook
    let emitter = StateChangeEmitterBuilder::default()
        .hook(Box::new(hook))
        .build();

    // Build the state controller with the emitter
    let mut controller = StateController::<TestStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(50),
            ..Default::default()
        })
        .database(pool.clone(), work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .state_handler(Arc::new(TestTransitionStateHandler))
        .state_change_emitter(emitter)
        .build_for_manual_iterations(cancel_token)?;

    // Run first iteration: A -> B
    controller.run_single_iteration().await;
    let event1 = receiver
        .recv()
        .await
        .expect("Expected first state change event");
    assert_eq!(event1.object_id, obj.id);
    assert_eq!(event1.previous_state, Some(TestObjectControllerState::A));
    assert_eq!(event1.new_state, TestObjectControllerState::B);

    // Run second iteration: B -> C
    controller.run_single_iteration().await;
    let event2 = receiver
        .recv()
        .await
        .expect("Expected second state change event");
    assert_eq!(event2.object_id, obj.id);
    assert_eq!(event2.previous_state, Some(TestObjectControllerState::B));
    assert_eq!(event2.new_state, TestObjectControllerState::C);

    // Run third iteration: C -> do_nothing (no transition, no event)
    controller.run_single_iteration().await;
    // Verify no more events in the channel
    assert!(
        receiver.try_recv().is_err(),
        "Expected no event for do_nothing outcome"
    );

    Ok(())
}

#[carbide_macros::sqlx_test]
async fn test_state_controller_manual_enqueuing(pool: sqlx::PgPool) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    // Create a single test object in state A
    let mut txn = pool.begin().await?;
    let _obj = create_test_object("test-obj-1".to_string(), &mut txn).await;
    txn.commit().await?;

    // Build the state controller with the emitter
    let mut controller = StateController::<TestStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(50),
            processor_dispatch_interval: Duration::from_millis(50),
            ..Default::default()
        })
        .database(pool.clone(), work_lock_manager_handle.clone())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .state_handler(Arc::new(TestTransitionStateHandler))
        .build_for_manual_iterations(cancel_token)?;

    // Transition A -> B, but no re-enqueuing
    controller.run_single_iteration_ext(false).await;

    let mut txn = pool.begin().await?;
    let queued = controller::db::fetch_queued_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
    )
    .await
    .unwrap();
    assert!(queued.is_empty());
    txn.commit().await.unwrap();

    let enqueuer = Enqueuer::<TestStateControllerIO>::new(pool.clone());
    enqueuer.enqueue_object(&"test-obj-1".to_string()).await?;
    let mut txn = pool.begin().await?;
    let queued = controller::db::fetch_queued_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
    )
    .await
    .unwrap();
    assert_eq!(
        queued,
        vec![QueuedObject {
            object_id: "test-obj-1".to_string(),
            processed_by: None,
        },]
    );
    txn.commit().await.unwrap();

    Ok(())
}

/// A state handler that fails with `ManualInterventionRequired` on its first
/// invocation, a transient error on its second, and succeeds afterwards.
#[derive(Debug, Default, Clone)]
struct TestManualInterventionStateHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl StateHandler for TestManualInterventionStateHandler {
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ObjectId = String;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        _controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Err(StateHandlerError::ManualInterventionRequired(
                "operator needed".to_string(),
            )),
            1 => Err(StateHandlerError::GenericError(eyre::eyre!(
                "transient failure"
            ))),
            _ => Ok(StateHandlerOutcome::do_nothing()),
        }
    }
}

fn per_object_state_recorder(
    prometheus_registry: &prometheus::Registry,
) -> crate::per_object::PerObjectStateRecorder {
    let registry =
        carbide_health_metrics::PerObjectMetricsRegistry::new(Vec::new(), Duration::from_secs(60));
    crate::per_object::PerObjectStateRecorder::new(
        "test_object",
        crate::per_object::PerObjectStateMetrics::new(
            &registry,
            prometheus_registry,
            Duration::from_secs(60),
        )
        .unwrap(),
    )
}

/// Boilerplate shared by the per-object metrics tests: tables, one
/// `test-obj-1` object, and a controller wired with the given handler and a
/// per-object state recorder whose series land on the returned registry.
/// The `JoinSet` keeps the work-lock manager alive for the test's duration.
async fn per_object_test_controller<IO>(
    pool: &sqlx::PgPool,
    handler: Arc<
        dyn StateHandler<
                State = TestObject,
                ControllerState = TestObjectControllerState,
                ObjectId = String,
                ContextObjects = TestStateControllerContextObjects,
            >,
    >,
) -> eyre::Result<(StateController<IO>, prometheus::Registry, JoinSet<()>)>
where
    IO: StateControllerIO<
            ObjectId = String,
            State = TestObject,
            ControllerState = TestObjectControllerState,
            ContextObjects = TestStateControllerContextObjects,
        >,
{
    create_test_state_controller_tables(pool).await;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    let mut txn = pool.begin().await?;
    create_test_object("test-obj-1".to_string(), &mut txn).await;
    txn.commit().await?;

    let prometheus_registry = prometheus::Registry::new();
    let controller = StateController::<IO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(50),
            ..Default::default()
        })
        .database(pool.clone(), work_lock_manager_handle)
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .state_handler(handler)
        .per_object_state_metrics(Some(per_object_state_recorder(&prometheus_registry)))
        .build_for_manual_iterations(CancellationToken::new())?;

    Ok((controller, prometheus_registry, join_set))
}

/// `(sorted attribute block, value)` rows for the named metric, parsed from
/// the registry's Prometheus text exposition.
fn parsed_prometheus_metrics(
    registry: &prometheus::Registry,
    metric_name: &str,
) -> Vec<(String, String)> {
    use prometheus::Encoder;
    let mut buffer = Vec::new();
    prometheus::TextEncoder::new()
        .encode(&registry.gather(), &mut buffer)
        .unwrap();
    let formatted = String::from_utf8(buffer).unwrap();
    let mut rows = Vec::new();
    for line in formatted.lines() {
        let Some(rest) = line.strip_prefix(metric_name) else {
            continue;
        };
        if let Some(rest) = rest.strip_prefix('{') {
            let Some((attrs, value)) = rest.split_once("} ") else {
                continue;
            };
            rows.push((format!("{{{attrs}}}"), value.to_string()));
        } else if let Some(value) = rest.strip_prefix(' ') {
            rows.push((String::new(), value.to_string()));
        }
    }
    rows.sort();
    rows
}

#[carbide_macros::sqlx_test]
async fn test_per_object_state_metrics_record_observed_state(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let (mut controller, prometheus_registry, _join_set) =
        per_object_test_controller::<TestStateControllerIO>(
            &pool,
            Arc::new(TestManualInterventionStateHandler::default()),
        )
        .await?;

    // First iteration: the handler requires manual intervention.
    controller.run_single_iteration().await;

    let entered = parsed_prometheus_metrics(
        &prometheus_registry,
        "carbide_object_state_entered_timestamp_seconds",
    );
    assert_eq!(entered.len(), 1);
    let (attrs, value) = &entered[0];
    for expected in [
        r#"object_type="test_object""#,
        r#"object_id="test-obj-1""#,
        r#"state="a""#,
        r#"substate="""#,
    ] {
        assert!(attrs.contains(expected), "{expected} not in {attrs}");
    }
    let entered_at = value.parse::<f64>().unwrap();
    let now = chrono::Utc::now().timestamp() as f64;
    assert!(
        (now - entered_at).abs() < 60.0,
        "entered timestamp {entered_at} not close to now {now}"
    );

    // The test IO has no SLA for any state: no SLA series may exist.
    assert!(
        parsed_prometheus_metrics(&prometheus_registry, "carbide_object_state_sla_seconds")
            .is_empty()
    );

    let intervention = parsed_prometheus_metrics(
        &prometheus_registry,
        "carbide_object_manual_intervention_required",
    );
    assert_eq!(intervention.len(), 1);
    let (attrs, value) = &intervention[0];
    assert!(
        attrs.contains(r#"reason="manual_intervention_required""#),
        "reason not in {attrs}"
    );
    assert_eq!(value, "1");

    // Second iteration: a transient (non-intervention) error leaves the
    // status undetermined — the series must survive, not flap.
    controller.run_single_iteration().await;
    assert_eq!(
        parsed_prometheus_metrics(
            &prometheus_registry,
            "carbide_object_manual_intervention_required"
        )
        .len(),
        1
    );

    // Third iteration: the handler recovered, so the fact stops being true
    // and the series disappears.
    controller.run_single_iteration().await;
    assert!(
        parsed_prometheus_metrics(
            &prometheus_registry,
            "carbide_object_manual_intervention_required"
        )
        .is_empty()
    );

    Ok(())
}

/// Delegates to [`TestStateControllerIO`] but resolves an SLA for every state
/// and flags state C as requiring manual intervention.
#[derive(Debug, Default)]
struct SlaTestStateControllerIO {
    inner: TestStateControllerIO,
}

#[async_trait::async_trait]
impl StateControllerIO for SlaTestStateControllerIO {
    type ObjectId = String;
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = TestStateControllerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "test_state_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "test_state_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "test_state_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        self.inner.list_objects(txn).await
    }

    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        self.inner.load_object_state(txn, object_id).await
    }

    async fn load_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        self.inner
            .load_controller_state(txn, object_id, state)
            .await
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<bool, DatabaseError> {
        self.inner
            .persist_controller_state(txn, object_id, old_version, new_version, new_state)
            .await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        self.inner
            .persist_state_history(txn, object_id, new_version, new_state)
            .await
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        self.inner.persist_outcome(txn, object_id, outcome).await
    }

    fn metric_state_names(state: &TestObjectControllerState) -> (&'static str, &'static str) {
        TestStateControllerIO::metric_state_names(state)
    }

    fn manual_intervention_reason(state: &Self::ControllerState) -> Option<&'static str> {
        match state {
            TestObjectControllerState::C => Some("test_stuck"),
            _ => None,
        }
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        StateSla::with_sla(
            Duration::from_secs(1800),
            chrono::Utc::now()
                .signed_duration_since(state.version.timestamp())
                .to_std()
                .unwrap_or_default(),
        )
    }
}

#[carbide_macros::sqlx_test]
async fn test_per_object_state_metrics_sla_and_state_based_intervention(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let (mut controller, prometheus_registry, _join_set) =
        per_object_test_controller::<SlaTestStateControllerIO>(
            &pool,
            Arc::new(TestTransitionStateHandler),
        )
        .await?;

    // First iteration transitions A -> B and records the committed state B
    // immediately, including B's resolved SLA; B needs no manual intervention.
    controller.run_single_iteration().await;
    let entered = parsed_prometheus_metrics(
        &prometheus_registry,
        "carbide_object_state_entered_timestamp_seconds",
    );
    assert_eq!(entered.len(), 1);
    assert!(entered[0].0.contains(r#"state="b""#), "{}", entered[0].0);
    let sla = parsed_prometheus_metrics(&prometheus_registry, "carbide_object_state_sla_seconds");
    assert_eq!(sla.len(), 1);
    assert!(sla[0].0.contains(r#"state="b""#), "{}", sla[0].0);
    assert_eq!(sla[0].1, "1800");
    assert!(
        parsed_prometheus_metrics(
            &prometheus_registry,
            "carbide_object_manual_intervention_required"
        )
        .is_empty()
    );

    // B -> C: the committed state C is flagged by the IO as requiring
    // intervention as soon as it is entered.
    controller.run_single_iteration().await;
    let entered = parsed_prometheus_metrics(
        &prometheus_registry,
        "carbide_object_state_entered_timestamp_seconds",
    );
    assert_eq!(entered.len(), 1);
    assert!(entered[0].0.contains(r#"state="c""#), "{}", entered[0].0);
    let intervention = parsed_prometheus_metrics(
        &prometheus_registry,
        "carbide_object_manual_intervention_required",
    );
    assert_eq!(intervention.len(), 1);
    let (attrs, value) = &intervention[0];
    for expected in [r#"state="c""#, r#"reason="test_stuck""#] {
        assert!(attrs.contains(expected), "{expected} not in {attrs}");
    }
    assert_eq!(value, "1");

    // A further iteration without a transition keeps emitting C's SLA.
    controller.run_single_iteration().await;
    let sla = parsed_prometheus_metrics(&prometheus_registry, "carbide_object_state_sla_seconds");
    assert_eq!(sla.len(), 1);
    let (attrs, value) = &sla[0];
    assert!(attrs.contains(r#"state="c""#), "state not in {attrs}");
    assert_eq!(value, "1800");

    Ok(())
}

/// A state handler that reports the object as deleted on its second
/// invocation.
#[derive(Debug, Default, Clone)]
struct TestDeletionStateHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl StateHandler for TestDeletionStateHandler {
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ObjectId = String;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        _object_id: &String,
        _state: &mut TestObject,
        _controller_state: &Self::ControllerState,
        _ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(StateHandlerOutcome::do_nothing())
        } else {
            Ok(StateHandlerOutcome::deleted())
        }
    }
}

#[carbide_macros::sqlx_test]
async fn test_per_object_state_metrics_cleared_on_deletion(pool: sqlx::PgPool) -> eyre::Result<()> {
    let (mut controller, prometheus_registry, _join_set) =
        per_object_test_controller::<TestStateControllerIO>(
            &pool,
            Arc::new(TestDeletionStateHandler::default()),
        )
        .await?;

    controller.run_single_iteration().await;
    assert_eq!(
        parsed_prometheus_metrics(
            &prometheus_registry,
            "carbide_object_state_entered_timestamp_seconds"
        )
        .len(),
        1
    );

    // The deletion iteration must remove the object's series instead of
    // leaving them to assert a deleted object's state until eviction.
    controller.run_single_iteration().await;
    assert!(
        parsed_prometheus_metrics(
            &prometheus_registry,
            "carbide_object_state_entered_timestamp_seconds"
        )
        .is_empty()
    );

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvalidationStage {
    Handler,
    DeferredWrite,
    FinalStateWrite,
}

#[derive(Debug)]
struct TestInvalidationStateHandler {
    pool: sqlx::PgPool,
    stage: InvalidationStage,
    persisted_state: Versioned<TestObjectControllerState>,
    persisted_outcome: PersistentStateHandlerOutcome,
    calls: AtomicUsize,
    batch_writes: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl StateHandler for TestInvalidationStateHandler {
    type State = TestObject;
    type ControllerState = TestObjectControllerState;
    type ObjectId = String;
    type ContextObjects = TestStateControllerContextObjects;

    async fn handle_object_state(
        &self,
        object_id: &String,
        state: &mut TestObject,
        _controller_state: &Self::ControllerState,
        ctx: &mut StateHandlerContext<Self::ContextObjects>,
    ) -> Result<StateHandlerOutcome<Self::ControllerState>, StateHandlerError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) != 0 {
            assert_eq!(state.controller_state.value, self.persisted_state.value);
            assert_eq!(state.controller_state.version, self.persisted_state.version);
            return Ok(StateHandlerOutcome::transition(
                TestObjectControllerState::B,
            ));
        }

        // Keep the object's version current in the handler and batch cases.
        // Otherwise the final version check could reject the transition too,
        // letting the test pass even if we ignored the earlier rejection.
        if self.stage == InvalidationStage::FinalStateWrite {
            sqlx::query(
                "UPDATE test_objects SET controller_state=$1, controller_state_version=$2,
                    controller_state_outcome=$3 WHERE id=$4",
            )
            .bind(sqlx::types::Json(&self.persisted_state.value))
            .bind(self.persisted_state.version)
            .bind(sqlx::types::Json(&self.persisted_outcome))
            .bind(object_id)
            .execute(&self.pool)
            .await?;
        }

        let mut txn = self.pool.begin().await?;
        sqlx::query("INSERT INTO test_iteration_writes VALUES ('handler')")
            .execute(&mut *txn)
            .await?;

        let batch_writes = self.batch_writes.clone();
        let stage = self.stage;
        let write: WriteOpFn = Box::new(move |txn| {
            async move {
                sqlx::query("INSERT INTO test_iteration_writes VALUES ('batch')")
                    .execute(&mut **txn)
                    .await?;
                batch_writes.fetch_add(1, Ordering::SeqCst);
                if stage == InvalidationStage::DeferredWrite {
                    ConditionalWrite::<(), _>::NotApplied(ControllerStateNotCurrent)
                        .check_applied()?;
                }
                Ok(())
            }
            .boxed()
        });
        ctx.pending_db_writes.push(write);

        if self.stage == InvalidationStage::Handler {
            ConditionalWrite::<(), _>::NotApplied(ControllerStateNotCurrent).check_applied()?;
        }
        Ok(StateHandlerOutcome::transition(TestObjectControllerState::B).with_txn(txn))
    }
}

#[carbide_macros::sqlx_test]
async fn test_handler_invalidation_rolls_back_and_requeues(pool: sqlx::PgPool) -> eyre::Result<()> {
    check_invalidated_iteration(pool, InvalidationStage::Handler).await
}

#[carbide_macros::sqlx_test]
async fn test_deferred_write_invalidation_rolls_back_and_requeues(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    check_invalidated_iteration(pool, InvalidationStage::DeferredWrite).await
}

#[carbide_macros::sqlx_test]
async fn test_lock_loss_requeues_without_publishing_the_transition(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    check_invalidated_iteration(pool, InvalidationStage::FinalStateWrite).await
}

async fn check_invalidated_iteration(
    pool: sqlx::PgPool,
    stage: InvalidationStage,
) -> eyre::Result<()> {
    create_test_state_controller_tables(&pool).await;
    sqlx::query("CREATE TABLE test_iteration_writes (name TEXT NOT NULL)")
        .execute(&pool)
        .await?;
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default()).await?;

    let io = Arc::new(TestStateControllerIO::default());
    let mut txn = pool.begin().await?;
    let object = create_test_object("test-obj-1".to_string(), &mut txn).await;
    let prior_outcome = PersistentStateHandlerOutcome::Wait {
        reason: "previous iteration".to_string(),
        source_ref: None,
    };
    io.persist_outcome(&mut txn, &object.id, prior_outcome.clone())
        .await?;
    txn.commit().await?;

    let (persisted_state, persisted_outcome, expected_batch_writes) = match stage {
        InvalidationStage::Handler => (object.controller_state.clone(), prior_outcome, 0),
        InvalidationStage::DeferredWrite => (object.controller_state.clone(), prior_outcome, 1),
        InvalidationStage::FinalStateWrite => (
            Versioned::new(
                TestObjectControllerState::C,
                object.controller_state.version.increment(),
            ),
            PersistentStateHandlerOutcome::Wait {
                reason: "concurrent iteration".to_string(),
                source_ref: None,
            },
            1,
        ),
    };
    let handler = Arc::new(TestInvalidationStateHandler {
        pool: pool.clone(),
        stage,
        persisted_state,
        persisted_outcome,
        calls: AtomicUsize::new(0),
        batch_writes: Arc::new(AtomicUsize::new(0)),
    });
    let (hook, mut events) = ChannelHook::new();
    let meter = TestMeter::default();
    let prometheus_registry = prometheus::Registry::new();
    let mut controller = StateController::<TestStateControllerIO>::builder()
        .database(pool.clone(), work_lock_manager_handle)
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(()))
        .io(io.clone())
        .state_handler(handler.clone())
        .meter("test_objects", meter.meter())
        .state_change_emitter(
            StateChangeEmitterBuilder::default()
                .hook(Box::new(hook))
                .build(),
        )
        .per_object_state_metrics(Some(per_object_state_recorder(&prometheus_registry)))
        .build_for_manual_iterations(CancellationToken::new())?;

    controller.run_single_iteration().await;

    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        handler.batch_writes.load(Ordering::SeqCst),
        expected_batch_writes,
    );
    let writes: Vec<String> = sqlx::query_scalar("SELECT name FROM test_iteration_writes")
        .fetch_all(&pool)
        .await?;
    assert!(
        writes.is_empty(),
        "invalidated writes were committed: {writes:?}",
    );

    let persisted_query = "SELECT controller_state, controller_state_version,
        controller_state_outcome FROM test_objects WHERE id=$1";
    let (state, version, outcome): (
        sqlx::types::Json<TestObjectControllerState>,
        ConfigVersion,
        sqlx::types::Json<PersistentStateHandlerOutcome>,
    ) = sqlx::query_as(persisted_query)
        .bind(&object.id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state.0, handler.persisted_state.value);
    assert_eq!(version, handler.persisted_state.version);
    assert_eq!(outcome.0, handler.persisted_outcome);
    assert_eq!(io.history_writes.load(Ordering::SeqCst), 0);
    assert!(events.try_recv().is_err());
    assert!(
        meter
            .parsed_metrics("test_objects_state_entered_total")
            .is_empty(),
    );
    assert!(
        parsed_prometheus_metrics(
            &prometheus_registry,
            "carbide_object_state_entered_timestamp_seconds",
        )
        .is_empty()
    );
    assert_eq!(
        meter.formatted_metric("test_objects_object_tasks_errored_total"),
        None,
    );
    let errors = meter.parsed_metrics("test_objects_with_state_handling_errors_per_state");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].1, "0");

    let mut txn = pool.begin().await?;
    let queued = controller::db::fetch_queued_objects(
        &mut txn,
        TestStateControllerIO::DB_QUEUED_OBJECTS_TABLE_NAME,
    )
    .await?;
    txn.commit().await?;
    assert_eq!(
        queued,
        vec![QueuedObject {
            object_id: object.id.clone(),
            processed_by: None,
        }]
    );

    // The next pass must reload the persisted state before it can transition.
    controller.run_single_iteration().await;
    assert_eq!(handler.calls.load(Ordering::SeqCst), 2);
    let (state, new_version, outcome): (
        sqlx::types::Json<TestObjectControllerState>,
        ConfigVersion,
        sqlx::types::Json<PersistentStateHandlerOutcome>,
    ) = sqlx::query_as(persisted_query)
        .bind(&object.id)
        .fetch_one(&pool)
        .await?;
    assert_eq!(state.0, TestObjectControllerState::B);
    assert_eq!(new_version.version_nr(), version.version_nr() + 1);
    assert!(matches!(
        outcome.0,
        PersistentStateHandlerOutcome::Transition {
            source_ref: Some(_)
        },
    ));
    assert_eq!(io.history_writes.load(Ordering::SeqCst), 1);
    events.try_recv()?;
    assert!(events.try_recv().is_err());
    let entered = parsed_prometheus_metrics(
        &prometheus_registry,
        "carbide_object_state_entered_timestamp_seconds",
    );
    assert_eq!(entered.len(), 1);
    assert!(entered[0].0.contains(r#"state="b""#));

    Ok(())
}
