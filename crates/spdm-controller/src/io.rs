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

//! State Controller IO implementation for dpa interfaces

use config_version::{ConfigVersion, Versioned};
use db::DatabaseError;
use db::attestation::spdm::load_snapshot_for_machine_and_device_id;
use model::StateSla;
use model::attestation::spdm::{SpdmAttestationState, SpdmDeviceAttestation, SpdmObjectId};
use model::controller_outcome::PersistentStateHandlerOutcome;
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;

use crate::context::SpdmStateHandlerContextObjects;
use crate::metrics::SpdmMetricsEmitter;

/// State Controller IO implementation for dpa interfaces
#[derive(Default, Debug)]
pub struct SpdmStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for SpdmStateControllerIO {
    type ObjectId = SpdmObjectId; // tuple of machine id and device id
    type State = SpdmDeviceAttestation;
    type ControllerState = SpdmAttestationState;
    type MetricsEmitter = SpdmMetricsEmitter;
    type ContextObjects = SpdmStateHandlerContextObjects;

    const DB_ITERATION_ID_TABLE_NAME: &'static str = "attestation_controller_iteration_ids";
    const DB_QUEUED_OBJECTS_TABLE_NAME: &'static str = "attestation_controller_queued_objects";

    const LOG_SPAN_CONTROLLER_NAME: &'static str = "attestation_controller";

    async fn list_objects(
        &self,
        txn: &mut PgConnection,
    ) -> Result<Vec<Self::ObjectId>, DatabaseError> {
        db::attestation::spdm::find_machine_ids_for_attestation(txn).await
    }

    /// Load SpdmDeviceAttestation
    async fn load_object_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
    ) -> Result<Option<Self::State>, DatabaseError> {
        Ok(Some(
            load_snapshot_for_machine_and_device_id(txn, &object_id.0, &object_id.1).await?,
        ))
    }

    // Load AttestationState
    async fn load_controller_state(
        &self,
        _txn: &mut PgConnection,
        _object_id: &Self::ObjectId,
        snapshot: &Self::State,
    ) -> Result<Versioned<Self::ControllerState>, DatabaseError> {
        let version = snapshot.state_version;
        Ok(Versioned::new(snapshot.state.clone(), version))
    }

    // Store AttestationState
    async fn persist_controller_state(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        old_version: ConfigVersion,
        new_version: ConfigVersion,
        new_controller_state: &Self::ControllerState,
    ) -> Result<bool, DatabaseError> {
        db::attestation::spdm::persist_controller_state(
            txn,
            object_id,
            old_version,
            new_version,
            new_controller_state,
        )
        .await
    }

    async fn persist_state_history(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        _new_version: ConfigVersion,
        new_controller_state: &Self::ControllerState,
    ) -> Result<(), DatabaseError> {
        db::attestation::spdm::update_history(txn, object_id, new_controller_state).await
    }

    // Store outcome
    async fn persist_outcome(
        &self,
        txn: &mut PgConnection,
        object_id: &Self::ObjectId,
        outcome: PersistentStateHandlerOutcome,
    ) -> Result<(), DatabaseError> {
        db::attestation::spdm::persist_outcome(txn, object_id, outcome).await
    }

    fn metric_state_names(
        state: &model::attestation::spdm::SpdmAttestationState,
    ) -> (&'static str, &'static str) {
        match state {
            SpdmAttestationState::FetchMetadata => ("fetchmetadata", ""),
            SpdmAttestationState::FetchCertificate => ("fetchcertificate", ""),
            SpdmAttestationState::TriggerEvidenceCollection { .. } => {
                ("triggerevidencecollection", "")
            }
            SpdmAttestationState::PollEvidenceCollection { .. } => ("pollevidencecolletion", ""),
            SpdmAttestationState::NrasVerification => ("nrasverification", ""),
            SpdmAttestationState::ApplyAppraisalPolicy => ("applyappraisalpolicy", ""),
            SpdmAttestationState::Failed(_) => ("failed", ""),
            SpdmAttestationState::Passed => ("passed", ""),
            SpdmAttestationState::Cancelled => ("cancelled", ""),
        }
    }

    fn state_sla(
        &self,
        _state: &Versioned<Self::ControllerState>,
        _object_state: &Self::State,
    ) -> StateSla {
        StateSla::no_sla()
    }
}
