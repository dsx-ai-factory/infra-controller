/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use config_version::{ConfigVersion, Versioned};
use db::nvlink_domain::lifecycle;
use db::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::nvlink_domain::{NvLinkDomainSnapshot, NvLinkDomainState};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;
use state_controller::metrics::NoopMetricsEmitter;
use uuid::Uuid;

use crate::NvLinkDomainStateHandlerContext;

/// Domain persistence plugged into the shared state-controller runtime.
#[derive(Debug, Default)]
pub struct NvLinkDomainStateControllerIO;

#[async_trait::async_trait]
impl StateControllerIO for NvLinkDomainStateControllerIO {
    type ObjectId = Uuid;
    type State = NvLinkDomainSnapshot;
    type ControllerState = NvLinkDomainState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = NvLinkDomainStateHandlerContext;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "nvlink_domain_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "nvlink_domain_controller_queued_objects";
    const LOG_SPAN_CONTROLLER_NAME: &'static str = "nvlink_domain_controller";

    async fn list_objects(&self, txn: &mut PgConnection) -> Result<Vec<Uuid>, DatabaseError> {
        lifecycle::find_ids(txn).await
    }

    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        id: &Uuid,
    ) -> Result<Option<NvLinkDomainSnapshot>, DatabaseError> {
        lifecycle::find_snapshot(txn, *id).await
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _id: &Uuid,
        state: &NvLinkDomainSnapshot,
    ) -> Result<Versioned<NvLinkDomainState>, DatabaseError> {
        Ok(state.controller_state.clone())
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        id: &Uuid,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &NvLinkDomainState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        lifecycle::try_update_controller_state(txn, *id, old_version, new_version, new_state).await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        id: &Uuid,
        new_version: ConfigVersion,
        new_state: &NvLinkDomainState,
    ) -> Result<(), DatabaseError> {
        db::state_history::persist(
            txn,
            db::state_history::StateHistoryTableId::NvLinkDomain,
            id,
            new_state,
            new_version,
        )
        .await?;
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        id: &Uuid,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        lifecycle::update_controller_state_outcome(txn, *id, outcome).await
    }

    fn metric_state_names(state: &NvLinkDomainState) -> (&'static str, &'static str) {
        match state {
            NvLinkDomainState::Created => ("created", ""),
            NvLinkDomainState::Discovering => ("discovering", ""),
            NvLinkDomainState::Validating { .. } => ("validating", ""),
            NvLinkDomainState::Ready => ("ready", ""),
            NvLinkDomainState::Maintenance { .. } => ("maintenance", ""),
            NvLinkDomainState::Error { .. } => ("error", ""),
            NvLinkDomainState::Deleting => ("deleting", ""),
        }
    }

    fn manual_intervention_reason(state: &NvLinkDomainState) -> Option<&'static str> {
        matches!(state, NvLinkDomainState::Error { .. }).then_some("error")
    }

    fn state_sla(
        &self,
        _state: &Versioned<NvLinkDomainState>,
        _object_state: &NvLinkDomainSnapshot,
    ) -> StateSla {
        StateSla::no_sla()
    }
}
