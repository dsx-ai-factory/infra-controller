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

//! Retains deleting peerings until DPUs acknowledge permission removal.

use carbide_uuid::vpc_peering::VpcPeeringId;
use config_version::{ConfigVersion, Versioned};
use db::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::vpc::VpcPeering;
use sqlx::{PgConnection, PgPool};
use state_controller::io::StateControllerIO;
use state_controller::metrics::NoopMetricsEmitter;
use state_controller::state_handler::{
    StateHandler, StateHandlerContext, StateHandlerContextObjects, StateHandlerError,
    StateHandlerOutcome,
};

#[derive(Debug, Default)]
pub(crate) struct VpcPeeringDeletion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeeringDeletionState {
    WaitingForDpus,
}

impl VpcPeeringDeletion {
    async fn wait_for_receivers(
        txn: &mut PgConnection,
        peering: &VpcPeering,
    ) -> Result<Option<StateHandlerOutcome<PeeringDeletionState>>, DatabaseError> {
        for host in db::vpc_peering::find_receivers(txn, peering).await? {
            if !host.managed_host_network_config_version_synced() {
                tracing::info!(
                    vpc_peering_id = %peering.id,
                    host_machine_id = %host.host_snapshot.id,
                    network_config_version = %host.host_snapshot.network_config.version,
                    "Waiting for VPC peering removal acknowledgements",
                );
                return Ok(Some(StateHandlerOutcome::wait(format!(
                    "waiting for all DPUs on host {} to acknowledge network version {} without the peering",
                    host.host_snapshot.id, host.host_snapshot.network_config.version,
                ))));
            }
        }
        Ok(None)
    }
}

impl StateHandlerContextObjects for VpcPeeringDeletion {
    type Services = PgPool;
    type ObjectMetrics = ();
}

#[async_trait::async_trait]
impl StateControllerIO for VpcPeeringDeletion {
    type ObjectId = VpcPeeringId;
    type State = VpcPeering;
    type ControllerState = PeeringDeletionState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = Self;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "vpc_peerings_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "vpc_peerings_controller_queued_objects";
    const LOG_SPAN_CONTROLLER_NAME: &'static str = "vpc_peering_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<VpcPeeringId>, DatabaseError> {
        db::vpc_peering::find_deleting_ids(txn).await
    }

    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        object_id: &VpcPeeringId,
    ) -> Result<Option<VpcPeering>, DatabaseError> {
        Ok(db::vpc_peering::find_by_ids(txn, vec![*object_id])
            .await?
            .pop())
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &VpcPeeringId,
        state: &VpcPeering,
    ) -> Result<Versioned<PeeringDeletionState>, DatabaseError> {
        let version = state.deletion_version.ok_or_else(|| {
            DatabaseError::FailedPrecondition(
                "VPC peering deletion has not been requested".to_string(),
            )
        })?;
        Ok(Versioned::new(
            PeeringDeletionState::WaitingForDpus,
            version,
        ))
    }

    async fn persist_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &VpcPeeringId,
        _old_version: ConfigVersion,
        _new_version: ConfigVersion,
        _new_state: &PeeringDeletionState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        // The API records the only phase. The controller waits or deletes;
        // it never replaces the initial deletion request.
        Err(DatabaseError::FailedPrecondition(
            "VPC peering deletion has no intermediate transitions".to_string(),
        ))
    }

    async fn persist_state_history(
        &self,
        _txn: &mut PgConnection,
        _object_id: &VpcPeeringId,
        _new_version: ConfigVersion,
        _new_state: &PeeringDeletionState,
    ) -> Result<(), DatabaseError> {
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &VpcPeeringId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db::vpc_peering::update_controller_state_outcome(txn, *object_id, outcome).await
    }

    fn metric_state_names(_state: &PeeringDeletionState) -> (&'static str, &'static str) {
        ("deleting", "")
    }

    fn state_sla(
        &self,
        _state: &Versioned<PeeringDeletionState>,
        _object_state: &VpcPeering,
    ) -> StateSla {
        StateSla::no_sla()
    }
}

#[async_trait::async_trait]
impl StateHandler for VpcPeeringDeletion {
    type ObjectId = VpcPeeringId;
    type State = VpcPeering;
    type ControllerState = PeeringDeletionState;
    type ContextObjects = Self;

    async fn handle_object_state(
        &self,
        object_id: &VpcPeeringId,
        state: &mut VpcPeering,
        _controller_state: &PeeringDeletionState,
        ctx: &mut StateHandlerContext<Self>,
    ) -> Result<StateHandlerOutcome<PeeringDeletionState>, StateHandlerError> {
        let mut txn = ctx.services.begin().await?;
        // An unsynced receiver only delays deletion. Do the ordinary wait
        // without the routing lock, then lock and repeat before removing it.
        if let Some(wait) = Self::wait_for_receivers(&mut txn, state).await? {
            return Ok(wait.with_txn(txn));
        }
        db::tenant_prefix_overlap::lock_checks(&mut txn).await?;
        let Some(current) = db::vpc_peering::find_by_id_for_update(&mut txn, *object_id).await?
        else {
            return Ok(StateHandlerOutcome::deleted().with_txn(txn));
        };
        if current.deletion_version != state.deletion_version {
            return Err(StateHandlerError::IterationInvalidated {
                source_ref: std::panic::Location::caller(),
            });
        }
        if let Some(wait) = Self::wait_for_receivers(&mut txn, &current).await? {
            return Ok(wait.with_txn(txn));
        }
        db::vpc_peering::final_delete(&mut txn, *object_id).await?;
        Ok(StateHandlerOutcome::deleted().with_txn(txn))
    }
}
