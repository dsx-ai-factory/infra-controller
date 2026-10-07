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

use ::db::DatabaseError;
use ::db::work_lock_manager::WorkLock;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::controller::periodic_enqueuer::PeriodicEnqueuer;
use crate::io::StateControllerIO;
use crate::state_handler::StateHandlerError;

mod builder;
pub mod db;
mod enqueuer;
pub use enqueuer::Enqueuer;

pub mod periodic_enqueuer;
pub mod processor;

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct ControllerIterationId(pub i64);

/// Metadata for a single state controller iteration
#[derive(Debug, Clone)]
pub struct ControllerIteration {
    /// The ID of the iteration
    pub id: ControllerIterationId,
    /// When the iteration started
    #[allow(dead_code)]
    pub started_at: DateTime<Utc>,
}

pub struct LockedControllerIteration {
    pub iteration_data: ControllerIteration,
    /// The lock for the work done in this iteration.
    pub _work_lock: WorkLock,
}

impl<'r> FromRow<'r, PgRow> for ControllerIteration {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let id: i64 = row.try_get("id")?;
        let started_at = row.try_get("started_at")?;
        Ok(ControllerIteration {
            id: ControllerIterationId(id),
            started_at,
        })
    }
}

/// Metadata for a single state controller iteration
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedObject {
    /// The ID of the object which should get scheduled
    pub object_id: String,
    /// Identifies the processor which is executing the state handler
    /// The value of this field will be NULL in case the object is not yet processed
    pub processed_by: Option<String>,
}

impl<'r> FromRow<'r, PgRow> for QueuedObject {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let object_id = row.try_get("object_id")?;
        let processed_by: Option<String> = row.try_get("processed_by")?;
        Ok(QueuedObject {
            object_id,
            processed_by,
        })
    }
}

/// The object static controller evaluates the current state of all objects of a
/// certain type in a Forge site, and decides which actions the system should
/// undertake to bring the state inline with the state users requested.
///
/// Each Forge API server is running a StateController instance for each object type.
/// While all instances run in parallel, the StateController uses internal
/// synchronization to make sure that inside a single site - only a single controller
/// will decide the next step for a single object.
pub struct StateController<IO: StateControllerIO> {
    enqueuer: PeriodicEnqueuer<IO>,
    processor: processor::StateProcessor<IO>,
}

impl<IO: StateControllerIO> StateController<IO> {
    /// Returns a [`Builder`] for configuring `StateController`
    pub fn builder() -> builder::Builder<IO> {
        builder::Builder::default()
    }

    /// Enqueues state handling tasks for all objects and processes them
    #[cfg(any(test, feature = "test-support"))]
    pub async fn run_single_iteration(&mut self) {
        self.run_single_iteration_ext(true).await
    }

    /// Enqueues state handling tasks for all objects and processes them
    #[cfg(any(test, feature = "test-support"))]
    pub async fn run_single_iteration_ext(&mut self, allow_requeue: bool) {
        // Delete stale object metrics - e.g. from predicted hosts
        // They make assertions on actually still valid metrics more tricky
        self.processor.object_metrics.clear();

        let _enqueuer_result = self.enqueuer.run_single_iteration().await;
        loop {
            // Prevent metric emission - we only emit them a single time after the loop
            // That cuts down the noise in tests
            self.processor.last_metric_emission_time = std::time::Instant::now();

            if let Err(err) = self
                .processor
                .run_single_iteration(std::time::Duration::MAX, allow_requeue)
                .await
            {
                tracing::error!(error = %err, "State processor iteration error");
            }
            if self.processor.object_tasks.is_empty() {
                break;
            }
        }
        // Immediately emit the latest set of metrics
        self.processor.emit_metrics();
    }
}

#[derive(Debug, thiserror::Error)]
enum IterationError {
    #[error("unable to perform database transaction: {0}")]
    TransactionError(#[from] sqlx::Error),
    #[error("unable to perform database transaction: {0}")]
    DatabaseError(#[from] DatabaseError),
    #[error("unable to acquire lock and start iteration")]
    LockError,
    #[error("A task panicked: {0}")]
    Panic(#[from] tokio::task::JoinError),
    #[error("state handler error: {0}")]
    StateHandlerError(#[from] StateHandlerError),
}
