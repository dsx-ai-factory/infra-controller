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
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::db::{DatabaseError, Transaction};
use config_version::Versioned;
use model::controller_outcome::PersistentStateHandlerOutcome;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use rand::RngExt;
use sqlx_query_tracing::SqlxQueryDataAggregation;
use tokio::task::{JoinError, JoinSet};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::db;
use crate::config::IterationConfig;
use crate::db_write_batch::DbWriteBatch;
use crate::io::StateControllerIO;
use crate::metrics::{
    IterationMetrics, MetricHolder, ObjectHandlerMetrics, StateProcessorMetricEmitter,
};
use crate::per_object::{ManualIntervention, PerObjectStateRecorder};
use crate::state_change_emitter::{StateChangeEmitter, StateChangeEvent};
use crate::state_handler::{
    FromStateHandlerResult, StateHandler, StateHandlerContext, StateHandlerContextObjects,
    StateHandlerError, StateHandlerOutcome,
};

/// The `missing` token used when an object's state row is gone from the
/// database; the per-object metrics clear path matches on it.
const MISSING_OBJECT_STATE: &str = "object_state";

/// The `StateProcessor` is responsible for executing the state handler functions
/// for all objects where state handling is requested.
pub(super) struct StateProcessor<IO: StateControllerIO> {
    /// A database connection pool that can be used for additional queries
    pub(super) pool: sqlx::PgPool,
    pub(super) handler_services: Arc<<IO::ContextObjects as StateHandlerContextObjects>::Services>,
    pub(super) io: Arc<IO>,
    pub(super) state_handler: Arc<
        dyn StateHandler<
                State = IO::State,
                ControllerState = IO::ControllerState,
                ContextObjects = IO::ContextObjects,
                ObjectId = IO::ObjectId,
            >,
    >,
    pub(super) metric_emitter: Option<ProcessorMetricsEmitter>,
    pub(super) metric_holder: Arc<MetricHolder<IO>>,
    /// When set, every processed object's state/SLA/manual-intervention is
    /// recorded as per-object gauges.
    pub(super) per_object_state: Option<PerObjectStateRecorder>,

    pub(super) object_metrics: HashMap<IO::ObjectId, CollectedMetrics<IO>>,
    pub(super) cancel_token: CancellationToken,
    pub(super) iteration_config: IterationConfig,
    /// Object handling tasks currently executing. Keeping them in a JoinSet ensures
    /// the processor cannot exit while work it claimed is still running.
    pub(super) object_tasks: JoinSet<ObjectHandlingTaskResult<IO>>,
    /// Objects where the state handling task was finished but where the entry
    /// in the database has not yet been deleted.
    pub(super) completed_objects: HashSet<IO::ObjectId>,
    /// Objects for which another object handling task should be queued since
    /// the state handler returned `Transition`
    pub(super) requeue_objects: HashSet<IO::ObjectId>,
    /// The last time a log message had been emitted
    pub(super) last_log_time: std::time::Instant,
    /// The last time aggregate metrics had been emitted
    pub(super) last_metric_emission_time: std::time::Instant,
    pub(super) stats_since_last_log: StatsSinceLastLog,
    /// Globally unique ID that identifies the state controller (and processor) working on objects
    pub(super) processor_id: String,
    /// Emitter for broadcasting state change events to registered hooks.
    pub(super) state_change_emitter: Arc<StateChangeEmitter<IO::ObjectId, IO::ControllerState>>,
}
pub(super) struct ObjectHandlingTaskResult<IO: StateControllerIO> {
    object_id: IO::ObjectId,
    metrics: ObjectHandlerMetrics<IO>,
}

pub(super) struct CollectedMetrics<IO: StateControllerIO> {
    metrics: ObjectHandlerMetrics<IO>,
    collected_at: std::time::Instant,
}

#[derive(Debug)]
pub(super) struct ProcessorIterationMetrics {
    /// When the last state processor iteration started
    iteration_started_at: std::time::Instant,
}

impl std::default::Default for ProcessorIterationMetrics {
    fn default() -> Self {
        Self {
            iteration_started_at: std::time::Instant::now(),
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub(super) struct SingleIterationResult {
    /// The amount of object handling tasks that have been dequeued from the database
    /// and dispatched for handling
    #[allow(dead_code)]
    pub(super) num_dispatched_tasks: usize,
    /// The amount of object handling tasks which completed
    pub(super) num_completed_tasks: usize,
}

#[derive(Debug, Default, Clone)]
pub(super) struct StatsSinceLastLog {
    /// The amount of object handling tasks that have been dequeued from the database
    /// and dispatched for handling
    num_dispatched_tasks: usize,
    /// The amount of object handling tasks which completed
    num_completed_tasks: usize,
    /// The amount of tasks which returned an error
    num_errored_tasks: usize,
    /// The amount of queued objects which have been deleted from the DB
    num_deleted_queued_objects: usize,
    /// The amount of objects which have been queued again for statehandling
    num_requeued_objects: usize,
    /// The aggregated sqlx metrics at the last time logs had been emitted
    db_query_metrics: SqlxQueryDataAggregation,
}

impl<IO: StateControllerIO> StateProcessor<IO> {
    /// Runs the state handler task repeatedly, while waiting for the configured
    /// amount of time between runs.
    ///
    /// The controller task will continue to run until `self.cancel_token` is cancelled.
    pub(super) async fn run(mut self) {
        let dispatch_interval = self.iteration_config.processor_dispatch_interval;
        let max_jitter = (dispatch_interval.as_millis() / 3) as u64;

        while !self.cancel_token.is_cancelled() {
            let start = Instant::now();
            let jitter = if max_jitter > 0 {
                rand::rng().random::<u64>() % max_jitter
            } else {
                0
            };
            let iteration_time = dispatch_interval.saturating_add(Duration::from_millis(jitter));
            let mut next_dispatch_at = start.checked_add(iteration_time).unwrap_or(start);

            match self.run_single_iteration(iteration_time, true).await {
                Ok(result) => {
                    // If any task completed, then there's a chance we can dispatch more tasks
                    // Therefore run another iteration without backoff
                    if result.num_completed_tasks > 1 {
                        next_dispatch_at = start;
                    }
                }
                Err(err) => {
                    tracing::error!(controller=IO::LOG_SPAN_CONTROLLER_NAME, error = %err, "State processor iteration error")
                }
            }

            // The iteration might not have used up all of dispatch_interval in case
            // all dispatched tasks finished earlier. In this case we wait the configured
            // time before the next dispatch.
            self.cancel_token
                .run_until_cancelled(tokio::time::sleep_until(next_dispatch_at.into()))
                .await;
        }

        tracing::info!(
            controller = IO::LOG_SPAN_CONTROLLER_NAME,
            "State processor stop was requested"
        );

        self.drain().await;
    }

    /// Calculates how many additional object handling tasks can be spawned
    fn remaining_capacity(&self) -> usize {
        self.iteration_config
            .max_concurrency
            .saturating_sub(self.object_tasks.len())
    }

    /// Performs a single state processor iteration.
    /// The iteration will dispatch as many object handling tasks as possible,
    /// and then wait for the specified time for as many completions as possible.
    pub(super) async fn run_single_iteration(
        &mut self,
        max_completion_wait_time: std::time::Duration,
        allow_requeue: bool,
    ) -> Result<SingleIterationResult, IterationError> {
        let span_id = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));
        let iteration_span = tracing::span!(
            parent: None,
            tracing::Level::INFO,
            "state_processor_iteration",
            span_id,
            carbide.trace_root = true,
            controller = IO::LOG_SPAN_CONTROLLER_NAME,
            component = IO::LOG_SPAN_CONTROLLER_NAME,
        );

        async {
            let run_metrics = ProcessorIterationMetrics::default();

            let num_dispatched_tasks = self.dequeue_and_dispatch_object_handling_tasks().await?;
            // We are assuming that we dispatch as many tasks that are available and fit into
            // the queue. Therefore its ok to wait until at least one task has been dequeued
            // before evaluating any next steps.
            let num_completed_tasks = self
                .wait_and_process_object_handling_task_completions(
                    max_completion_wait_time,
                    allow_requeue,
                )
                .await;

            // Delete the DB entries for tasks which finished in `wait_and_process_all_object_tasks`.
            self.finalize_completed_objects().await?;

            self.emit_metrics_if_necessary();
            self.emit_periodic_log_if_necessary();

            if let Some(emitter) = self.metric_emitter.as_ref() {
                emitter.emit_run_counters_and_histograms(&run_metrics);
            }

            Ok(SingleIterationResult {
                num_dispatched_tasks,
                num_completed_tasks,
            })
        }
        .instrument(iteration_span.clone())
        .await
    }

    /// Periodically calculates aggregate metrics for all objects that have been processed
    /// and makes them available for the metrics subsystem.
    fn emit_metrics_if_necessary(&mut self) {
        // Aggregate metrics are only recalculated in certain periods to reduce load
        let now = std::time::Instant::now();
        if now
            < self
                .last_metric_emission_time
                .checked_add(self.iteration_config.metric_emission_interval)
                .unwrap_or(now)
        {
            return;
        }

        self.emit_metrics();
    }

    /// Calculates aggregate metrics for all objects that have been processed
    /// and makes them available for the metrics subsystem.
    /// Evicts metrics for objects that are older than the desired hold period
    pub(super) fn emit_metrics(&mut self) {
        let now = std::time::Instant::now();
        self.last_metric_emission_time = now;

        let mut aggregate = IterationMetrics::<IO>::default();
        for object_metrics in self.object_metrics.values() {
            aggregate.merge_object_handling_metrics(&object_metrics.metrics);
        }

        // Remove metrics that have not been refreshed for the configured time
        let min_age = now
            .checked_sub(self.iteration_config.metric_hold_time)
            .unwrap_or(now);
        self.object_metrics
            .retain(|_object_id, metrics| metrics.collected_at >= min_age);

        emit_object_metrics_as_log(&aggregate);

        self.metric_holder
            .last_iteration_specific_metrics
            .update(aggregate.specific);
        self.metric_holder
            .last_iteration_common_metrics
            .update(aggregate.common);
    }

    fn emit_periodic_log_if_necessary(&mut self) {
        let now = std::time::Instant::now();
        if now
            < self
                .last_log_time
                .checked_add(self.iteration_config.processor_log_interval)
                .unwrap_or(now)
        {
            return;
        }

        self.last_log_time = now;
        let db_query_metrics = sqlx_query_tracing::fetch_and_update_current_span_attributes();

        let stats = std::mem::take(&mut self.stats_since_last_log);
        let db_metrics_since_last_query = db_query_metrics.diff(&stats.db_query_metrics);
        self.stats_since_last_log.db_query_metrics = db_query_metrics;

        if let Some(emitter) = self.metric_emitter.as_ref() {
            emitter.emit_db_ops_metrics(&db_metrics_since_last_query, IO::LOG_SPAN_CONTROLLER_NAME);
        }

        tracing::info!(
            controller = IO::LOG_SPAN_CONTROLLER_NAME,
            in_flight_task_count = self.object_tasks.len(),
            completed_task_count = stats.num_completed_tasks,
            dispatched_task_count = stats.num_dispatched_tasks,
            requeued_object_count = stats.num_requeued_objects,
            errored_task_count = stats.num_errored_tasks,
            sql_query_count = db_metrics_since_last_query.num_queries,
            sql_affected_row_count = db_metrics_since_last_query.total_rows_affected,
            sql_returned_row_count = db_metrics_since_last_query.total_rows_returned,
            sql_total_query_duration_microseconds =
                db_metrics_since_last_query.total_query_duration.as_micros(),
            "state_processor",
        );
    }

    /// Processes and returns all finished object handling tasks. Will wait until at least one
    /// task is completed, if all of them are still running.
    ///
    /// This function waits on a *single* task, rather than every task, so that in the common case,
    /// it does not need to block at all, but simply reports on what tasks have completed since the
    /// last iteration. For example:
    ///
    /// - Iteration 1: 100 tasks are running, wait for the first one to finish, return only that one
    /// - Iteration 2: 1 new task is dequeued (bringing the total to 100 again), 50 of the tasks are
    //    now complete from the last iteration. Return 50 completed tasks, not blocking.
    /// - Iteration 3: 50 new tasks are dequeued, another 25 tasks are now completed from iteration
    ///   2. Return 25 completed tasks, not blocking.
    /// - Iteration 4: 25 tasks are enqueued, another 25 tasks are completed from iteration 3.
    ///   Return 25 completed tasks, not blocking, etc.
    ///
    /// If we waited for every task in iteration 2, it would take longer for iteration 3 to begin,
    /// and tasks would have to wait longer to be dequeued.
    ///
    /// The function will return if all in-flight tasks have been completed and additional waiting
    /// is unnecessary. In addition, `max_duration` can be used to specify how long to wait for
    /// the first completion.
    ///
    /// Returns the amount of tasks completed.
    async fn wait_and_process_object_handling_task_completions(
        &mut self,
        max_duration: std::time::Duration,
        allow_requeue: bool,
    ) -> usize {
        let total_completions =
            match tokio::time::timeout(max_duration, self.object_tasks.join_many()).await {
                Ok(results) => {
                    let now = Instant::now();
                    let count = results.len();
                    for result in results {
                        self.process_object_handling_task_result(result, allow_requeue, now);
                    }
                    count
                }
                Err(_) => {
                    tracing::error!(
                        in_flight_task_count = self.object_tasks.len(),
                        "Timed out waiting for state controller object handling tasks to complete"
                    );
                    0
                }
            };

        if total_completions > 0
            && let Some(emitter) = &self.metric_emitter
        {
            emitter
                .completed_tasks_counter
                .add(total_completions as u64, &[]);
        }

        total_completions
    }

    /// Determines how many additional state handling tasks can be processed,
    /// dequeues up to that amount from the global queue, and sends the task up to processing.
    async fn dequeue_and_dispatch_object_handling_tasks(
        &mut self,
    ) -> Result<usize, IterationError> {
        // Determine how many new objects can still be processed and dequeue that amount
        let capacity = self.remaining_capacity();
        let objects = if capacity > 0 {
            // Acquire new object handling tasks
            // If processing of an object was already start by another state controller
            // but not committed, it can be acquired after a certain amount of time.
            // The time is higher than the task handling timeout on each state controller.
            // This guarantees that the task is no longer processed by the original owner.
            let capacity = capacity.min(u32::MAX as usize) as u32;
            let mut txn = self.pool.begin().await?;
            let queued = db::acquire_queued_objects(
                &mut txn,
                IO::DB_QUEUED_OBJECTS_TABLE_NAME,
                capacity,
                &self.processor_id,
                self.iteration_config.max_object_handling_time * 3,
            )
            .await?;
            txn.commit().await?;
            queued
        } else {
            Vec::new()
        };

        let objects: Vec<IO::ObjectId> = objects
            .into_iter()
            .filter_map(|object| match IO::ObjectId::from_str(&object.object_id) {
                Ok(id) => Some(id),
                Err(_) => {
                    tracing::error!(
                        controller = IO::LOG_SPAN_CONTROLLER_NAME,
                        object_id = object.object_id.as_str(),
                        "Can not convert queued object ID to IO::ObjectID format"
                    );
                    None
                }
            })
            .collect();

        let num_dispatched_tasks = objects.len();
        self.stats_since_last_log.num_dispatched_tasks += num_dispatched_tasks;

        // Send off the new objects for processing
        for object_id in objects {
            self.dispatch_object_handling_task(object_id);
        }

        if let Some(emitter) = &self.metric_emitter
            && num_dispatched_tasks > 0
        {
            emitter
                .dispatched_tasks_counter
                .add(num_dispatched_tasks as u64, &[]);
        }

        Ok(num_dispatched_tasks)
    }

    // Executes the state handling function for all objects for a single queued object
    fn dispatch_object_handling_task(&mut self, object_id: IO::ObjectId) {
        let cloned_object_id = object_id.clone();
        let pool = self.pool.clone();
        let services = self.handler_services.as_ref().clone();
        let io = self.io.clone();
        let handler = self.state_handler.clone();
        let max_object_handling_time = self.iteration_config.max_object_handling_time;
        let metrics_emitter = self.metric_holder.emitter.clone();
        let state_change_emitter = self.state_change_emitter.clone();
        let per_object_state = self.per_object_state.clone();
        self.object_tasks
            .build_task()
            .name(&format!("state_processor {object_id}"))
            .spawn(
                async move {
                    let metrics = process_object(
                        cloned_object_id.clone(),
                        pool,
                        services,
                        io,
                        handler,
                        max_object_handling_time,
                        metrics_emitter,
                        state_change_emitter,
                        per_object_state,
                    )
                    .await;

                    ObjectHandlingTaskResult {
                        object_id: cloned_object_id,
                        metrics,
                    }
                }
                .in_current_span(),
            )
            .expect("Expect task to spawn");
    }

    async fn finalize_completed_objects(&mut self) -> Result<(), DatabaseError> {
        if self.completed_objects.is_empty() {
            return Ok(());
        }

        let object_ids: Vec<String> = self
            .completed_objects
            .iter()
            .map(|id| id.to_string())
            .collect();
        let queue_objects: Vec<String> = self
            .requeue_objects
            .iter()
            .map(|id| id.to_string())
            .collect();

        let mut txn = Transaction::begin(&self.pool).await?;
        let num_deleted = db::delete_queued_objects(
            &mut txn,
            IO::DB_QUEUED_OBJECTS_TABLE_NAME,
            &object_ids,
            &self.processor_id,
        )
        .await?;
        let num_requeued =
            db::queue_objects(&mut txn, IO::DB_QUEUED_OBJECTS_TABLE_NAME, &queue_objects).await?;
        txn.commit().await?;

        test_assert!(
            object_ids.len() == num_deleted,
            "BUG: Not all objects have been deleted from the database after cleanup"
        );

        self.stats_since_last_log.num_deleted_queued_objects += num_deleted;
        self.stats_since_last_log.num_requeued_objects += num_requeued;
        if let Some(emitter) = &self.metric_emitter {
            emitter.requeued_tasks_counter.add(num_requeued as u64, &[]);
        }
        self.completed_objects.clear();
        self.requeue_objects.clear();
        Ok(())
    }

    fn process_object_handling_task_result(
        &mut self,
        task_result: ObjectHandlingTaskResult<IO>,
        allow_requeue: bool,
        collected_at: std::time::Instant,
    ) {
        // We don't remove objects from the database here but store them first
        // and remove them later in order to not forget about these in case there
        // is a transient database error
        self.completed_objects.insert(task_result.object_id.clone());
        // If the state handler returned `Transition`, then run the handler again
        // as soon as possible. An invalidated iteration also needs a fresh
        // snapshot before it can make progress.
        if allow_requeue
            && (task_result.metrics.common.next_state.is_some()
                || task_result.metrics.common.iteration_invalidated)
        {
            self.requeue_objects.insert(task_result.object_id.clone());
        }

        self.stats_since_last_log.num_completed_tasks += 1;
        if task_result.metrics.common.error.is_some() {
            self.stats_since_last_log.num_errored_tasks += 1;
            if let Some(emitter) = &self.metric_emitter {
                emitter.errored_tasks_counter.add(1, &[]);
            }
        }

        self.object_metrics.insert(
            task_result.object_id.clone(),
            CollectedMetrics {
                metrics: task_result.metrics,
                collected_at,
            },
        );
    }

    /// Waits for all work already claimed by this processor and finalizes its
    /// queue entries. No new queue rows are acquired after draining begins.
    async fn drain(&mut self) {
        tracing::info!(
            controller = IO::LOG_SPAN_CONTROLLER_NAME,
            in_flight_task_count = self.object_tasks.len(),
            "State processor draining"
        );

        // wait_and_process_object_handling_task_completions returns all completed tasks, returns
        // once a single task is complete. Run it until all tasks are done. Nothing can enqueue
        // items to self.object_tasks while this is happening (we're borrowing &mut self), so this
        // is guaranteed to terminate eventually.
        while !self.object_tasks.is_empty() {
            self.wait_and_process_object_handling_task_completions(Duration::MAX, true)
                .await;
        }

        if let Err(error) = self.finalize_completed_objects().await {
            // This only happens if there's a database error, nothing we can really do here.
            tracing::error!(
                controller = IO::LOG_SPAN_CONTROLLER_NAME,
                error = %error,
                completed_task_count = self.completed_objects.len(),
                "Failed to finalize completed objects while draining"
            );
        }

        self.emit_metrics();
        tracing::info!(
            controller = IO::LOG_SPAN_CONTROLLER_NAME,
            "State processor drained"
        );
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum IterationError {
    #[error("unable to perform database transaction: {0}")]
    TransactionError(#[from] sqlx::Error),
    #[error("unable to perform database transaction: {0}")]
    DatabaseError(#[from] DatabaseError),
    #[error("A task panicked: {0}")]
    Panic(#[from] tokio::task::JoinError),
    #[error("state handler error: {0}")]
    StateHandlerError(#[from] StateHandlerError),
}

#[allow(clippy::too_many_arguments)]
async fn process_object<IO: StateControllerIO>(
    object_id: IO::ObjectId,
    pool: sqlx::PgPool,
    mut services: <IO::ContextObjects as StateHandlerContextObjects>::Services,
    io: Arc<IO>,
    handler: Arc<
        dyn StateHandler<
                State = IO::State,
                ControllerState = IO::ControllerState,
                ContextObjects = IO::ContextObjects,
                ObjectId = IO::ObjectId,
            >,
    >,
    max_object_handling_time: std::time::Duration,
    metrics_emitter: Option<Arc<StateProcessorMetricEmitter<IO>>>,
    state_change_emitter: Arc<StateChangeEmitter<IO::ObjectId, IO::ControllerState>>,
    per_object_state: Option<PerObjectStateRecorder>,
) -> ObjectHandlerMetrics<IO> {
    let mut metrics = ObjectHandlerMetrics::<IO>::default();

    let start = Instant::now();

    // Note that this inner async block is required to be able to use
    // the ? operator in the inner block, and then return a `Result`
    // from the other outer block.
    let result: Result<
        Result<StateHandlerOutcome<_>, StateHandlerError>,
        tokio::time::error::Elapsed,
    > = tokio::time::timeout(max_object_handling_time, async {
        let mut txn = pool.begin().await?;
        let mut snapshot = io
            .load_object_state(&mut txn, &object_id)
            .await?
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: object_id.to_string(),
                missing: MISSING_OBJECT_STATE,
            })?;
        let controller_state = io
            .load_controller_state(&mut txn, &object_id, &snapshot)
            .await?;
        metrics.common.initial_state = Some(controller_state.value.clone());
        metrics.common.state_entered_at = Some(controller_state.version.timestamp());
        // Unwrap uses a very large duration as default to show something is wrong
        metrics.common.time_in_state = chrono::Utc::now()
            .signed_duration_since(controller_state.version.timestamp())
            .to_std()
            .unwrap_or(Duration::from_secs(60 * 60 * 24));

        let state_sla = io.state_sla(&controller_state, &snapshot);
        metrics.common.time_in_state_above_sla = state_sla.time_in_state_above_sla;
        metrics.common.sla = state_sla.sla;

        let mut pending_db_writes = DbWriteBatch::new();
        let mut ctx = StateHandlerContext {
            services: &mut services,
            metrics: &mut metrics.specific,
            pending_db_writes: &mut pending_db_writes,
        };

        // Commit the transaction now, since we don't want to leave a txn open
        // throughout handle_object_state.
        txn.commit().await?;

        let handler_output = handler
            .handle_object_state(&object_id, &mut snapshot, &controller_state.value, &mut ctx)
            .await;

        // What transaction should we use for persisting the outcome? If the
        // handler was successful and gave us back a transaction, use that,
        // otherwise make our own.
        let (handler_outcome, mut txn) = match handler_output {
            Ok(mut outcome) => {
                let mut txn = if let Some(txn) = outcome.take_transaction() {
                    txn
                } else {
                    pool.begin().await?
                };
                if let Err(e) = pending_db_writes.apply_all(&mut txn).await {
                    if matches!(e, StateHandlerError::IterationInvalidated { .. }) {
                        txn.rollback().await?;
                        return Err(e);
                    }
                    // If there's an error running the writes, count that as the handler outcome
                    (Err(e), txn)
                } else {
                    (Ok(outcome), txn)
                }
            }
            Err(e @ StateHandlerError::IterationInvalidated { .. }) => return Err(e),
            Err(e) => (Err(e), pool.begin().await?),
        };

        let mut next_state = None;
        let mut next_state_entered_at = None;
        let mut next_state_sla = None;
        if let Ok(StateHandlerOutcome::Transition {
            next_state: next, ..
        }) = &handler_outcome
        {
            next_state = Some(next.clone());

            if *next == controller_state.value {
                tracing::warn!(next_state = ?next, %object_id, "Transition to current state");
            }
            let new_version = controller_state.version.increment();
            next_state_entered_at = Some(new_version.timestamp());
            // Resolve the SLA of the state being entered, so the committing
            // iteration publishes it without a one-iteration gap.
            next_state_sla = io
                .state_sla(&Versioned::new(next.clone(), new_version), &snapshot)
                .sla;
            if io
                .persist_controller_state(
                    &mut txn,
                    &object_id,
                    controller_state.version,
                    new_version,
                    next,
                )
                .await?
            {
                io.persist_state_history(&mut txn, &object_id, new_version, next)
                    .await?;
            } else {
                // The state transition did not apply, so roll back the handler's
                // other writes too, including queued observations.
                txn.rollback().await?;
                return Err(StateHandlerError::IterationInvalidated {
                    source_ref: std::panic::Location::caller(),
                });
            }
        }

        let is_success = handler_outcome.is_ok();

        // If the state handler neither transitioned nor returned no error,
        // but the object is stuck in the state for longer than the defined SLA,
        // then transform the outcome into an error
        let handler_outcome = match handler_outcome {
            Ok(StateHandlerOutcome::Wait { reason, .. }) if state_sla.time_in_state_above_sla => {
                Err(StateHandlerError::TimeInStateAboveSla {
                    handler_outcome: format!("Wait(\"{reason}\")"),
                })
            }
            Ok(StateHandlerOutcome::DoNothing { .. }) if state_sla.time_in_state_above_sla => {
                Err(StateHandlerError::TimeInStateAboveSla {
                    handler_outcome: "DoNothing".to_string(),
                })
            }
            _ => handler_outcome,
        };

        if is_success {
            // Commit transaction only when handler returned the Success.
            if !matches!(handler_outcome, Ok(StateHandlerOutcome::Deleted { .. })) {
                let db_outcome =
                    PersistentStateHandlerOutcome::from_result(handler_outcome.as_ref());
                io.persist_outcome(&mut txn, &object_id, db_outcome).await?;
            }

            txn.commit().await.map_err(StateHandlerError::from)?;
        } else if !matches!(handler_outcome, Ok(StateHandlerOutcome::Deleted { .. })) {
            // Whatever is the reason, outcome must be stored in db.
            let _ = txn.rollback().await;
            let mut txn = pool.begin().await?;
            let db_outcome = PersistentStateHandlerOutcome::from_result(handler_outcome.as_ref());
            io.persist_outcome(&mut txn, &object_id, db_outcome).await?;
            txn.commit().await.map_err(StateHandlerError::from)?;
        }

        // Only emit the next state as metric if the transaction was actually
        // committed and we are sure we reached the next state
        metrics.common.next_state = next_state;
        if metrics.common.next_state.is_some() {
            metrics.common.state_entered_at = next_state_entered_at;
            metrics.common.sla = next_state_sla;
        }

        handler_outcome
    })
    .await;
    metrics.common.handler_latency = start.elapsed();

    // Emit the state changed event to registered hooks
    if let Some(next_state) = &metrics.common.next_state {
        state_change_emitter.emit(StateChangeEvent {
            object_id: &object_id,
            #[cfg(any(test, feature = "test-support"))]
            previous_state: metrics.common.initial_state.as_ref(),
            new_state: next_state,
            timestamp: chrono::Utc::now(),
        });
    }

    // Emit the object handling metrics for this state handler invocation
    if let Some(emitter) = metrics_emitter {
        emitter.emit_object_counters_and_histograms(&metrics);
    }

    let deleted = matches!(&result, Ok(Ok(StateHandlerOutcome::Deleted { .. })));
    let result = match result {
        Ok(Ok(_result)) => Ok(()),
        Ok(Err(StateHandlerError::IterationInvalidated { source_ref })) => {
            tracing::info!(%object_id, %source_ref, "Controller iteration invalidated");
            metrics.common.iteration_invalidated = true;
            Ok(())
        }
        Ok(Err(err)) => Err(err),
        Err(_timeout) => Err(StateHandlerError::Timeout {
            object_id: object_id.to_string(),
            state: metrics
                .common
                .initial_state
                .as_ref()
                .map(|state| format!("{state:?}"))
                .unwrap_or_default(),
        }),
    };
    if let Err(e) = result {
        tracing::warn!(%object_id, initial_state = ?metrics.common.initial_state, error = ?e, "State handler error");
        metrics.common.error = Some(e);
    }

    // Record the object's state as per-object gauges: the committed
    // post-transition state when this iteration transitioned, the observed
    // state otherwise. Objects that no longer exist (deleted, or their state
    // vanished from the database) have their series removed instead of
    // lingering until hold-period eviction.
    let object_gone = deleted
        || matches!(
            &metrics.common.error,
            Some(StateHandlerError::MissingData {
                missing: MISSING_OBJECT_STATE,
                ..
            })
        );
    if let Some(recorder) = &per_object_state {
        if object_gone {
            recorder.clear(&object_id.to_string());
        } else if !metrics.common.iteration_invalidated
            && let (Some(final_state), Some(entered)) = (
                metrics
                    .common
                    .next_state
                    .as_ref()
                    .or(metrics.common.initial_state.as_ref()),
                metrics.common.state_entered_at,
            )
        {
            let (state, substate) = IO::metric_state_names(final_state);
            let manual_intervention = match (
                IO::manual_intervention_reason(final_state),
                &metrics.common.error,
            ) {
                (Some(reason), _) => ManualIntervention::Required(reason),
                (None, Some(error @ StateHandlerError::ManualInterventionRequired(_))) => {
                    ManualIntervention::Required(error.metric_label())
                }
                // An SLA breach is a symptom, not an intervention trigger, and
                // does not obscure the object's status.
                (None, None) | (None, Some(StateHandlerError::TimeInStateAboveSla { .. })) => {
                    ManualIntervention::NotRequired
                }
                // Any other error leaves the status undetermined this
                // iteration: keep an existing series alive instead of
                // flapping a stuck object out of alerts.
                (None, Some(_)) => ManualIntervention::Unknown,
            };
            recorder.record(
                &object_id.to_string(),
                state,
                substate,
                entered,
                metrics.common.sla,
                manual_intervention,
            );
        } else {
            // The state could not be loaded, or a conditional write rejected
            // this iteration. A prerequisite rejection need not mean the
            // object's own state changed. Keep existing series alive until
            // the requeued/next pass can record a fresh observation.
            recorder.touch(&object_id.to_string());
        }
    }

    metrics
}

#[derive(Debug)]
pub(super) struct ProcessorMetricsEmitter {
    iteration_latency: Histogram<f64>,
    dispatched_tasks_counter: Counter<u64>,
    completed_tasks_counter: Counter<u64>,
    requeued_tasks_counter: Counter<u64>,
    errored_tasks_counter: Counter<u64>,
    db: sqlx_query_tracing::DatabaseMetricEmitters,
}

impl ProcessorMetricsEmitter {
    pub(super) fn new(object_type: &str, meter: &Meter) -> Self {
        let db = sqlx_query_tracing::DatabaseMetricEmitters::new(meter);

        let iteration_latency = meter
            .f64_histogram(format!("{object_type}_iteration_latency"))
            .with_description(format!(
                "The elapsed time in the last state processor iteration to handle objects of type {object_type}"
            ))
            .with_unit("ms")
            .build();

        let dispatched_tasks_counter = meter
            .u64_counter(format!("{object_type}_object_tasks_dispatched"))
            .with_description(format!(
                "Number of object handling tasks dequeued and dispatched for processing for objects of type {object_type}"
            ))
            .build();

        let completed_tasks_counter = meter
            .u64_counter(format!("{object_type}_object_tasks_completed"))
            .with_description(format!(
                "Number of object handling tasks completed for objects of type {object_type}"
            ))
            .build();

        let requeued_tasks_counter = meter
            .u64_counter(format!("{object_type}_object_tasks_requeued"))
            .with_description(format!(
                "Number of object handling tasks requeued for objects of type {object_type}"
            ))
            .build();

        let errored_tasks_counter = meter
            .u64_counter(format!("{object_type}_object_tasks_errored"))
            .with_description(format!(
                "Number of object handling tasks completed with an error for objects of type {object_type}"
            ))
            .build();

        Self {
            iteration_latency,
            db,
            dispatched_tasks_counter,
            completed_tasks_counter,
            requeued_tasks_counter,
            errored_tasks_counter,
        }
    }

    fn emit_db_ops_metrics(
        &self,
        db_metrics: &sqlx_query_tracing::SqlxQueryDataAggregation,
        log_span_name: &str,
    ) {
        // We use an attribute to distinguish the query counter from the
        // ones that are used for other state controller and for gRPC requests
        let attrs = &[KeyValue::new("operation", log_span_name.to_string())];
        self.db.emit(db_metrics, attrs);
    }

    fn emit_run_counters_and_histograms(&self, run_metrics: &ProcessorIterationMetrics) {
        self.iteration_latency.record(
            1000.0 * run_metrics.iteration_started_at.elapsed().as_secs_f64(),
            &[],
        );
    }
}

/// Emits the aggregate metrics that had been collected as a single log line.
fn emit_object_metrics_as_log<IO: StateControllerIO>(iteration_metrics: &IterationMetrics<IO>) {
    let mut total_objects = 0;
    let mut states: HashMap<String, usize> = HashMap::new();
    let mut states_above_sla: HashMap<String, usize> = HashMap::new();
    let mut error_types: HashMap<String, HashMap<String, usize>> = HashMap::new();
    for (full_state, state_metrics) in iteration_metrics.common.state_metrics.iter() {
        total_objects += state_metrics.num_objects;

        let full_state_name = if !full_state.substate.is_empty() {
            format!("{}.{}", full_state.state, full_state.substate)
        } else {
            full_state.state.to_string()
        };

        for (error, &count) in state_metrics.handling_errors_per_type.iter() {
            *error_types
                .entry(full_state_name.clone())
                .or_default()
                .entry(error.to_string())
                .or_default() += count;
        }

        states.insert(full_state_name.clone(), state_metrics.num_objects);
        if state_metrics.num_objects_above_sla > 0 {
            states_above_sla.insert(full_state_name.clone(), state_metrics.num_objects_above_sla);
        }
    }

    let states = serde_json::to_string(&states).unwrap_or_else(|_| "{}".to_string());
    let states_above_sla =
        serde_json::to_string(&states_above_sla).unwrap_or_else(|_| "{}".to_string());
    let error_types = serde_json::to_string(&error_types).unwrap_or_else(|_| "{}".to_string());

    tracing::info!(
        name: "state_controller_object_metrics",
        controller = IO::LOG_SPAN_CONTROLLER_NAME,
        total_object_count = total_objects,
        %states,
        %states_above_sla,
        %error_types,
        "State controller object metrics"
    );
}

trait JoinMany<T> {
    /// Get all completed futures in this [`JoinSet`], blocking until at least one future completes.
    /// Differs from [`JoinSet::join_next`] in that it will return all completed futures, not just
    /// one. Differs from [`JoinSet::join_all`] in that it will wait for a maximum of one future to
    /// complete, returning even if other futures in the set are still running.
    ///
    /// It will also panic on any [`JoinError`], identically to how [`JoinSet::join_all`] behaves.
    fn join_many(&mut self) -> impl Future<Output = Vec<T>>;
}

impl<T: 'static> JoinMany<T> for JoinSet<T> {
    async fn join_many(&mut self) -> Vec<T> {
        let unwrap_join_error = |result: Option<Result<T, JoinError>>| match result {
            Some(Ok(result)) => Some(result),
            Some(Err(error)) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            // Safety: this only happens if the task was aborted with [`JoinHandle::abort`] or
            // [`JoinSet::abort_all`], in which case we should panic.
            Some(Err(error)) => panic!("{error}"),
            None => None,
        };

        if let Some(result) = unwrap_join_error(self.join_next().await) {
            let mut results = vec![result];
            while let Some(result) = unwrap_join_error(self.try_join_next()) {
                results.push(result);
            }
            results
        } else {
            vec![]
        }
    }
}
