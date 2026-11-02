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
use config_version::{ConfigVersion, Versioned};
use db::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use sqlx::PgConnection;

use crate::metrics::MetricsEmitter;
use crate::state_handler::StateHandlerContextObjects;

/// This trait defines on what objects a state controller instance will act,
/// and how it loads the objects state.
#[async_trait::async_trait]
pub trait StateControllerIO: Send + Sync + std::fmt::Debug + 'static + Default {
    /// Uniquely identifies the object that is controlled
    /// The type needs to be convertible into a String
    type ObjectId: std::fmt::Display
        + std::fmt::Debug
        + std::str::FromStr
        + PartialEq
        + Eq
        + std::hash::Hash
        + Send
        + Sync
        + 'static
        + Clone;
    /// The full state of the object.
    /// This might contain all kinds of information, which different pieces of the full
    /// state being updated by various components.
    type State: Send + Sync + 'static;
    /// This defines the state that the state machine implemented in the state handler
    /// actively acts upon. It is passed via the `controller_state` parameter to
    /// each state handler, and can be modified via this parameter.
    /// This state may not be updated by any other component.
    type ControllerState: std::fmt::Debug + Send + Sync + 'static + Clone + Eq;
    /// Defines how metrics that are specific to this kind of object are handled
    type MetricsEmitter: MetricsEmitter;
    /// The collection of generic objects which are referenced in StateHandlerContext
    type ContextObjects: StateHandlerContextObjects<
        ObjectMetrics = <Self::MetricsEmitter as MetricsEmitter>::ObjectMetrics,
    >;

    /// The name of the table in the database that will be used to generate run IDs
    /// The table will be locked whenever a new iteration is started
    const DB_ITERATION_ID_TABLE_NAME: &'static str;

    /// The name of the table in the database that will be used to enqueue objects
    /// within a certain iteration.
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str;

    /// The name that will be used for the logging span created by the State Controller
    const LOG_SPAN_CONTROLLER_NAME: &'static str;

    /// Resolves the list of objects that the state controller should act upon
    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError>;

    /// Loads a state of an object
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError>;

    /// Loads the object state that is owned by the state controller
    async fn load_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError>;

    /// Persists the object state that is owned by the state controller.
    ///
    /// `old_version` is the current version (used in the WHERE clause for
    /// optimistic locking). `new_version` is the incremented version to store.
    /// Both are computed by the processor so that implementations do not need
    /// to call `.increment()` themselves.
    ///
    /// Returns `Applied(())` when the state is persisted in the caller's
    /// transaction. A missing object, changed version, or unmet persistence
    /// condition returns `NotApplied(ControllerStateNotCurrent)`; database
    /// failures remain errors. The processor records history only after an
    /// applied write and rolls back the iteration's writes on rejection.
    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError>;

    /// Persists a state history record for debugging and audit purposes.
    ///
    /// Called by the processor after each successful state transition
    /// (i.e. when `persist_controller_state` returns `Applied(())`).
    /// `new_version` is the version that was just written by
    /// `persist_controller_state`.
    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError>;

    /// Save the result of the most recent controller iteration
    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError>;

    /// Returns the names that should be used in metrics for a given object state
    /// The first returned value is the value that will be used for the main `state`
    /// attribute on each metric. The 2nd value - if not empty - will be used for
    /// an optional substate attribute.
    fn metric_state_names(state: &Self::ControllerState) -> (&'static str, &'static str);

    /// Returns a stable reason token when the state itself signals that the
    /// object requires manual operator action (e.g. a terminal failure state),
    /// for the per-object manual-intervention metric. Values must come from a
    /// closed set — never free-text errors.
    fn manual_intervention_reason(_state: &Self::ControllerState) -> Option<&'static str> {
        None
    }

    /// Defines whether an object is in a certain state for longer than allowed
    /// by the SLA and returns the SLA.
    ///
    /// If an object stays in a state for longer than expected, a metric will
    /// be emitted.
    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        object_state: &Self::State,
    ) -> StateSla;
}
