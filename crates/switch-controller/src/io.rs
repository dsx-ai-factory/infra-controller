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

//! State Controller IO implementation for Switches

use carbide_uuid::switch::SwitchId;
use config_version::{ConfigVersion, Versioned};
use db::{
    ConditionalWrite, ControllerStateNotCurrent, DatabaseError, ObjectColumnFilter,
    switch as db_switch,
};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::switch::{
    ConfigureCertificateState, Switch, SwitchControllerState, SwitchDecommissioningState,
    SwitchMaintenanceOperation, SwitchSearchFilter, state_sla,
};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;

use crate::context::SwitchStateHandlerContextObjects;
use crate::metrics::SwitchMetricsEmitter;

/// State Controller IO implementation for Switches
#[derive(Default, Debug)]
pub struct SwitchStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for SwitchStateControllerIO {
    type ObjectId = SwitchId;
    type State = Switch;
    type ControllerState = SwitchControllerState;
    type MetricsEmitter = SwitchMetricsEmitter;
    type ContextObjects = SwitchStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "switch_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "switch_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "switch_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db_switch::find_ids(
            txn,
            SwitchSearchFilter {
                rack_id: None,
                deleted: model::DeletedFilter::Include,
                controller_state: None,
                bmc_mac: None,
                ..Default::default()
            },
        )
        .await
    }

    /// Loads a state snapshot from the database
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        switch_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        let mut switches = db_switch::find_by(
            txn,
            ObjectColumnFilter::One(db::switch::IdColumn, switch_id),
        )
        .await?;
        if switches.is_empty() {
            return Ok(None);
        } else if switches.len() != 1 {
            return Err(DatabaseError::new(
                "Switch::find()",
                sqlx::Error::Decode(
                    eyre::eyre!(
                        "searching for switch {} returned multiple results",
                        switch_id
                    )
                    .into(),
                ),
            ));
        }
        let switch = switches.swap_remove(0);
        Ok(Some(switch))
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
        db_switch::try_update_controller_state(txn, *object_id, old_version, new_version, new_state)
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
            db::state_history::StateHistoryTableId::Switch,
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
        db_switch::update_controller_state_outcome(txn, *object_id, outcome).await
    }

    fn metric_state_names(state: &SwitchControllerState) -> (&'static str, &'static str) {
        match state {
            SwitchControllerState::Created => ("created", ""),
            SwitchControllerState::Initializing { .. } => ("initializing", ""),
            SwitchControllerState::Configuring { .. } => ("configuring", ""),
            SwitchControllerState::FetchInfo => ("fetchinfo", ""),
            SwitchControllerState::Validating { .. } => ("validating", ""),
            SwitchControllerState::BomValidating { .. } => ("bomvalidating", ""),
            SwitchControllerState::Ready => ("ready", ""),
            SwitchControllerState::Decommissioning {
                decommissioning_state,
            } => (
                "decommissioning",
                match decommissioning_state {
                    SwitchDecommissioningState::SuppressingSiteExplorer => {
                        "suppressing_site_explorer"
                    }
                    SwitchDecommissioningState::SuppressingNvosDhcp => "suppressing_nvos_dhcp",
                    SwitchDecommissioningState::FactoryResetNvos => "factory_reset_nvos",
                    SwitchDecommissioningState::WaitingForNvosDhcpAcknowledgement => {
                        "waiting_for_nvos_dhcp_acknowledgement"
                    }
                    SwitchDecommissioningState::SuppressingBmcDhcp => "suppressing_bmc_dhcp",
                    SwitchDecommissioningState::FactoryResetBmc => "factory_reset_bmc",
                    SwitchDecommissioningState::WaitingForBmcDhcpAcknowledgement => {
                        "waiting_for_bmc_dhcp_acknowledgement"
                    }
                    SwitchDecommissioningState::DeletingManagedCredentials => {
                        "deleting_managed_credentials"
                    }
                    SwitchDecommissioningState::Decommissioned => "decommissioned",
                },
            ),
            SwitchControllerState::RotatingBmc { .. } => ("rotatingbmc", ""),
            SwitchControllerState::Maintenance {
                operation,
                configure_certificate,
                ..
            } => {
                let substate = match operation {
                    SwitchMaintenanceOperation::PowerOn => "power_on",
                    SwitchMaintenanceOperation::PowerOff => "power_off",
                    SwitchMaintenanceOperation::Reset => "reset",
                    SwitchMaintenanceOperation::ReconfigureCertificate => {
                        return match configure_certificate {
                            Some(ConfigureCertificateState::WaitForComplete { .. }) => {
                                ("maintenance", "reconfigure_certificate_wait")
                            }
                            _ => ("maintenance", "reconfigure_certificate"),
                        };
                    }
                };
                ("maintenance", substate)
            }
            SwitchControllerState::ReProvisioning { .. } => ("reprovisioning", ""),
            SwitchControllerState::Error { .. } => ("error", ""),
            SwitchControllerState::Deleting => ("deleting", ""),
        }
    }

    fn manual_intervention_reason(state: &Self::ControllerState) -> Option<&'static str> {
        // The stored cause is free text, so the reason is a fixed token.
        matches!(state, SwitchControllerState::Error { .. }).then_some("error")
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        state_sla(&state.value, &state.version)
    }
}
