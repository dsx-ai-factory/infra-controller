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

//! State Controller IO implementation for Racks

use carbide_rack_controller::context::RackStateHandlerContextObjects;
use carbide_rack_controller::metrics::RackMetricsEmitter;
use carbide_uuid::rack::RackId;
use config_version::{ConfigVersion, Versioned};
use db::rack::IdColumn;
use db::{
    ConditionalWrite, ControllerStateNotCurrent, DatabaseError, ObjectColumnFilter, rack as db_rack,
};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::rack::{
    Rack, RackMaintenanceState, RackSearchFilter, RackState, RackValidationState, state_sla,
};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;

use crate as carbide_rack_controller;

/// State Controller IO implementation for Racks
#[derive(Default, Debug)]
pub struct RackStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for RackStateControllerIO {
    type ObjectId = RackId;
    type State = Rack;
    type ControllerState = RackState;
    type MetricsEmitter = RackMetricsEmitter;
    type ContextObjects = RackStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "rack_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "rack_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "rack_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db_rack::find_ids(txn, RackSearchFilter::default()).await
    }

    /// Loads a state snapshot from the database
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        rack_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        let mut racks = db_rack::find_by(txn, ObjectColumnFilter::One(IdColumn, rack_id)).await?;
        if racks.is_empty() {
            return Ok(None);
        } else if racks.len() != 1 {
            return Err(DatabaseError::new(
                "Rack::find()",
                sqlx::Error::Decode(
                    eyre::eyre!("searching for rack {} returned multiple results", rack_id).into(),
                ),
            ));
        }
        let rack = racks.swap_remove(0);
        Ok(Some(rack))
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _rack_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        Ok(state.controller_state.clone())
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        rack_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        db_rack::try_update_controller_state(txn, rack_id, old_version, new_version, new_state)
            .await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        rack_id: &Self::ObjectId,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        db::state_history::persist(
            txn,
            db::state_history::StateHistoryTableId::Rack,
            rack_id,
            new_state,
            new_version,
        )
        .await?;
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        rack_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db_rack::update_controller_state_outcome(txn, rack_id, outcome).await
    }

    fn metric_state_names(state: &RackState) -> (&'static str, &'static str) {
        match state {
            RackState::Created => ("created", ""),
            RackState::Discovering => ("discovering", ""),
            RackState::Validating { validating_state } => match validating_state {
                RackValidationState::Pending => ("validation", "pending"),
                RackValidationState::InProgress { .. } => ("validation", "in_progress"),
                RackValidationState::Partial { .. } => ("validation", "partial"),
                RackValidationState::FailedPartial { .. } => ("validation", "failed_partial"),
                RackValidationState::Validated { .. } => ("validation", "validated"),
                RackValidationState::Failed { .. } => ("validation", "failed"),
            },
            RackState::Ready => ("ready", ""),
            RackState::Maintenance { maintenance_state } => match maintenance_state {
                RackMaintenanceState::FirmwareUpgrade { .. } => ("maintenance", "firmware_upgrade"),
                RackMaintenanceState::NVOSUpdate { .. } => ("maintenance", "nvos_update"),
                RackMaintenanceState::ConfigureNmxCluster { .. } => {
                    ("maintenance", "configure_nmx_cluster")
                }
                RackMaintenanceState::PowerSequence { .. } => ("maintenance", "power_sequence"),
                RackMaintenanceState::Completed => ("maintenance", "completed"),
            },
            RackState::Error { .. } => ("error", ""),
            RackState::Deleting => ("deleting", ""),
        }
    }

    fn manual_intervention_reason(state: &Self::ControllerState) -> Option<&'static str> {
        // The stored cause is free text, so the reason is a fixed token.
        matches!(state, RackState::Error { .. }).then_some("error")
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        state_sla(&state.value, &state.version)
    }
}
