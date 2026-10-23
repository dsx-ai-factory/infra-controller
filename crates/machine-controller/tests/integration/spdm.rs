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

use std::collections::BTreeSet;

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, MOCK_HOST_HARDWARE_CLASS,
};
use carbide_uuid::machine::MachineId;
use chrono::Utc;
use model::attestation::profile::{
    AttestationPolicyDocument, AttesterSelection, AttesterSelectionMode, ComponentIdMatch,
};
use model::attestation::spdm::SpdmAttestationState;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::machine::{
    CleanupContext, CleanupState, FailureCause, FailureDetails, FailureSource, MachineState,
    ManagedHostState, SpdmMeasuringState, StateMachineArea,
};
use model::test_support::ManagedHostConfig;
use state_controller::state_handler::StateHandlerError;

use crate::env::Env;

// Polling and recovery need a discovered host and a working BMC.
async fn setup_spdm_host(pool: PgPool, spdm_enabled: bool) -> (Env, TestManagedHost) {
    let env = Env::builder(pool)
        .configure_runtime(|config| config.spdm.enabled = spdm_enabled)
        .build()
        .await;
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let mh = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;
    (env, mh)
}

/// A matching job polls the latest machine results for its devices, including failure details.
#[sqlx_test]
async fn matching_spdm_jobs_poll_latest_machine_results(pool: sqlx::PgPool) -> eyre::Result<()> {
    enum Expected {
        Advance,
        Wait,
        Failed,
    }
    struct Case {
        scenario: &'static str,
        job_state: SpdmAttestationState,
        gpu_0_state: SpdmAttestationState,
        gpu_1_state: SpdmAttestationState,
        expected: Expected,
    }
    let cases = [
        Case {
            scenario: "a cancelled job reports only current selected failures from another job",
            job_state: SpdmAttestationState::Cancelled,
            gpu_0_state: SpdmAttestationState::Passed,
            gpu_1_state: SpdmAttestationState::Failed("selected current failure".to_string()),
            expected: Expected::Failed,
        },
        Case {
            scenario: "newer selected passes supersede a failed job and excluded failures",
            job_state: SpdmAttestationState::Failed("superseded job failure".to_string()),
            gpu_0_state: SpdmAttestationState::Passed,
            gpu_1_state: SpdmAttestationState::Passed,
            expected: Expected::Advance,
        },
        Case {
            scenario: "a cancelled machine advances after a passed job",
            job_state: SpdmAttestationState::Passed,
            gpu_0_state: SpdmAttestationState::Cancelled,
            gpu_1_state: SpdmAttestationState::Passed,
            expected: Expected::Advance,
        },
        Case {
            scenario: "a passed job waits for newer selected work",
            job_state: SpdmAttestationState::Passed,
            gpu_0_state: SpdmAttestationState::FetchMetadata,
            gpu_1_state: SpdmAttestationState::Passed,
            expected: Expected::Wait,
        },
        Case {
            scenario: "an unfinished matching job waits even when newer machine results have passed",
            job_state: SpdmAttestationState::FetchMetadata,
            gpu_0_state: SpdmAttestationState::Passed,
            gpu_1_state: SpdmAttestationState::Passed,
            expected: Expected::Wait,
        },
    ];
    let (mut env, mh) = setup_spdm_host(pool, true).await;
    let machine_id: MachineId = mh.host.id.into();

    for case in cases {
        let job_id = uuid::Uuid::new_v4();
        let current_job_id = uuid::Uuid::new_v4();
        let historical_job_id = uuid::Uuid::new_v4();
        let poll_state = ManagedHostState::HostInit {
            machine_state: MachineState::SpdmMeasuring {
                spdm_measuring_state: SpdmMeasuringState::PollResult(job_id),
            },
        };
        let mut txn = env.test_harness.db_txn().await;
        // Replace setup evidence so each case controls every selected device's latest result.
        sqlx::query("DELETE FROM spdm_machine_devices_attestation WHERE machine_id = $1")
            .bind(machine_id)
            .execute(&mut *txn)
            .await?;
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        let selection = AttesterSelection {
            mode: AttesterSelectionMode::Allowlist,
            component_ids: vec![ComponentIdMatch::Prefix("HGX_IRoT_GPU".to_string())],
        };
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(selection),
            "test",
        )
        .await?;
        let current_time = Utc::now();
        for (device_id, state, row_job_id, started_at) in [
            (
                "HGX_IRoT_GPU_0",
                case.job_state,
                job_id,
                current_time - chrono::Duration::minutes(1),
            ),
            (
                "HGX_IRoT_GPU_2",
                SpdmAttestationState::Passed,
                job_id,
                current_time - chrono::Duration::minutes(1),
            ),
            (
                "HGX_IRoT_GPU_1",
                SpdmAttestationState::Passed,
                job_id,
                current_time - chrono::Duration::minutes(1),
            ),
            (
                "HGX_IRoT_GPU_1",
                SpdmAttestationState::Failed("historical failure".to_string()),
                historical_job_id,
                current_time - chrono::Duration::minutes(2),
            ),
            (
                "HGX_IRoT_GPU_0",
                case.gpu_0_state,
                current_job_id,
                current_time,
            ),
            (
                "HGX_IRoT_GPU_1",
                case.gpu_1_state,
                current_job_id,
                current_time,
            ),
            (
                "HGX_IRoT_GPU_2",
                SpdmAttestationState::Passed,
                current_job_id,
                current_time,
            ),
            (
                "ERoT_BMC_0",
                SpdmAttestationState::Failed("excluded failure".to_string()),
                current_job_id,
                current_time,
            ),
        ] {
            sqlx::query(
                "INSERT INTO spdm_machine_devices_attestation
                 (machine_id, device_id, nonce, state, job_id, started_at)
                 VALUES ($1, $2, gen_random_uuid(), $3, $4, $5)",
            )
            .bind(machine_id)
            .bind(device_id)
            .bind(sqlx::types::Json(state))
            .bind(row_job_id)
            .bind(started_at)
            .execute(&mut *txn)
            .await?;
        }
        db::machine::update_state(&mut txn, &mh.host.id, &poll_state).await?;
        txn.commit().await?;

        env.run_single_iteration().await;
        // Reload the state and persisted outcome to distinguish waiting from a handler error.
        let mut txn = env.test_harness.db_txn().await;
        let machine = mh.host.db_machine(&mut txn).await;
        let cancelled: bool = sqlx::query_scalar(
            "SELECT bool_or(cancelled_at IS NOT NULL) FROM spdm_machine_devices_attestation WHERE job_id = $1",
        )
        .bind(job_id)
        .fetch_one(&mut *txn)
        .await?;
        assert!(
            !cancelled,
            "{}: a matching job must not be cancelled",
            case.scenario
        );
        match case.expected {
            Expected::Advance => assert_eq!(
                machine.current_state(),
                &ManagedHostState::HostInit {
                    machine_state: MachineState::WaitingForDiscovery
                },
                "{}",
                case.scenario,
            ),
            Expected::Wait => {
                assert_eq!(machine.current_state(), &poll_state, "{}", case.scenario);
                assert!(
                    matches!(
                        machine.controller_state_outcome,
                        Some(PersistentStateHandlerOutcome::DoNothing { .. })
                    ),
                    "{}: {:?}",
                    case.scenario,
                    machine.controller_state_outcome
                );
            }
            Expected::Failed => {
                let ManagedHostState::Failed {
                    details:
                        FailureDetails {
                            cause: FailureCause::SpdmAttestationFailed { err },
                            source,
                            ..
                        },
                    retry_count,
                    machine_id: failed_machine_id,
                } = machine.current_state()
                else {
                    panic!("{}: {:?}", case.scenario, machine.current_state());
                };
                assert_eq!(
                    err, "Device: HGX_IRoT_GPU_1, failed reason: selected current failure. ",
                    "{}",
                    case.scenario
                );
                assert_eq!(
                    source,
                    &FailureSource::StateMachineArea(StateMachineArea::HostInit)
                );
                assert_eq!(*retry_count, 0);
                assert_eq!(*failed_machine_id, machine_id);
            }
        }
    }
    Ok(())
}

/// Membership changes cancel the old job before returning to the trigger state.
#[sqlx_test]
async fn changed_spdm_attesters_cancel_and_restart_jobs(pool: PgPool) -> eyre::Result<()> {
    struct Case {
        scenario: &'static str,
        job_devices: Vec<(&'static str, SpdmAttestationState)>,
        selected_devices: &'static [&'static str],
    }
    let cases = [
        Case {
            scenario: "an added attester restarts an active job even with results in another job",
            job_devices: vec![("HGX_IRoT_GPU_0", SpdmAttestationState::FetchMetadata)],
            selected_devices: &["HGX_IRoT_GPU_0", "HGX_IRoT_GPU_1"],
        },
        Case {
            scenario: "removing a failed attester cancels the terminal job",
            job_devices: vec![
                ("HGX_IRoT_GPU_0", SpdmAttestationState::Passed),
                (
                    "HGX_IRoT_GPU_1",
                    SpdmAttestationState::Failed("removed device failure".to_string()),
                ),
            ],
            selected_devices: &["HGX_IRoT_GPU_0"],
        },
        Case {
            scenario: "a replacement with the same device count still restarts the job",
            job_devices: vec![("HGX_IRoT_GPU_0", SpdmAttestationState::Passed)],
            selected_devices: &["HGX_IRoT_GPU_1"],
        },
        Case {
            scenario: "an empty selection cancels an active job and then skips without contacting the BMC",
            job_devices: vec![("HGX_IRoT_GPU_0", SpdmAttestationState::FetchMetadata)],
            selected_devices: &[],
        },
    ];
    let (mut env, mh) = setup_spdm_host(pool, true).await;
    let machine_id: MachineId = mh.host.id.into();
    let trigger_state = ManagedHostState::HostInit {
        machine_state: MachineState::SpdmMeasuring {
            spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
        },
    };

    for case in cases {
        env.redfish_sim.clear_create_client_error();
        let job_id = uuid::Uuid::new_v4();
        let other_job_id = uuid::Uuid::new_v4();
        let mut txn = env.test_harness.db_txn().await;
        // Isolate each selection change from jobs and policies left by earlier cases.
        sqlx::query("DELETE FROM spdm_machine_devices_attestation WHERE machine_id = $1")
            .bind(machine_id)
            .execute(&mut *txn)
            .await?;
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        let selection = AttesterSelection {
            mode: if case.selected_devices.is_empty() {
                AttesterSelectionMode::None
            } else {
                AttesterSelectionMode::Allowlist
            },
            component_ids: case
                .selected_devices
                .iter()
                .map(|id| ComponentIdMatch::Exact((*id).to_string()))
                .collect(),
        };
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(selection),
            "test",
        )
        .await?;
        for (device_id, state) in &case.job_devices {
            sqlx::query(
                "INSERT INTO spdm_machine_devices_attestation
                 (machine_id, device_id, nonce, state, job_id, started_at)
                 VALUES ($1, $2, gen_random_uuid(), $3, $4, now() - interval '1 minute')",
            )
            .bind(machine_id)
            .bind(device_id)
            .bind(sqlx::types::Json(state))
            .bind(job_id)
            .execute(&mut *txn)
            .await?;
        }
        // Existing evidence elsewhere cannot substitute for membership in the polled job.
        // This job must also remain uncancelled when the polled job is replaced.
        sqlx::query(
            "INSERT INTO spdm_machine_devices_attestation
             (machine_id, device_id, nonce, state, job_id, started_at)
             VALUES ($1, 'HGX_IRoT_GPU_1', gen_random_uuid(), $2, $3, now())",
        )
        .bind(machine_id)
        .bind(sqlx::types::Json(SpdmAttestationState::Passed))
        .bind(other_job_id)
        .execute(&mut *txn)
        .await?;
        db::machine::update_state(
            &mut txn,
            &mh.host.id,
            &ManagedHostState::HostInit {
                machine_state: MachineState::SpdmMeasuring {
                    spdm_measuring_state: SpdmMeasuringState::PollResult(job_id),
                },
            },
        )
        .await?;
        txn.commit().await?;
        if case.selected_devices.is_empty() {
            env.redfish_sim
                .set_create_client_error("the BMC cannot be reached");
        }

        env.run_single_iteration().await;
        // Reload both effects: cancellation and the lifecycle transition must be persisted together.
        let mut txn = env.test_harness.db_txn().await;
        let machine = mh.host.db_machine(&mut txn).await;
        assert_eq!(machine.current_state(), &trigger_state, "{}", case.scenario);
        let old_attestations =
            db::attestation::spdm::get_attestations_for_job_id(&mut txn, &job_id).await?;
        assert_eq!(
            old_attestations.len(),
            case.job_devices.len(),
            "{}",
            case.scenario
        );
        assert!(
            old_attestations
                .iter()
                .all(|attester| attester.cancelled_at.is_some()),
            "{}: every old job device must have a cancellation request",
            case.scenario
        );
        let other_attestations =
            db::attestation::spdm::get_attestations_for_job_id(&mut txn, &other_job_id).await?;
        assert!(
            other_attestations
                .iter()
                .all(|attester| attester.cancelled_at.is_none()),
            "{}: unrelated jobs must remain uncancelled",
            case.scenario
        );
        txn.commit().await?;

        env.run_single_iteration().await;
        // The trigger creates a distinct job for the current selection, or follows its empty-selection skip path.
        let mut txn = env.test_harness.db_txn().await;
        let machine = mh.host.db_machine(&mut txn).await;
        if case.selected_devices.is_empty() {
            assert_eq!(
                machine.current_state(),
                &ManagedHostState::HostInit {
                    machine_state: MachineState::WaitingForDiscovery,
                },
                "{}",
                case.scenario
            );
            let jobs: Vec<uuid::Uuid> = sqlx::query_scalar(
                "SELECT DISTINCT job_id FROM spdm_machine_devices_attestation WHERE machine_id = $1",
            ).bind(machine_id).fetch_all(&mut *txn).await?;
            assert_eq!(
                jobs.into_iter().collect::<BTreeSet<_>>(),
                BTreeSet::from([job_id, other_job_id]),
                "{}: no replacement job should be created for an empty selection",
                case.scenario
            );
        } else {
            let ManagedHostState::HostInit {
                machine_state:
                    MachineState::SpdmMeasuring {
                        spdm_measuring_state: SpdmMeasuringState::PollResult(new_job_id),
                    },
            } = machine.current_state()
            else {
                panic!("{}: {:?}", case.scenario, machine.current_state());
            };
            assert_ne!(*new_job_id, job_id, "{}", case.scenario);
            assert_ne!(*new_job_id, other_job_id, "{}", case.scenario);
            let new_attestations =
                db::attestation::spdm::get_attestations_for_job_id(&mut txn, new_job_id).await?;
            // The replacement is scheduled by the managed-host controller for every selected device.
            let origins: Vec<bool> = sqlx::query_scalar(
                "SELECT triggered_by_mh_controller FROM spdm_machine_devices_attestation
                 WHERE machine_id = $1 AND job_id = $2",
            )
            .bind(machine_id)
            .bind(new_job_id)
            .fetch_all(&mut *txn)
            .await?;
            assert_eq!(
                origins,
                vec![true; case.selected_devices.len()],
                "{}: every replacement device must be marked as controller-triggered",
                case.scenario
            );
            assert_eq!(
                new_attestations
                    .iter()
                    .map(|attester| attester.device_id.as_str())
                    .collect::<BTreeSet<_>>(),
                case.selected_devices
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>(),
                "{}",
                case.scenario
            );
        }
    }
    Ok(())
}

/// Recovery uses current selection and advances without repeating completed or waived work.
#[sqlx_test]
async fn failed_spdm_recovery_checks_current_attesters(pool: PgPool) -> eyre::Result<()> {
    struct Case {
        scenario: &'static str,
        states: [Option<SpdmAttestationState>; 3],
        source: StateMachineArea,
        expected: Option<ManagedHostState>,
        selection_mode: AttesterSelectionMode,
    }
    let passed_case = || Case {
        scenario: "a stale failed device absent from the BMC does not block selected passes",
        states: std::array::from_fn(|_| Some(SpdmAttestationState::Passed)),
        source: StateMachineArea::HostInit,
        expected: Some(ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        }),
        selection_mode: AttesterSelectionMode::Allowlist,
    };
    let (mut env, mh) = setup_spdm_host(pool, true).await;
    let machine_id: MachineId = mh.host.id.into();

    for case in [
        passed_case(),
        Case {
            scenario: "passed and cancelled devices resume assignment without another job",
            states: [
                Some(SpdmAttestationState::Passed),
                Some(SpdmAttestationState::Cancelled),
                Some(SpdmAttestationState::Passed),
            ],
            source: StateMachineArea::MainFlow,
            expected: Some(ManagedHostState::StartAssignmentCycle),
            ..passed_case()
        },
        Case {
            scenario: "all cancelled devices resume deprovisioning without another job",
            states: std::array::from_fn(|_| Some(SpdmAttestationState::Cancelled)),
            source: StateMachineArea::AssignedInstance,
            expected: Some(ManagedHostState::WaitingForCleanup {
                cleanup_state: CleanupState::Init,
                cleanup_context: CleanupContext::Deprovision,
            }),
            ..passed_case()
        },
        Case {
            scenario: "cancellation does not override a selected failure",
            states: [
                Some(SpdmAttestationState::Cancelled),
                Some(SpdmAttestationState::Failed("still failing".to_string())),
                Some(SpdmAttestationState::Passed),
            ],
            expected: None,
            ..passed_case()
        },
        Case {
            scenario: "unfinished selected work keeps recovery waiting",
            states: [
                Some(SpdmAttestationState::Passed),
                Some(SpdmAttestationState::FetchMetadata),
                Some(SpdmAttestationState::Passed),
            ],
            expected: None,
            ..passed_case()
        },
        Case {
            scenario: "a selected device without results keeps recovery waiting",
            states: [
                Some(SpdmAttestationState::Passed),
                None,
                Some(SpdmAttestationState::Passed),
            ],
            expected: None,
            ..passed_case()
        },
        Case {
            scenario: "an empty selection advances without BMC access or results",
            states: [None, None, None],
            selection_mode: AttesterSelectionMode::None,
            ..passed_case()
        },
    ] {
        env.redfish_sim.clear_create_client_error();
        let failed_state = ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::SpdmAttestationFailed {
                    err: "previous failure".to_string(),
                },
                failed_at: Utc::now(),
                source: FailureSource::StateMachineArea(case.source),
            },
            retry_count: 7,
            machine_id,
        };
        let mut txn = env.test_harness.db_txn().await;
        // The removed device still matches the profile, but is absent from BMC inventory.
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        let skip_bmc = case.selection_mode == AttesterSelectionMode::None;
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(AttesterSelection {
                mode: case.selection_mode,
                component_ids: if skip_bmc {
                    Vec::new()
                } else {
                    vec![ComponentIdMatch::Prefix("HGX_IRoT_GPU".to_string())]
                },
            }),
            "test",
        )
        .await?;
        sqlx::query("DELETE FROM spdm_machine_devices_attestation WHERE machine_id = $1")
            .bind(machine_id)
            .execute(&mut *txn)
            .await?;
        let job_id = uuid::Uuid::new_v4();
        for (device_id, state) in [
            ("HGX_IRoT_GPU_0", case.states[0].clone()),
            ("HGX_IRoT_GPU_1", case.states[1].clone()),
            ("HGX_IRoT_GPU_2", case.states[2].clone()),
            (
                "HGX_IRoT_GPU_removed",
                Some(SpdmAttestationState::Failed(
                    "stale device failure".to_string(),
                )),
            ),
        ] {
            let Some(state) = state else { continue };
            sqlx::query(
                "INSERT INTO spdm_machine_devices_attestation
                 (machine_id, device_id, nonce, state, job_id, started_at)
                 VALUES ($1, $2, gen_random_uuid(), $3, $4, now())",
            )
            .bind(machine_id)
            .bind(device_id)
            .bind(sqlx::types::Json(state))
            .bind(job_id)
            .execute(&mut *txn)
            .await?;
        }
        db::machine::update_state(&mut txn, &mh.host.id, &failed_state).await?;
        txn.commit().await?;
        if skip_bmc {
            env.redfish_sim
                .set_create_client_error("the BMC cannot be reached");
        }

        env.run_single_iteration().await;
        // Reload state and outcome so an error cannot masquerade as blocked recovery.
        let mut txn = env.test_harness.db_txn().await;
        let machine = mh.host.db_machine(&mut txn).await;
        assert_eq!(
            machine.current_state(),
            case.expected.as_ref().unwrap_or(&failed_state),
            "{}",
            case.scenario
        );
        if case.expected.is_none() {
            // Failed states have a zero SLA, so a successful no-op records an SLA error.
            let expected_error = StateHandlerError::TimeInStateAboveSla {
                handler_outcome: "DoNothing".to_string(),
            }
            .to_string();
            assert!(
                matches!(
                    machine.controller_state_outcome.as_ref(),
                    Some(PersistentStateHandlerOutcome::Error { err, .. }) if err == &expected_error
                ),
                "{}: {:?}",
                case.scenario,
                machine.controller_state_outcome
            );
        }
        // Recovery consumes existing results without inserting rows into this or another job.
        let (rows, jobs): (i64, i64) = sqlx::query_as(
            "SELECT count(*), count(DISTINCT job_id)
             FROM spdm_machine_devices_attestation WHERE machine_id = $1",
        )
        .bind(machine_id)
        .fetch_one(&mut *txn)
        .await?;
        assert_eq!(
            rows,
            1 + case.states.iter().flatten().count() as i64,
            "{}",
            case.scenario
        );
        assert_eq!(jobs, 1, "{}", case.scenario);
    }
    Ok(())
}

/// Disabling SPDM releases failed hosts even when evidence and the BMC are unavailable.
#[sqlx_test]
async fn disabled_spdm_recovery_skips_attestation(pool: PgPool) -> eyre::Result<()> {
    let (mut env, mh) = setup_spdm_host(pool, false).await;
    let machine_id: MachineId = mh.host.id.into();
    let mut txn = env.test_harness.db_txn().await;
    // No attestation evidence can satisfy recovery; only the site setting permits it.
    sqlx::query("DELETE FROM spdm_machine_devices_attestation WHERE machine_id = $1")
        .bind(machine_id)
        .execute(&mut *txn)
        .await?;
    db::machine::update_state(
        &mut txn,
        &mh.host.id,
        &ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::SpdmAttestationFailed {
                    err: "previous failure".to_string(),
                },
                failed_at: Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
            },
            retry_count: 7,
            machine_id,
        },
    )
    .await?;
    txn.commit().await?;
    env.redfish_sim
        .set_create_client_error("the BMC cannot be reached");

    env.run_single_iteration().await;
    // Reload the persisted destination to prove recovery skipped scheduling entirely.
    let mut txn = env.test_harness.db_txn().await;
    let machine = mh.host.db_machine(&mut txn).await;
    assert_eq!(
        machine.current_state(),
        &ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        }
    );
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM spdm_machine_devices_attestation WHERE machine_id = $1",
    )
    .bind(machine_id)
    .fetch_one(&mut *txn)
    .await?;
    assert_eq!(rows, 0);
    Ok(())
}
