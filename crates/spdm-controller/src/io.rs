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

//! State controller IO for SPDM device attestation jobs.

use config_version::{ConfigVersion, Versioned};
use db::attestation::spdm::load_snapshot_for_machine_and_device_id;
use db::{ConditionalWrite, ControllerStateNotCurrent, DatabaseError};
use model::StateSla;
use model::attestation::spdm::{SpdmAttestationState, SpdmDeviceAttestation, SpdmObjectId};
use model::controller_outcome::PersistentStateHandlerOutcome;
use sqlx::PgConnection;
use state_controller::io::StateControllerIO;

use crate::context::SpdmStateHandlerContextObjects;
use crate::metrics::SpdmMetricsEmitter;

/// Loads and persists SPDM device attestation jobs for the state controller.
#[derive(Default, Debug)]
pub struct SpdmStateControllerIO {}

#[async_trait::async_trait]
impl StateControllerIO for SpdmStateControllerIO {
    type ObjectId = SpdmObjectId; // tuple of machine ID, device ID, and job ID
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
            load_snapshot_for_machine_and_device_id(txn, &object_id.0, &object_id.1, &object_id.2)
                .await?,
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
    ) -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, DatabaseError> {
        let write = db::attestation::spdm::persist_controller_state(
            txn,
            object_id,
            old_version,
            new_version,
            new_controller_state,
        )
        .await?;

        // Republish the machine's alerts from the result just written. Only a
        // settled device changes what the report says, and the processor rolls
        // back a rejected write, so neither other case is worth rebuilding for.
        // This is the whole list rather than this device's part of it, so it
        // does not matter whether scheduling or another device wrote it last.
        if matches!(write, ConditionalWrite::Applied(())) && new_controller_state.is_terminal() {
            let machine_id = &object_id.0;
            let report = db::attestation::health::build_health_report(txn, machine_id).await?;
            db::machine::update_attestation_health_report(txn, machine_id, &report).await?;
        }

        Ok(write)
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
            SpdmAttestationState::WaitForTaskCancellation { .. } => ("waitfortaskcancellation", ""),
            SpdmAttestationState::NrasVerification => ("nrasverification", ""),
            SpdmAttestationState::ApplyAppraisalPolicy => ("applyappraisalpolicy", ""),
            SpdmAttestationState::Failed(_) => ("failed", ""),
            SpdmAttestationState::Passed => ("passed", ""),
            SpdmAttestationState::Waived => ("cancelled", ""),
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

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
    use health_report::HealthReport;
    use model::attestation::profile::{
        AttestationPolicyDocument, AttesterSelection, AttesterSelectionMode, ComponentIdMatch,
        UnsatisfiedRequirement,
    };
    use model::attestation_scheduling::{AttestationScheduling, SchedulingOutcome};
    use uuid::Uuid;

    use super::*;

    const HARDWARE_CLASS: &str = "nvidia_dgx-h100";

    /// A machine whose profile attests every `GPU_` attester, carrying the
    /// scheduling decision `outcome` describes.
    async fn machine_with_profile(
        txn: &mut PgConnection,
        seed: u8,
        outcome: SchedulingOutcome,
    ) -> Result<MachineId, Box<dyn std::error::Error>> {
        let machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [seed; 32],
            MachineType::Host,
        );
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(&mut *txn)
            .await?;
        db::attestation_profile::create(
            &mut *txn,
            HARDWARE_CLASS,
            &AttestationPolicyDocument::new(AttesterSelection {
                mode: AttesterSelectionMode::Allowlist,
                component_ids: vec![ComponentIdMatch::Prefix("GPU_".to_string())],
            }),
            "test",
        )
        .await?;
        db::machine::set_attestation_scheduling(
            &mut *txn,
            &machine_id,
            &AttestationScheduling {
                outcome,
                hardware_class: Some(HARDWARE_CLASS.to_string()),
                used_any_fallback: false,
                profile_version: Some("V7-T1760631000000000".to_string()),
                unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                    ComponentIdMatch::Exact("GPU_9".to_string()),
                ])),
                job_id: None,
            },
        )
        .await?;
        Ok(machine_id)
    }

    /// Adds one device's row for a job, returning the object and the state
    /// version a persist call has to match.
    async fn insert_device(
        txn: &mut PgConnection,
        machine_id: &MachineId,
        device_id: &str,
        job_id: Uuid,
        state: SpdmAttestationState,
        started_at: &str,
    ) -> Result<(SpdmObjectId, ConfigVersion), Box<dyn std::error::Error>> {
        let query = "INSERT INTO spdm_machine_devices_attestation \
             (machine_id, device_id, nonce, state, job_id, started_at) \
             VALUES ($1, $2, $3, $4, $5, $6::timestamptz) \
             RETURNING state_version";
        let version: ConfigVersion = sqlx::query_scalar(query)
            .bind(machine_id)
            .bind(device_id)
            .bind(Uuid::new_v4())
            .bind(sqlx::types::Json(state))
            .bind(job_id)
            .bind(started_at)
            .fetch_one(&mut *txn)
            .await?;
        Ok((
            SpdmObjectId(*machine_id, device_id.to_string(), job_id),
            version,
        ))
    }

    /// Writes `state` for the device the way the processor does, and returns
    /// the alert IDs and targets the machine is left carrying.
    async fn settle(
        txn: &mut PgConnection,
        object_id: &SpdmObjectId,
        version: ConfigVersion,
        state: SpdmAttestationState,
    ) -> Result<Vec<(String, Option<String>)>, Box<dyn std::error::Error>> {
        let write = SpdmStateControllerIO::default()
            .persist_controller_state(txn, object_id, version, version.increment(), &state)
            .await?;
        assert!(
            matches!(write, ConditionalWrite::Applied(())),
            "the version gate must accept the write the test set up"
        );

        let stored: Option<sqlx::types::Json<HealthReport>> = sqlx::query_scalar(
            "SELECT health_reports->'merges'->'attestation' FROM machines WHERE id = $1",
        )
        .bind(object_id.0)
        .fetch_one(txn)
        .await?;
        Ok(stored
            .map(|sqlx::types::Json(report)| {
                report
                    .alerts
                    .into_iter()
                    .map(|alert| (alert.id.as_str().to_string(), alert.target))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The report is the machine's whole list, not this device's part of it,
    /// and a device still being attested contributes nothing to it yet.
    #[crate::sqlx_test]
    async fn a_settled_device_publishes_the_whole_list_for_its_machine(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            machine_with_profile(txn.as_mut(), 1, SchedulingOutcome::PartiallySatisfied).await?;
        let job_id = Uuid::new_v4();
        let (failing, version) = insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            job_id,
            SpdmAttestationState::NrasVerification,
            "2026-01-01T00:00:00Z",
        )
        .await?;
        insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_1",
            job_id,
            SpdmAttestationState::NrasVerification,
            "2026-01-01T00:00:00Z",
        )
        .await?;

        let alerts = settle(
            txn.as_mut(),
            &failing,
            version,
            SpdmAttestationState::Failed("bad GPU_0".to_string()),
        )
        .await?;

        assert_eq!(
            alerts,
            vec![
                (
                    "AttestationPolicyUnsatisfied".to_string(),
                    Some(HARDWARE_CLASS.to_string())
                ),
                (
                    "SpdmAttestationFailed".to_string(),
                    Some("GPU_0".to_string())
                ),
            ],
            "the coverage alert from the stored decision and the failure just \
             written are both reported, and GPU_1 is absent because it has not \
             finished"
        );

        txn.rollback().await?;
        Ok(())
    }

    /// Attestation passes through several states per device, so rebuilding on
    /// every one of them would republish the same list on each poll.
    #[crate::sqlx_test]
    async fn a_transition_that_does_not_settle_leaves_the_report_alone(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            machine_with_profile(txn.as_mut(), 2, SchedulingOutcome::PartiallySatisfied).await?;
        let (device, version) = insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            Uuid::new_v4(),
            SpdmAttestationState::FetchCertificate,
            "2026-01-01T00:00:00Z",
        )
        .await?;

        let alerts = settle(
            txn.as_mut(),
            &device,
            version,
            SpdmAttestationState::NrasVerification,
        )
        .await?;

        assert!(
            alerts.is_empty(),
            "a device still being attested leaves the source untouched, even \
             though the stored decision calls for a coverage alert"
        );

        txn.rollback().await?;
        Ok(())
    }

    /// Jobs for one device can overlap, and the later one holds the answer.
    #[crate::sqlx_test]
    async fn a_late_result_does_not_displace_a_newer_jobs_result(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            machine_with_profile(txn.as_mut(), 3, SchedulingOutcome::Scheduled).await?;
        let (older, version) = insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            Uuid::new_v4(),
            SpdmAttestationState::NrasVerification,
            "2026-01-01T00:00:00Z",
        )
        .await?;
        insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            Uuid::new_v4(),
            SpdmAttestationState::Passed,
            "2026-01-02T00:00:00Z",
        )
        .await?;

        let alerts = settle(
            txn.as_mut(),
            &older,
            version,
            SpdmAttestationState::Failed("bad GPU_0".to_string()),
        )
        .await?;

        assert!(
            alerts.is_empty(),
            "the newer job's pass is this attester's result, so the older job \
             finishing as a failure raises nothing"
        );

        txn.rollback().await?;
        Ok(())
    }

    /// Re-attesting must not clear a failure before the retry has answered,
    /// because the machine is no safer while that attester is being measured.
    #[crate::sqlx_test]
    async fn a_failure_survives_re_attestation_and_clears_once_the_retry_passes(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            machine_with_profile(txn.as_mut(), 4, SchedulingOutcome::Scheduled).await?;
        let first_job = Uuid::new_v4();
        let (failing, failing_version) = insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            first_job,
            SpdmAttestationState::NrasVerification,
            "2026-01-01T00:00:00Z",
        )
        .await?;
        let (sibling, sibling_version) = insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_1",
            first_job,
            SpdmAttestationState::NrasVerification,
            "2026-01-01T00:00:00Z",
        )
        .await?;
        settle(
            txn.as_mut(),
            &failing,
            failing_version,
            SpdmAttestationState::Failed("bad GPU_0".to_string()),
        )
        .await?;

        // A retry for GPU_0 is under way when its sibling finishes.
        let (retry, retry_version) = insert_device(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            Uuid::new_v4(),
            SpdmAttestationState::NrasVerification,
            "2026-01-02T00:00:00Z",
        )
        .await?;
        let alerts = settle(
            txn.as_mut(),
            &sibling,
            sibling_version,
            SpdmAttestationState::Passed,
        )
        .await?;
        assert_eq!(
            alerts,
            vec![(
                "SpdmAttestationFailed".to_string(),
                Some("GPU_0".to_string())
            )],
            "the failure stands while GPU_0 is attested again"
        );

        let alerts = settle(
            txn.as_mut(),
            &retry,
            retry_version,
            SpdmAttestationState::Passed,
        )
        .await?;
        assert!(
            alerts.is_empty(),
            "the retry passing clears the failure, and an empty list deletes \
             the source"
        );

        txn.rollback().await?;
        Ok(())
    }
}
