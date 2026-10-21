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
    FailureCause, FailureDetails, FailureSource, MachineState, ManagedHostState,
    SpdmMeasuringState, StateMachineArea,
};
use model::test_support::ManagedHostConfig;
use state_controller::state_handler::StateHandlerError;

use crate::env::Env;

// Both polling and recovery need a discovered host with SPDM enabled and a working BMC.
async fn setup_spdm_host(pool: PgPool) -> (Env, TestManagedHost) {
    let env = Env::builder(pool)
        .configure_runtime(|config| config.spdm.enabled = true)
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

/// Polling must use current selected results across jobs, including for failure details.
#[sqlx_test]
async fn terminal_spdm_jobs_recheck_current_attesters(pool: sqlx::PgPool) -> eyre::Result<()> {
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
        disable_policy: bool,
        expected: Expected,
    }
    let cases = [
        Case {
            scenario: "a cancelled job reports only current selected failures from another job",
            job_state: SpdmAttestationState::Cancelled,
            gpu_0_state: SpdmAttestationState::Passed,
            gpu_1_state: SpdmAttestationState::Failed("selected current failure".to_string()),
            disable_policy: false,
            expected: Expected::Failed,
        },
        Case {
            scenario: "newer selected passes supersede a failed job and excluded failures",
            job_state: SpdmAttestationState::Failed("superseded job failure".to_string()),
            gpu_0_state: SpdmAttestationState::Passed,
            gpu_1_state: SpdmAttestationState::Passed,
            disable_policy: false,
            expected: Expected::Advance,
        },
        Case {
            scenario: "a cancelled machine advances after a passed job",
            job_state: SpdmAttestationState::Passed,
            gpu_0_state: SpdmAttestationState::Cancelled,
            gpu_1_state: SpdmAttestationState::Passed,
            disable_policy: false,
            expected: Expected::Advance,
        },
        Case {
            scenario: "a passed job waits for newer selected work",
            job_state: SpdmAttestationState::Passed,
            gpu_0_state: SpdmAttestationState::FetchMetadata,
            gpu_1_state: SpdmAttestationState::Passed,
            disable_policy: false,
            expected: Expected::Wait,
        },
        Case {
            scenario: "a disabled policy advances without contacting the BMC",
            job_state: SpdmAttestationState::Failed("obsolete failure".to_string()),
            gpu_0_state: SpdmAttestationState::Passed,
            gpu_1_state: SpdmAttestationState::Passed,
            disable_policy: true,
            expected: Expected::Advance,
        },
    ];
    let (mut env, mh) = setup_spdm_host(pool).await;
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
        let selection = if case.disable_policy {
            AttesterSelection {
                mode: AttesterSelectionMode::None,
                component_ids: Vec::new(),
            }
        } else {
            AttesterSelection {
                mode: AttesterSelectionMode::Allowlist,
                component_ids: vec![ComponentIdMatch::Prefix("HGX_IRoT_GPU".to_string())],
            }
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
                "HGX_IRoT_GPU_1",
                SpdmAttestationState::Failed("historical failure".to_string()),
                historical_job_id,
                current_time - chrono::Duration::minutes(1),
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
        if case.disable_policy {
            env.redfish_sim
                .set_create_client_error("the BMC cannot be reached");
        }

        env.run_single_iteration().await;
        // Reload the state and persisted outcome to distinguish waiting from a handler error.
        let mut txn = env.test_harness.db_txn().await;
        let machine = mh.host.db_machine(&mut txn).await;
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

/// Recovery follows current BMC inventory, and cancellation cannot stand in for a pass.
#[sqlx_test]
async fn failed_spdm_recovery_checks_current_attesters(pool: PgPool) -> eyre::Result<()> {
    struct Case {
        scenario: &'static str,
        selected_state: SpdmAttestationState,
        resume: bool,
    }
    let (mut env, mh) = setup_spdm_host(pool).await;
    let machine_id: MachineId = mh.host.id.into();
    let failed_state = ManagedHostState::Failed {
        details: FailureDetails {
            cause: FailureCause::SpdmAttestationFailed {
                err: "previous failure".to_string(),
            },
            failed_at: Utc::now(),
            source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
        },
        retry_count: 7,
        machine_id,
    };

    for case in [
        Case {
            scenario: "a stale failed device absent from the BMC does not block selected passes",
            selected_state: SpdmAttestationState::Passed,
            resume: true,
        },
        Case {
            scenario: "a selected cancellation keeps the machine failed",
            selected_state: SpdmAttestationState::Cancelled,
            resume: false,
        },
    ] {
        let mut txn = env.test_harness.db_txn().await;
        // The removed device still matches the profile, but is absent from the BMC's inventory.
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(AttesterSelection {
                mode: AttesterSelectionMode::Allowlist,
                component_ids: vec![ComponentIdMatch::Prefix("HGX_IRoT_GPU".to_string())],
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
            ("HGX_IRoT_GPU_0", SpdmAttestationState::Passed),
            ("HGX_IRoT_GPU_1", case.selected_state),
            ("HGX_IRoT_GPU_2", SpdmAttestationState::Passed),
            (
                "HGX_IRoT_GPU_removed",
                SpdmAttestationState::Failed("stale device failure".to_string()),
            ),
        ] {
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

        env.run_single_iteration().await;
        // Reload both the state and outcome so an error cannot masquerade as blocked recovery.
        let mut txn = env.test_harness.db_txn().await;
        let machine = mh.host.db_machine(&mut txn).await;
        if case.resume {
            assert_eq!(
                machine.current_state(),
                &ManagedHostState::HostInit {
                    machine_state: MachineState::SpdmMeasuring {
                        spdm_measuring_state: SpdmMeasuringState::TriggerMeasurements,
                    },
                },
                "{}",
                case.scenario
            );
        } else {
            assert_eq!(machine.current_state(), &failed_state, "{}", case.scenario);
            // Failed states have a zero SLA, so the controller records this successful no-op as an SLA error.
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
    }
    Ok(())
}
