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
use model::attestation_scheduling::AttestationScheduling;
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
/// built the stored one or when. Locks the machine row until the caller's
/// transaction ends, so the caller must store the result in that transaction.
pub async fn build_health_report(
    txn: &mut PgConnection,
    machine_id: &MachineId,
) -> Result<HealthReport, DatabaseError> {
    let mut report = HealthReport::empty(HealthReport::ATTESTATION_SOURCE.to_string());

    // Serialize rebuilds per machine. The reads below take no lock of their own,
    // so under READ COMMITTED two devices finishing at once would each build a
    // list from a snapshot missing the other's result, and the second store
    // would drop the first one's alert. Holding the parent row makes the second
    // rebuild wait and then read the first one's committed result.
    //
    // The mode has to be `FOR NO KEY UPDATE`. A caller arrives here having
    // written one of the machine's device rows, which leaves it holding a key
    // share of this row. `FOR UPDATE` conflicts with that share, so two
    // devices of one machine finishing together each waited for the other's
    // share to be released, and deadlocked. This mode does not conflict with
    // it, and still conflicts with itself and with `FOR UPDATE`, which is what
    // serializes the rebuilds against each other and against scheduling.
    let lock = "SELECT 1 FROM machines WHERE id = $1 FOR NO KEY UPDATE";
    sqlx::query(lock)
        .bind(machine_id)
        .fetch_optional(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(lock, e))?;

    // A machine that has never been scheduled has nothing to report.
    let Some(scheduling) =
        crate::machine::get_attestation_scheduling(&mut *txn, machine_id).await?
    else {
        return Ok(report);
    };
    report.alerts.extend(scheduling.coverage_alert());

    let selected = selected_attesters(&mut *txn, machine_id, &scheduling).await?;
    let newest = spdm::get_newest_finished_attestations_for_machine_id(txn, machine_id).await?;
    report.alerts.extend(newest.iter().filter_map(|details| {
        let failure = details.failure_reason()?;
        selected.contains(&details.device_id).then(|| {
            HealthProbeAlert::spdm_attestation_failed(
                details.device_id.clone(),
                format!("{failure} (job {})", details.job_id),
            )
        })
    }));

    Ok(report)
}

/// Returns the attesters the machine's latest decision selects, out of those it
/// has attestation records for.
///
/// Resolving the profile again is what lets a profile change drop an attester
/// out of the report. A class or profile that has gone away since scheduling
/// ran selects no attester.
///
/// The profile is applied to the attesters with results rather than to what
/// the BMC reported last, whatever the stored outcome. An attester the BMC
/// stops reporting keeps its failure, so a BMC that reports nothing cannot
/// reopen allocation.
async fn selected_attesters(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    scheduling: &AttestationScheduling,
) -> Result<HashSet<String>, DatabaseError> {
    // An override replaced the profile for the attempt that recorded it, so
    // what it selected stays in scope until a later decision replaces the
    // record, exactly as a profile's selection does. Only an override that
    // selected something is recorded, so there is no empty one to ignore.
    if let Some(overridden) = &scheduling.attester_override {
        return Ok(overridden.iter().cloned().collect());
    }

    let ProfileResolution::Resolved { profile, .. } = crate::attestation_profile::resolve(
        &mut *txn,
        scheduling.decision.hardware_class.as_deref(),
    )
    .await?
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
    use model::attestation_scheduling::{SchedulingDecision, SchedulingOutcome};

    use super::*;
    use crate::TransactionVending as _;

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
    ) -> Result<uuid::Uuid, Box<dyn std::error::Error>> {
        let job_id = uuid::Uuid::new_v4();
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
        .bind(job_id)
        .bind(cancellation_target)
        .execute(txn)
        .await?;
        Ok(job_id)
    }

    #[crate::sqlx_test]
    async fn only_selected_attesters_are_reported_as_failed(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.txn_begin().await?;
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

        let mut gpu_0_job = None;
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
            let job_id = insert_attestation(
                txn.as_mut(),
                &machine_id,
                device_id,
                state,
                cancellation_target,
            )
            .await?;
            if device_id == "GPU_0" {
                gpu_0_job = Some(job_id);
            }
        }

        // The stored decision predates the profile above, which is the usual
        // reason the two disagree, and is what the coverage alert reports.
        crate::machine::set_attestation_scheduling(
            txn.as_mut(),
            &machine_id,
            &AttestationScheduling {
                decision: SchedulingDecision {
                    outcome: SchedulingOutcome::PartiallySatisfied,
                    hardware_class: Some("nvidia_dgx-h100".to_string()),
                    used_any_fallback: false,
                    profile_version: Some("V7-T1760631000000000".to_string()),
                },
                unsatisfied: Some(UnsatisfiedRequirement::AllowlistPatterns(vec![
                    ComponentIdMatch::Exact("GPU_9".to_string()),
                ])),
                attester_override: None,
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
        assert_eq!(
            report.alerts[1].message,
            format!("bad GPU_0 (job {})", gpu_0_job.expect("GPU_0 was inserted")),
            "the failure alert names the job that produced the result"
        );
        assert_eq!(report.source, HealthReport::ATTESTATION_SOURCE);

        txn.rollback().await?;
        Ok(())
    }

    /// An override replaces the profile for the attempt that recorded it, so
    /// its failures are reported like any other and keep blocking allocation
    /// after the job ends. The profile decides again only once a later
    /// decision replaces the record.
    #[crate::sqlx_test]
    async fn an_overrides_attesters_are_reported_in_place_of_the_profiles(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.txn_begin().await?;
        let machine_id = insert_machine(txn.as_mut(), 4).await?;

        // The profile selects GPU_1, which the override did not ask for.
        crate::attestation_profile::create(
            txn.as_mut(),
            "nvidia_dgx-h100",
            &AttestationPolicyDocument::new(AttesterSelection {
                mode: AttesterSelectionMode::Allowlist,
                component_ids: vec![ComponentIdMatch::Exact("GPU_1".to_string())],
            }),
            "test",
        )
        .await?;

        for device_id in ["GPU_0", "GPU_1", "GPU_2"] {
            insert_attestation(
                txn.as_mut(),
                &machine_id,
                device_id,
                SpdmAttestationState::Failed(format!("bad {device_id}")),
                None,
            )
            .await?;
        }

        crate::machine::set_attestation_scheduling(
            txn.as_mut(),
            &machine_id,
            &AttestationScheduling {
                decision: SchedulingDecision {
                    outcome: SchedulingOutcome::AttesterOverrideScheduled,
                    hardware_class: Some("nvidia_dgx-h100".to_string()),
                    used_any_fallback: false,
                    profile_version: None,
                },
                unsatisfied: None,
                attester_override: Some(vec!["GPU_0".to_string(), "GPU_2".to_string()]),
                job_id: None,
            },
        )
        .await?;

        let report = build_health_report(txn.as_mut(), &machine_id).await?;

        assert_eq!(
            report
                .alerts
                .iter()
                .map(|alert| (alert.id.as_str(), alert.target.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("SpdmAttestationFailed", Some("GPU_0")),
                ("SpdmAttestationFailed", Some("GPU_2")),
            ],
            "the override's attesters are reported and the profile's GPU_1 is not"
        );

        txn.rollback().await?;
        Ok(())
    }

    /// The row lock's contract, which no single-transaction test can show. A
    /// rebuild has to wait for another rebuild of the same machine, or both
    /// build their list from the same snapshot and the later write drops the
    /// other's alert. It has to do that while another transaction is writing
    /// the same machine's device rows, which is how every caller arrives here.
    #[crate::sqlx_test]
    async fn a_rebuild_waits_for_another_rebuild_of_the_same_machine(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut setup = pool.txn_begin().await?;
        let machine_id = insert_machine(setup.as_mut(), 3).await?;
        for device_id in ["GPU_0", "GPU_1"] {
            insert_attestation(
                setup.as_mut(),
                &machine_id,
                device_id,
                SpdmAttestationState::Passed,
                None,
            )
            .await?;
        }
        // Finish both devices here. The job-cleanup trigger takes a per-machine
        // advisory lock the first time a device's `completed_at` appears, which
        // would serialize the two transactions below before either reached the
        // lock under test.
        sqlx::query(
            "UPDATE spdm_machine_devices_attestation SET completed_at = now() \
             WHERE machine_id = $1",
        )
        .bind(machine_id)
        .execute(setup.as_mut())
        .await?;
        setup.commit().await?;

        let mut first = pool.txn_begin().await?;
        let mut second = pool.txn_begin().await?;
        // Bound every statement below, so an unexpected wait fails the test
        // with the statement that waited instead of hanging it. The first
        // transaction is given room to be slow, since it must not wait at all.
        sqlx::query("SET LOCAL lock_timeout = '10s'")
            .execute(&mut *first)
            .await?;
        sqlx::query("SET LOCAL lock_timeout = '250ms'")
            .execute(&mut *second)
            .await?;

        // Hold each transaction's own device row, the way a caller does by
        // writing the result it just settled, before either asks for the
        // machine row. That write leaves a caller holding a key share of the
        // machine row, but this update changes no key column and so takes
        // none; the share is taken explicitly, since it is what a rebuild
        // locking with `FOR UPDATE` would wait on.
        for (txn, device_id) in [(&mut first, "GPU_0"), (&mut second, "GPU_1")] {
            sqlx::query(
                "UPDATE spdm_machine_devices_attestation SET nonce = gen_random_uuid() \
                 WHERE machine_id = $1 AND device_id = $2",
            )
            .bind(machine_id)
            .bind(device_id)
            .execute(&mut **txn)
            .await?;
            sqlx::query("SELECT 1 FROM machines WHERE id = $1 FOR KEY SHARE")
                .bind(machine_id)
                .execute(&mut **txn)
                .await?;
        }

        build_health_report(first.as_mut(), &machine_id).await?;

        let error = build_health_report(second.as_mut(), &machine_id)
            .await
            .expect_err("a second rebuild must wait for the first");
        let DatabaseError::Sqlx(annotated) = &error else {
            panic!("expected a query failure: {error:?}");
        };
        // 55P03 is the timeout waiting for the first rebuild. 40P01 would mean
        // the two rebuilds deadlocked on the machine row instead of queueing.
        assert_eq!(
            annotated
                .source
                .as_database_error()
                .and_then(|database| database.code())
                .as_deref(),
            Some("55P03"),
            "{error:?}"
        );
        Ok(())
    }

    #[crate::sqlx_test]
    async fn a_machine_that_was_never_scheduled_reports_nothing(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.txn_begin().await?;
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
