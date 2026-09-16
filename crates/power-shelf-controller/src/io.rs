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

//! State Controller IO implementation for PowerShelves

use carbide_uuid::power_shelf::PowerShelfId;
use config_version::{ConfigVersion, Versioned};
use db::{
    ConditionalWrite, ControllerStateNotCurrent, DatabaseError, ObjectColumnFilter,
    power_shelf as db_power_shelf,
};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::power_shelf::{
    PowerShelf, PowerShelfControllerState, PowerShelfSearchFilter, state_sla,
};
use model::{DeletedFilter, StateSla};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;

use crate::context::PowerShelfStateHandlerContextObjects;
use crate::metrics::PowerShelfMetricsEmitter;

/// State Controller IO implementation for PowerShelves
#[derive(Default, Debug)]
pub struct PowerShelfStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for PowerShelfStateControllerIO {
    type ObjectId = PowerShelfId;
    type State = PowerShelf;
    type ControllerState = PowerShelfControllerState;
    type MetricsEmitter = PowerShelfMetricsEmitter;
    type ContextObjects = PowerShelfStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "power_shelf_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "power_shelf_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "power_shelf_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db_power_shelf::find_ids(
            txn,
            PowerShelfSearchFilter {
                rack_id: None,
                deleted: DeletedFilter::Include,
                controller_state: None,
                bmc_mac: None,
            },
        )
        .await
    }

    /// Loads a state snapshot from the database
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        power_shelf_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        let mut power_shelves = db_power_shelf::find_by(
            txn,
            ObjectColumnFilter::One(db::power_shelf::IdColumn, power_shelf_id),
        )
        .await?;
        if power_shelves.is_empty() {
            return Ok(None);
        } else if power_shelves.len() != 1 {
            return Err(DatabaseError::new(
                "PowerShelf::find()",
                sqlx::Error::Decode(
                    eyre::eyre!(
                        "searching for PowerShelf {} returned multiple results",
                        power_shelf_id
                    )
                    .into(),
                ),
            ));
        }
        let power_shelf = power_shelves.swap_remove(0);
        Ok(Some(power_shelf))
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
        db_power_shelf::try_update_controller_state(
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
            db::state_history::StateHistoryTableId::PowerShelf,
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
        db_power_shelf::update_controller_state_outcome(txn, *object_id, outcome).await
    }

    fn metric_state_names(state: &PowerShelfControllerState) -> (&'static str, &'static str) {
        match state {
            PowerShelfControllerState::Initializing => ("initializing", ""),
            PowerShelfControllerState::FetchingData => ("fetching_data", ""),
            PowerShelfControllerState::Configuring => ("configuring", ""),
            PowerShelfControllerState::Ready => ("ready", ""),
            PowerShelfControllerState::RotatingBmc { .. } => ("rotatingbmc", ""),
            PowerShelfControllerState::Maintenance { operation } => {
                let op = match operation {
                    model::power_shelf::PowerShelfMaintenanceOperation::PowerOn => "power_on",
                    model::power_shelf::PowerShelfMaintenanceOperation::PowerOff => "power_off",
                };
                ("maintenance", op)
            }
            PowerShelfControllerState::ReProvisioning {
                reprovisioning_state,
            } => {
                let sub = match reprovisioning_state {
                    model::power_shelf::ReProvisioningState::WaitingForRackFirmwareUpgrade => {
                        "waiting_for_rack_firmware_upgrade"
                    }
                };
                ("reprovisioning", sub)
            }
            PowerShelfControllerState::Decommissioning {
                decommissioning_state,
            } => {
                let sub = match decommissioning_state {
                    model::power_shelf::PowerShelfDecommissioningState::SuppressingSiteExplorer => {
                        "suppressingsiteexplorer"
                    }
                    model::power_shelf::PowerShelfDecommissioningState::SuppressingBmcDhcp => {
                        "suppressingbmcdhcp"
                    }
                    model::power_shelf::PowerShelfDecommissioningState::FactoryResetBmc => {
                        "factoryresetbmc"
                    }
                    model::power_shelf::PowerShelfDecommissioningState::WaitingForBmcDhcpAcknowledgement => {
                        "waitingforbmcdhcpacknowledgement"
                    }
                    model::power_shelf::PowerShelfDecommissioningState::DeletingManagedCredentials => {
                        "deletingmanagedcredentials"
                    }
                    model::power_shelf::PowerShelfDecommissioningState::Decommissioned => {
                        "decommissioned"
                    }
                };
                ("decommissioning", sub)
            }
            PowerShelfControllerState::Error { .. } => ("error", ""),
            PowerShelfControllerState::Deleting => ("deleting", ""),
        }
    }

    fn manual_intervention_reason(state: &Self::ControllerState) -> Option<&'static str> {
        // The stored cause is free text, so the reason is a fixed token.
        matches!(state, PowerShelfControllerState::Error { .. }).then_some("error")
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        state_sla(&state.value, &state.version)
    }
}
