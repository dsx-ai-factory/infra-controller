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

//! Builds the `attestation` health source from what is stored about a machine.

use std::collections::HashSet;

use carbide_uuid::machine::MachineId;
use health_report::{HealthProbeAlert, HealthReport};
use model::attestation::profile::{ProfileResolution, SelectionOutcome};
use sqlx::PgConnection;

use crate::DatabaseError;
use crate::attestation::spdm;

/// Builds the complete alert list for one machine's `attestation` source.
///
/// The list is the coverage alert the stored scheduling decision calls for,
/// plus one failure alert for each selected attester whose newest finished
/// result counts as a failure, which an operator waiver suppresses and an
/// operator failure creates. Callers store it with
/// [`crate::machine::update_attestation_health_report`], which deletes the
/// source when the list comes out empty.
///
/// The list is built in full every time, so it does not matter which caller
/// built the stored one or when.
pub async fn build_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<HealthReport, DatabaseError> {
    let mut report = HealthReport::empty(HealthReport::ATTESTATION_SOURCE.to_string());

    // A machine that has never been scheduled has nothing to report.
    let Some(scheduling) =
        crate::machine::get_attestation_scheduling(&mut *txn, machine_id).await?
    else {
        return Ok(report);
    };
    report.alerts.extend(scheduling.coverage_alert());

    let selected =
        selected_attesters(&mut *txn, machine_id, scheduling.hardware_class.as_deref()).await?;
    let newest = spdm::get_newest_finished_attestations_for_machine_id(txn, machine_id).await?;
    report.alerts.extend(newest.iter().filter_map(|details| {
        let failure = details.failure_reason()?;
        selected.contains(&details.device_id).then(|| {
            HealthProbeAlert::spdm_attestation_failed(
                details.device_id.clone(),
                failure.to_string(),
            )
        })
    }));

    Ok(report)
}

/// Returns the attesters the machine's current profile selects, out of those it
/// has attestation records for.
///
/// Resolving the profile again is what lets a profile change drop an attester
/// out of the report. An outcome that selects nothing selects no attester, as
/// does a class or profile that has gone away since scheduling ran.
async fn selected_attesters(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    hardware_class: Option<&str>,
) -> Result<HashSet<String>, DatabaseError> {
    let ProfileResolution::Resolved { profile, .. } =
        crate::attestation_profile::resolve(&mut *txn, hardware_class).await?
    else {
        return Ok(HashSet::new());
    };

    let device_ids = spdm::list_device_ids_for_machine_id(txn, machine_id).await?;
    let eligible: Vec<&str> = device_ids.iter().map(String::as_str).collect();
    Ok(
        match profile.policy_document.selection.evaluate(&eligible) {
            SelectionOutcome::Scheduled(selected)
            | SelectionOutcome::PartiallySatisfied { selected, .. } => {
                selected.into_iter().collect()
            }
            SelectionOutcome::AttestationDisabled
            | SelectionOutcome::NoAttestersFound
            | SelectionOutcome::PolicyMatchedNothing(_) => HashSet::new(),
        },
    )
}

#[cfg(test)]
mod tests {
    use carbide_uuid::machine::{MachineIdSource, MachineType};
    use model::attestation::profile::{
        AttestationPolicyDocument, AttesterSelection, AttesterSelectionMode, ComponentIdMatch,
        UnsatisfiedRequirement,
    };
    use model::attestation::spdm::{SpdmAttestationState, SpdmCancellationTarget};
    use model::attestation_scheduling::{AttestationScheduling, SchedulingOutcome};

    use super::*;

    async fn insert_machine(
        txn: &mut PgConnection,
        seed: u8,
    ) -> Result<MachineId, Box<dyn std::error::Error>> {
        let machine_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [seed; 32],
            MachineType::Host,
        );
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(machine_id)
            .execute(txn)
            .await?;
        Ok(machine_id)
    }

    async fn insert_attestation(
        txn: &mut PgConnection,
        machine_id: &MachineId,
        device_id: &str,
        state: SpdmAttestationState,
        cancellation_target: Option<SpdmCancellationTarget>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            "INSERT INTO spdm_machine_devices_attestation \
             (machine_id, device_id, nonce, state, job_id, started_at, \
              cancelled_at, cancellation_target) \
             VALUES ($1, $2, $3, $4, $5, now(), \
                     CASE WHEN $6::TEXT IS NULL THEN NULL ELSE now() END, $6)",
        )
        .bind(machine_id)
        .bind(device_id)
        .bind(uuid::Uuid::new_v4())
        .bind(sqlx::types::Json(state))
        .bind(uuid::Uuid::new_v4())
        .bind(cancellation_target)
        .execute(txn)
        .await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn only_selected_attesters_are_reported_as_failed(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = insert_machine(txn.as_mut(), 1).await?;

        // The current profile attests GPU_0, GPU_1 and GPU_3 but not GPU_2.
        crate::attestation_profile::create(
            txn.as_mut(),
            "nvidia_dgx-h100",
            &AttestationPolicyDocument::new(AttesterSelection {
                mode: AttesterSelectionMode::Allowlist,
                component_ids: vec![
                    ComponentIdMatch::Exact("GPU_0".to_string()),
                    ComponentIdMatch::Exact("GPU_1".to_string()),
                    ComponentIdMatch::Exact("GPU_3".to_string()),
                ],
            }),
            "test",
        )
        .await?;

        for (device_id, state, cancellation_target) in [
            (
                "GPU_0",
                SpdmAttestationState::Failed("bad GPU_0".to_string()),
                None,
            ),
            ("GPU_1", SpdmAttestationState::Passed, None),
            (
                "GPU_2",
                SpdmAttestationState::Failed("bad GPU_2".to_string()),
                None,
            ),
            (
                "GPU_3",
                SpdmAttestationState::Failed("bad GPU_3".to_string()),
                Some(SpdmCancellationTarget::Waive),
            ),
        ] {
            insert_attestation(
                txn.as_mut(),
                &machine_id,
                device_id,
                state,
                cancellation_target,
            )
            .await?;
        }

        // The stored decision predates the profile above, which is the usual
        // reason the two disagree, and is what the coverage alert reports.
        crate::machine::set_attestation_scheduling(
            txn.as_mut(),
            &machine_id,
            &AttestationScheduling {
                outcome: SchedulingOutcome::PartiallySatisfied,
                hardware_class: Some("nvidia_dgx-h100".to_string()),
                used_any_fallback: false,
                profile_version: Some("V7-T1760631000000000".to_string()),
                unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                    ComponentIdMatch::Exact("GPU_9".to_string()),
                ])),
                job_id: None,
            },
        )
        .await?;

        let report = build_health_report(txn.as_mut(), &machine_id).await?;

        let alerts: Vec<(&str, Option<&str>)> = report
            .alerts
            .iter()
            .map(|alert| (alert.id.as_str(), alert.target.as_deref()))
            .collect();
        assert_eq!(
            alerts,
            vec![
                ("AttestationPolicyUnsatisfied", Some("nvidia_dgx-h100")),
                ("SpdmAttestationFailed", Some("GPU_0")),
            ],
            "the coverage alert and the failure of a selected attester are both \
             reported, GPU_2's failure is not because the profile excludes it, \
             and GPU_3's is not because an operator waived it"
        );
        assert_eq!(report.source, HealthReport::ATTESTATION_SOURCE);

        txn.rollback().await?;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn a_machine_that_was_never_scheduled_reports_nothing(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id = insert_machine(txn.as_mut(), 2).await?;
        insert_attestation(
            txn.as_mut(),
            &machine_id,
            "GPU_0",
            SpdmAttestationState::Failed("bad GPU_0".to_string()),
            None,
        )
        .await?;

        let report = build_health_report(txn.as_mut(), &machine_id).await?;

        assert!(
            report.alerts.is_empty(),
            "without a stored decision there is no selection to report against"
        );

        txn.rollback().await?;
        Ok(())
    }
}
