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

//! State Controller IO implementation for Infiniband Partitions

use carbide_uuid::infiniband::IBPartitionId;
use config_version::{ConfigVersion, Versioned};
use db::{self, ConditionalWrite, ControllerStateNotCurrent, DatabaseError, ObjectColumnFilter};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::ib_partition::{self, IBPartition, IBPartitionControllerState};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;
use state_controller::metrics::NoopMetricsEmitter;

use crate::context::IBPartitionStateHandlerContextObjects;

/// State Controller IO implementation for Infiniband Partitions
#[derive(Default, Debug)]
pub struct IBPartitionStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for IBPartitionStateControllerIO {
    type ObjectId = IBPartitionId;
    type State = IBPartition;
    type ControllerState = IBPartitionControllerState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = IBPartitionStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "ib_partition_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "ib_partition_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "ib_partition_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db::ib_partition::list_segment_ids(txn).await
    }

    /// Loads a state snapshot from the database
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        partition_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        let mut partitions = db::ib_partition::find_by(
            txn,
            ObjectColumnFilter::One(db::ib_partition::IdColumn, partition_id),
        )
        .await?;
        if partitions.is_empty() {
            return Ok(None);
        } else if partitions.len() != 1 {
            return Err(DatabaseError::new(
                "IBPartition::find()",
                sqlx::Error::Decode(
                    eyre::eyre!(
                        "searching for IBPartition {} returned multiple results",
                        partition_id
                    )
                    .into(),
                ),
            ));
        }
        let partition = partitions.swap_remove(0);
        Ok(Some(partition))
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
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        db::ib_partition::try_update_controller_state(
            txn,
            *object_id,
            old_version,
            new_version,
            new_state,
        )
        .await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        db::state_history::persist(
            txn,
            db::state_history::StateHistoryTableId::IbPartition,
            object_id,
            new_state,
            new_version,
        )
        .await?;
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db::ib_partition::update_controller_state_outcome(txn, *object_id, outcome).await
    }

    fn metric_state_names(state: &IBPartitionControllerState) -> (&'static str, &'static str) {
        match state {
            IBPartitionControllerState::Provisioning => ("provisioning", ""),
            IBPartitionControllerState::Ready => ("ready", ""),
            IBPartitionControllerState::Error { .. } => ("error", ""),
            IBPartitionControllerState::Deleting => ("deleting", ""),
        }
    }

    fn manual_intervention_reason(state: &Self::ControllerState) -> Option<&'static str> {
        // The stored cause is free text, so the reason is a fixed token.
        matches!(state, IBPartitionControllerState::Error { .. }).then_some("error")
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        ib_partition::state_sla(&state.value, &state.version)
    }
}
