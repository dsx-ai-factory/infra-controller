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

use std::collections::HashSet;
use std::sync::Arc;

use db::work_lock_manager::WorkLockManagerHandle;
use opentelemetry::metrics::Meter;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::config::IterationConfig;
use crate::controller::StateController;
use crate::controller::periodic_enqueuer::{EnqueuerMetricsEmitter, PeriodicEnqueuer};
use crate::controller::processor::{ProcessorMetricsEmitter, StateProcessor};
use crate::io::StateControllerIO;
use crate::metrics::MetricHolder;
use crate::per_object::PerObjectStateRecorder;
use crate::state_change_emitter::StateChangeEmitter;
use crate::state_handler::{NoopStateHandler, StateHandler, StateHandlerContextObjects};

/// The return value of `[Builder::build_internal]`
struct BuildOrSpawn<IO: StateControllerIO> {
    controller_name: String,
    controller: StateController<IO>,
}

#[derive(Debug, thiserror::Error)]
pub enum StateControllerBuildError {
    #[error("missing parameter {0}")]
    MissingArgument(&'static str),

    #[error("task spawn error: {0}")]
    IOError(#[from] std::io::Error),
}

/// A builder for `StateController`
pub struct Builder<IO: StateControllerIO> {
    database: Option<sqlx::PgPool>,
    work_lock_manager_handle: Option<WorkLockManagerHandle>,
    iteration_config: IterationConfig,
    object_type_for_metrics: Option<String>,
    meter: Option<Meter>,
    io: Option<Arc<IO>>,
    state_handler: Arc<
        dyn StateHandler<
                State = IO::State,
                ControllerState = IO::ControllerState,
                ContextObjects = IO::ContextObjects,
                ObjectId = IO::ObjectId,
            >,
    >,
    services: Option<Arc<<IO::ContextObjects as StateHandlerContextObjects>::Services>>,
    state_change_emitter: Arc<StateChangeEmitter<IO::ObjectId, IO::ControllerState>>,
    processor_id: Option<String>,
    per_object_state_metrics: Option<PerObjectStateRecorder>,
}

impl<IO: StateControllerIO> Default for Builder<IO> {
    /// Creates a new `Builder`
    fn default() -> Self {
        Self {
            database: None,
            work_lock_manager_handle: None,
            iteration_config: IterationConfig::default(),
            io: None,
            state_handler: Arc::new(NoopStateHandler::<
                IO::ObjectId,
                IO::State,
                IO::ControllerState,
                IO::ContextObjects,
            >::default()),
            meter: None,
            object_type_for_metrics: None,
            services: None,
            state_change_emitter: Arc::new(StateChangeEmitter::default()),
            processor_id: None,
            per_object_state_metrics: None,
        }
    }
}

impl<IO: StateControllerIO> Builder<IO> {
    /// Builds a [`StateController`] with all configured options with the intention
    /// of calling the `run_single_iteration` whenever required
    #[cfg(any(test, feature = "test-support"))]
    pub fn build_for_manual_iterations(
        self,
        cancel_token: CancellationToken,
    ) -> Result<StateController<IO>, StateControllerBuildError> {
        let build_or_spawn = self.build_internal(cancel_token)?;
        Ok(build_or_spawn.controller)
    }

    /// Builds a [`StateController`] with all configured options
    /// and spawns the state controller as background task.
    ///
    /// The state controller will continue to run as long as the returned `StateControllerHandle`
    /// is kept alive.
    pub fn build_and_spawn(
        self,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> Result<(), StateControllerBuildError> {
        let build_or_spawn = self.build_internal(cancel_token)?;

        join_set
            .build_task()
            .name(&format!(
                "state_controller_periodic_enqueuer {}",
                build_or_spawn.controller_name
            ))
            .spawn(async move { build_or_spawn.controller.enqueuer.run().await })?;

        join_set
            .build_task()
            .name(&format!(
                "state_processor {}",
                build_or_spawn.controller_name
            ))
            .spawn(async move { build_or_spawn.controller.processor.run().await })?;
        Ok(())
    }

    /// Builds a [`StateController`] with all configured options
    fn build_internal(
        mut self,
        cancel_token: CancellationToken,
    ) -> Result<BuildOrSpawn<IO>, StateControllerBuildError> {
        let database = self
            .database
            .take()
            .ok_or(StateControllerBuildError::MissingArgument("database"))?;

        let processor_id = self
            .processor_id
            .take()
            .ok_or(StateControllerBuildError::MissingArgument("processor_id"))?;

        let object_type_for_metrics = self.object_type_for_metrics.take();
        let meter = self.meter.take();

        if self.iteration_config.max_concurrency == 0 {
            return Err(StateControllerBuildError::MissingArgument(
                "max_concurrency",
            ));
        }
        let controller_name = object_type_for_metrics.unwrap_or_else(|| "undefined".to_string());

        let services = self
            .services
            .take()
            .ok_or(StateControllerBuildError::MissingArgument("services"))?;

        // This defines the shared storage location for metrics between the state handler
        // and the OTEL framework
        let metric_holder = Arc::new(MetricHolder::new(
            meter.clone(),
            &controller_name,
            self.iteration_config.metric_hold_time,
        ));

        let work_lock_manager_handle = self.work_lock_manager_handle.take().ok_or(
            StateControllerBuildError::MissingArgument("work_lock_manager_handle"),
        )?;

        let period_enqueuer_metric_emitter = meter
            .clone()
            .map(|meter| EnqueuerMetricsEmitter::new(&controller_name, &meter));

        let per_object_state_metrics = self.per_object_state_metrics.take();

        let enqueuer = PeriodicEnqueuer::<IO> {
            pool: database.clone(),
            work_lock_manager_handle,
            cancel_token: cancel_token.clone(),
            metric_emitter: period_enqueuer_metric_emitter,
            iteration_config: self.iteration_config,
            io: self.io.clone().unwrap_or_default(),
            per_object_state: per_object_state_metrics.clone(),
            known_object_ids: Default::default(),
            pending_clears: Default::default(),
        };

        let processor_metric_emitter =
            meter.map(|meter| ProcessorMetricsEmitter::new(&controller_name, &meter));

        let processor = StateProcessor::<IO> {
            pool: database,
            cancel_token,
            iteration_config: self.iteration_config,
            handler_services: services,
            io: self.io.unwrap_or_default(),
            state_handler: self.state_handler.clone(),
            metric_emitter: processor_metric_emitter,
            metric_holder,
            per_object_state: per_object_state_metrics,
            state_change_emitter: self.state_change_emitter,
            object_tasks: JoinSet::new(),
            completed_objects: HashSet::new(),
            requeue_objects: HashSet::new(),
            object_metrics: Default::default(),
            last_log_time: std::time::Instant::now(),
            stats_since_last_log: Default::default(),
            last_metric_emission_time: std::time::Instant::now(),
            processor_id,
        };

        let controller = StateController::<IO> {
            processor,
            enqueuer,
        };

        Ok(BuildOrSpawn {
            controller,
            controller_name,
        })
    }

    /// Configures the utilized database, and the singleton work_lock_manager that corresponds to it
    pub fn database(
        mut self,
        db: sqlx::PgPool,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        self.database = Some(db);
        self.work_lock_manager_handle = Some(work_lock_manager_handle);
        self
    }

    /// Configures the ID of this state controller. The ID needs to be unique
    /// within a Carbide deployment. Hostnames can be used as processor IDs
    /// in case its guaranteed that a host never runs more than a single processor.
    /// K8S POD IDs will naturally satisfy the requirement.
    pub fn processor_id(mut self, processor_id: String) -> Self {
        self.processor_id = Some(processor_id);
        self
    }

    /// Configures the services that will be available within the StateHandlerContext
    pub fn services(
        mut self,
        services: Arc<<IO::ContextObjects as StateHandlerContextObjects>::Services>,
    ) -> Self {
        self.services = Some(services);
        self
    }

    /// Configures the Meter that will be used for emitting metrics
    pub fn meter(mut self, object_type_for_metrics: impl Into<String>, meter: Meter) -> Self {
        self.object_type_for_metrics = Some(object_type_for_metrics.into());
        self.meter = Some(meter);
        self
    }

    /// Configures how the state controller performs iterations
    pub fn iteration_config(mut self, config: IterationConfig) -> Self {
        self.iteration_config = config;
        self
    }

    /// Sets the IO handler configuration
    pub fn io(mut self, io: Arc<IO>) -> Self {
        self.io = Some(io);
        self
    }

    /// Sets the function that will be called to advance the state of a single object
    pub fn state_handler(
        mut self,
        handler: Arc<
            dyn StateHandler<
                    State = IO::State,
                    ControllerState = IO::ControllerState,
                    ContextObjects = IO::ContextObjects,
                    ObjectId = IO::ObjectId,
                >,
        >,
    ) -> Self {
        self.state_handler = handler;
        self
    }

    /// Enables per-object state progress metrics: every processed object's
    /// current state, SLA, and manual-intervention status are recorded under
    /// the recorder's object type.
    pub fn per_object_state_metrics(mut self, recorder: Option<PerObjectStateRecorder>) -> Self {
        self.per_object_state_metrics = recorder;
        self
    }

    /// Sets the state change emitter for broadcasting state transitions to hooks
    pub fn state_change_emitter(
        mut self,
        emitter: StateChangeEmitter<IO::ObjectId, IO::ControllerState>,
    ) -> Self {
        self.state_change_emitter = Arc::new(emitter);
        self
    }
}
