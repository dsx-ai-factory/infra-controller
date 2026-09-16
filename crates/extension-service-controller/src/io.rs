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

//! State-controller IO for DPF Helm chart extension services.

use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::{ConfigVersion, Versioned};
use db::{self, ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use model::StateSla;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::extension_service::{
    ExtensionService, ExtensionServiceLifecycleState, ExtensionServiceType,
};
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;
use state_controller::metrics::NoopMetricsEmitter;

use crate::context::ExtensionServiceStateHandlerContextObjects;

/// IO implementation for DPF Helm chart extension services.
#[derive(Default, Debug)]
pub struct ExtensionServiceStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for ExtensionServiceStateControllerIO {
    type ObjectId = ExtensionServiceId;
    type State = ExtensionService;
    type ControllerState = ExtensionServiceLifecycleState;
    type MetricsEmitter = NoopMetricsEmitter;
    type ContextObjects = ExtensionServiceStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "extension_services_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str =
        "extension_services_controller_queued_objects";
    const LOG_SPAN_CONTROLLER_NAME: &'static str = "extension_service_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        // Include soft-deleted rows so deletion reconciliation can remove a
        // detached DPUService after its parent has been deleted.
        db::extension_service::find_ids(
            txn,
            Some(ExtensionServiceType::DpfHelmChart),
            None,
            None,
            true,
            false,
        )
        .await
    }

    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        service_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        Ok(
            db::extension_service::find_by_ids(txn, &[*service_id], true, false)
                .await?
                .pop()
                .filter(|service| service.service_type == ExtensionServiceType::DpfHelmChart),
        )
    }

    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _service_id: &Self::ObjectId,
        state: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        Ok(state.status.controller_state.clone())
    }

    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        service_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        db::extension_service::try_update_controller_state(
            txn,
            *service_id,
            old_version,
            new_version,
            new_state,
        )
        .await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        service_id: &Self::ObjectId,
        new_version: ConfigVersion,
        new_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        db::state_history::persist(
            txn,
            db::state_history::StateHistoryTableId::ExtensionService,
            service_id,
            new_state,
            new_version,
        )
        .await?;
        Ok(())
    }

    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        service_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db::extension_service::update_controller_state_outcome(txn, *service_id, outcome).await
    }

    fn metric_state_names(state: &ExtensionServiceLifecycleState) -> (&'static str, &'static str) {
        match state {
            ExtensionServiceLifecycleState::Creating => ("creating", ""),
            ExtensionServiceLifecycleState::Ready => ("ready", ""),
            ExtensionServiceLifecycleState::Updating => ("updating", ""),
            ExtensionServiceLifecycleState::Deleting => ("deleting", ""),
            ExtensionServiceLifecycleState::Deleted => ("deleted", ""),
            ExtensionServiceLifecycleState::Failed => ("failed", ""),
        }
    }

    fn state_sla(
        &self,
        _state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        // @TODO(Felicity): Add SLA.
        StateSla::no_sla()
    }
}
