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

//! State controller IO implementation for VPC prefixes.

use carbide_uuid::vpc::VpcPrefixId;
use config_version::{ConfigVersion, Versioned};
use db::{self, ConditionalWrite, ControllerStateNotCurrent, DatabaseError, ObjectColumnFilter};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::vpc_prefix::{self, VpcPrefix, VpcPrefixControllerState, VpcPrefixDeletionState};
use model::{DeletedFilter, StateSla};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;
use state_controller::metrics::NoopMetricsEmitter;

use crate::context::VpcPrefixStateHandlerContextObjects;

/// State controller IO implementation for VPC prefixes.
#[derive(Default, Debug)]
pub struct VpcPrefixStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for VpcPrefixStateControllerIO {
    type ObjectId = VpcPrefixId;
    type State = VpcPrefix;
    type ControllerState = VpcPrefixControllerState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = VpcPrefixStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str =
        "network_vpc_prefixes_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str =
        "network_vpc_prefixes_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "vpc_prefix_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db::vpc_prefix::search(
            txn,
            vpc_prefix::VpcPrefixSearch {
                site_prefix_id: None,
                deleted_filter: DeletedFilter::Include,
                ..Default::default()
            },
        )
        .await
    }

    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        vpc_prefix_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        // Load the controller-visible row, including prefixes marked deleted.
        let mut prefixes = db::vpc_prefix::get_by_id(
            txn,
            ObjectColumnFilter::One(db::vpc_prefix::IdColumn, vpc_prefix_id),
            DeletedFilter::Include,
        )
        .await?;
        if prefixes.is_empty() {
            return Ok(None);
        }
        if prefixes.len() > 1 {
            return Err(DatabaseError::new(
                "db::vpc_prefix::get_by_id()",
                sqlx::Error::Decode(
                    eyre::eyre!(
                        "searching for VpcPrefix {} returned multiple results",
                        vpc_prefix_id
                    )
                    .into(),
                ),
            ));
        }

        // Return the only matching prefix row to the controller framework.
        Ok(Some(prefixes.swap_remove(0)))
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        // Read the versioned controller state from the object snapshot.
        Ok(state.status.controller_state.clone())
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        // Optimistically update the controller-owned state version and value.
        db::vpc_prefix::try_update_controller_state(
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
        // Record the transition in the VPC prefix state-history table.
        db::state_history::persist(
            txn,
            db::state_history::StateHistoryTableId::VpcPrefix,
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
        // Store the latest handler outcome alongside the prefix row.
        db::vpc_prefix::update_controller_state_outcome(txn, *object_id, outcome).await
    }

    fn metric_state_names(state: &VpcPrefixControllerState) -> (&'static str, &'static str) {
        /// Returns the aggregate metric substate name for deletion states.
        fn deletion_state_name(deletion_state: &VpcPrefixDeletionState) -> &'static str {
            match deletion_state {
                VpcPrefixDeletionState::DrainNetworkPrefixes { .. } => "drainnetworkprefixes",
                VpcPrefixDeletionState::DBDelete => "dbdelete",
            }
        }

        // Map the controller state to the labels used by framework metrics.
        match state {
            VpcPrefixControllerState::Provisioning => ("provisioning", ""),
            VpcPrefixControllerState::Ready => ("ready", ""),
            VpcPrefixControllerState::Deleting { deletion_state } => {
                ("deleting", deletion_state_name(deletion_state))
            }
        }
    }

    fn state_sla(
        &self,
        state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        // Delegate SLA calculation to the VPC prefix model helper.
        vpc_prefix::state_sla(&state.value, &state.version)
    }
}
