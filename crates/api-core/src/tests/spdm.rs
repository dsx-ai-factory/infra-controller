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
pub(in crate::tests) mod tests {

    use carbide_spdm_controller::io::SpdmStateControllerIO;
    use carbide_uuid::machine::MachineId;
    use chrono::{DateTime, Utc};
    use config_version::ConfigVersion;
    use model::attestation::profile::{
        ANY_HARDWARE_CLASS, AttestationPolicyDocument, AttesterSelection, AttesterSelectionMode,
        ComponentIdMatch,
    };
    use model::attestation::spdm::{SpdmAttestationState, SpdmCancellationTarget, SpdmObjectId};
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use rpc::forge::forge_server::Forge;
    use rpc::forge::list_machine_attestation_statuses_request::Target;
    use rpc::forge::{
        ListMachineAttestationStatusesRequest, SpdmAttestationJobStatus, SpdmAttestationStatus,
        SpdmAttesterSelectionStatus, SpdmListAttestationRequest,
        SpdmListAttestationRequestSelector, SpdmMachineAttestationMachineRequest,
        SpdmMachineAttestationTriggerRequest, SpdmSchedulingOutcome,
    };
    use sqlx::PgConnection;
    use state_controller::io::StateControllerIO;
    use tonic::Request;

    use crate::cfg::file::CarbideConfig;
    use crate::test_support::fixture_config::MOCK_HOST_HARDWARE_CLASS;
    use crate::tests::common::api_fixtures::{
        TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
        create_test_env_with_overrides, get_config,
    };

    fn machine_assessment_request(machine_id: MachineId) -> ListMachineAttestationStatusesRequest {
        ListMachineAttestationStatusesRequest {
            target: Some(Target::Machine(SpdmMachineAttestationMachineRequest {
                machine_id: Some(machine_id),
                redfish_timeout_secs: None,
            })),
        }
    }

    /// The default test config leaves SPDM disabled, matching a fresh
    /// deployment. `trigger_machine_attestation` refuses to schedule at a site
    /// with it switched off, so every test that drives a trigger turns it on.
    ///
    /// Enabling it also makes `create_managed_host` attest the machine during
    /// host init. Tests that model a misbehaving BMC therefore inject the fault
    /// after setup, so it applies to the trigger under test rather than to that
    /// unrelated attestation.
    fn spdm_enabled_config() -> CarbideConfig {
        let mut config = get_config();
        config.spdm.enabled = true;
        config
    }

    /// The fixture seeds a profile for the mock host's class, and creation
    /// rejects a duplicate class, so seeding unconditionally would panic the
    /// moment a test built a second environment on one pool. Nothing else
    /// holds the fixture to seeding only when the profile is absent.
    #[crate::sqlx_test]
    async fn a_second_spdm_environment_reuses_the_seeded_profile(pool: sqlx::PgPool) {
        for _ in 0..2 {
            let mut overrides = TestEnvOverrides::no_network_segments();
            overrides.config = Some(spdm_enabled_config());
            let _env = create_test_env_with_overrides(pool.clone(), overrides).await;
        }
    }

    #[crate::sqlx_test]
    async fn test_spdm_controller_persistence_honors_versions_and_terminal_results(
        pool: sqlx::PgPool,
    ) {
        let env = create_test_env(pool).await;
        let machine_id = create_managed_host(&env).await.host().id.into();
        let job_id = uuid::Uuid::new_v4();
        let object_id = SpdmObjectId(machine_id, "HGX_IRoT_GPU_0".to_string(), job_id);
        let old_version = ConfigVersion::initial();

        sqlx::query(
            "INSERT INTO spdm_machine_devices_attestation \
             (machine_id, device_id, nonce, state, state_version, job_id, started_at) \
             VALUES ($1, $2, gen_random_uuid(), $3, $4, $5, now())",
        )
        .bind(machine_id)
        .bind(&object_id.1)
        .bind(sqlx::types::Json(SpdmAttestationState::FetchMetadata))
        .bind(old_version)
        .bind(job_id)
        .execute(&env.pool)
        .await
        .unwrap();

        // Another iteration commits before the stale transition is persisted.
        let current_version = old_version.increment();
        let mut txn = env.db_txn().await;
        assert_eq!(
            db::attestation::spdm::persist_controller_state(
                &mut txn,
                &object_id,
                old_version,
                current_version,
                &SpdmAttestationState::FetchCertificate,
            )
            .await
            .unwrap(),
            db::ConditionalWrite::Applied(())
        );
        txn.commit().await.unwrap();

        // The same-state case pins the persistence contract, not an SPDM
        // handler path. Use a distinct replacement so IO cannot recompute it.
        let replacement_version = current_version.increment().increment();
        struct Case {
            scenario: &'static str,
            device_id: &'static str,
            old_version: ConfigVersion,
            state: SpdmAttestationState,
            expected: db::ConditionalWrite<(), db::ControllerStateNotCurrent>,
            stored_state: Option<(SpdmAttestationState, ConfigVersion)>,
        }
        let io = SpdmStateControllerIO::default();
        for case in [
            Case {
                scenario: "stale iteration cannot replace the committed state",
                device_id: "HGX_IRoT_GPU_0",
                old_version,
                state: SpdmAttestationState::Passed,
                expected: db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent),
                stored_state: Some((SpdmAttestationState::FetchCertificate, current_version)),
            },
            Case {
                scenario: "persistence stores the supplied version for the same state",
                device_id: "HGX_IRoT_GPU_0",
                old_version: current_version,
                state: SpdmAttestationState::FetchCertificate,
                expected: db::ConditionalWrite::Applied(()),
                stored_state: Some((SpdmAttestationState::FetchCertificate, replacement_version)),
            },
            Case {
                scenario: "missing device rejects the transition",
                device_id: "missing-device",
                old_version: replacement_version,
                state: SpdmAttestationState::Passed,
                expected: db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent),
                stored_state: None,
            },
        ] {
            let mut txn = env.db_txn().await;
            let write = io
                .persist_controller_state(
                    &mut txn,
                    &SpdmObjectId(machine_id, case.device_id.to_string(), job_id),
                    case.old_version,
                    replacement_version,
                    &case.state,
                )
                .await
                .unwrap();
            txn.commit().await.unwrap();
            assert_eq!(write, case.expected, "{}", case.scenario);

            let stored_state: Option<(sqlx::types::Json<SpdmAttestationState>, ConfigVersion)> =
                sqlx::query_as(
                    "SELECT state, state_version FROM spdm_machine_devices_attestation \
                     WHERE machine_id = $1 AND device_id = $2 AND job_id = $3",
                )
                .bind(machine_id)
                .bind(case.device_id)
                .bind(job_id)
                .fetch_optional(&env.pool)
                .await
                .unwrap();
            assert_eq!(
                stored_state.map(|(state, version)| (state.0, version)),
                case.stored_state,
                "{}",
                case.scenario
            );
        }

        // A request records intent without invalidating an in-flight transition.
        env.api
            .fail_attestation(Request::new(job_id.into()))
            .await
            .unwrap();
        let terminal_version = replacement_version.increment();
        let mut txn = env.db_txn().await;
        assert_eq!(
            io.persist_controller_state(
                &mut txn,
                &object_id,
                replacement_version,
                terminal_version,
                &SpdmAttestationState::Passed,
            )
            .await
            .unwrap(),
            db::ConditionalWrite::Applied(())
        );
        txn.commit().await.unwrap();

        // Terminal results stay final even with a pending marker and no completion
        // timestamp. Explicit queueing also exercises work discovered before completion.
        for state in [
            SpdmAttestationState::Passed,
            SpdmAttestationState::Failed("verification failed".to_string()),
            SpdmAttestationState::Waived,
        ] {
            sqlx::query(
                "UPDATE spdm_machine_devices_attestation
                SET state = $2, completed_at = NULL WHERE job_id = $1",
            )
            .bind(job_id)
            .bind(sqlx::types::Json(&state))
            .execute(&env.pool)
            .await
            .unwrap();
            let mut txn = env.db_txn().await;
            db::attestation::spdm::request_attestation_cancellation(
                &mut txn,
                &job_id,
                SpdmCancellationTarget::Fail,
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();
            state_controller::controller::Enqueuer::<SpdmStateControllerIO>::new(env.pool.clone())
                .enqueue_object(&object_id)
                .await
                .unwrap();
            env.run_spdm_controller_iteration_no_requeue().await;
            let mut txn = env.db_txn().await;
            let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                &object_id.1,
                &job_id,
            )
            .await
            .unwrap();
            assert_eq!(snapshot.state, state);
            assert_eq!(snapshot.state_version, terminal_version);
            assert!(snapshot.completed_at.is_some());
            assert_eq!(
                snapshot.cancellation_target,
                Some(SpdmCancellationTarget::Fail)
            );
            assert!(matches!(
                snapshot.state_outcome,
                Some(PersistentStateHandlerOutcome::DoNothing { .. })
            ));
        }
    }

    /// With SPDM off no state controller is spawned, so anything this scheduled
    /// would sit unprocessed forever. The "enabled" half of the contract is
    /// covered by every other test in this module, which all schedule
    /// successfully with SPDM on.
    #[crate::sqlx_test]
    async fn trigger_is_refused_when_spdm_is_disabled_for_the_site(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env(pool).await;
        assert!(
            !env.config.spdm.enabled,
            "this test relies on the default config leaving SPDM disabled"
        );

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
        let status = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await
            .expect_err("a site with SPDM disabled must not start attestation");

        assert_eq!(tonic::Code::Unavailable, status.code());

        // Refusing has to leave no work behind: a caller that retries after
        // enabling SPDM should start from nothing.
        assert_eq!(0, list_machines_under_attestation(&env).await?.len());

        env.redfish_sim
            .set_create_client_error("disabled assessment must not contact BMCs");
        let assessment = env
            .api
            .list_machine_attestation_statuses(Request::new(machine_assessment_request(
                machine_id.into(),
            )))
            .await?
            .into_inner();
        assert_eq!(assessment.statuses.len(), 1);
        assert!(assessment.statuses[0].attestation_status.is_none());
        assert_eq!(
            assessment.statuses[0].attester_selection_status(),
            SpdmAttesterSelectionStatus::Disabled
        );

        Ok(())
    }

    /// What the trigger reports for each outcome a profile can produce.
    ///
    /// The machine moves on to the same next state no matter which of these
    /// happened: the profile disabled attestation, it named attesters the BMC
    /// does not have, or nobody wrote one. Only the response tells them apart.
    #[crate::sqlx_test]
    async fn trigger_reports_the_profile_that_decided(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        struct Case {
            scenario: &'static str,
            /// Replaces the profile the fixture seeded for the mock host's
            /// class, run before the trigger.
            reprofile: fn() -> Vec<(String, AttesterSelection)>,
            expect_outcome: SpdmSchedulingOutcome,
            expect_devices: i32,
            expect_fallback: bool,
        }

        let cases = [
            Case {
                scenario: "a profile written for the class attests what it names",
                reprofile: || vec![(MOCK_HOST_HARDWARE_CLASS.to_string(), gpu_allowlist())],
                expect_outcome: SpdmSchedulingOutcome::Scheduled,
                expect_devices: 3,
                expect_fallback: false,
            },
            Case {
                scenario: "the any profile covers a class nobody wrote one for",
                reprofile: || vec![(ANY_HARDWARE_CLASS.to_string(), gpu_allowlist())],
                expect_outcome: SpdmSchedulingOutcome::Scheduled,
                expect_devices: 3,
                expect_fallback: true,
            },
            Case {
                scenario: "NONE attests nothing without contacting the BMC",
                reprofile: || {
                    vec![(
                        MOCK_HOST_HARDWARE_CLASS.to_string(),
                        AttesterSelection {
                            mode: AttesterSelectionMode::None,
                            component_ids: Vec::new(),
                        },
                    )]
                },
                expect_outcome: SpdmSchedulingOutcome::AttestationDisabled,
                expect_devices: 0,
                expect_fallback: false,
            },
            Case {
                scenario: "an unprofiled class names the missing profile",
                reprofile: Vec::new,
                expect_outcome: SpdmSchedulingOutcome::NoProfile,
                expect_devices: 0,
                expect_fallback: false,
            },
            Case {
                scenario: "a profile naming absent attesters is not silent",
                reprofile: || {
                    vec![(
                        MOCK_HOST_HARDWARE_CLASS.to_string(),
                        AttesterSelection {
                            mode: AttesterSelectionMode::Allowlist,
                            component_ids: vec![ComponentIdMatch::Exact("NOT_PRESENT".to_string())],
                        },
                    )]
                },
                expect_outcome: SpdmSchedulingOutcome::PolicyMatchedNothing,
                expect_devices: 0,
                expect_fallback: false,
            },
            Case {
                // The absent pattern is reported without costing the GPUs the
                // present one matched, which is the whole point of keeping this
                // apart from PolicyMatchedNothing.
                scenario: "one absent pattern does not stop the attesters that are there",
                reprofile: || {
                    vec![(
                        MOCK_HOST_HARDWARE_CLASS.to_string(),
                        AttesterSelection {
                            mode: AttesterSelectionMode::Allowlist,
                            component_ids: vec![
                                ComponentIdMatch::Prefix("HGX_IRoT_GPU".to_string()),
                                ComponentIdMatch::Exact("NOT_PRESENT".to_string()),
                            ],
                        },
                    )]
                },
                expect_outcome: SpdmSchedulingOutcome::PartiallySatisfied,
                expect_devices: 3,
                expect_fallback: false,
            },
        ];

        // One host, reused: the fixture attests it during init, which needs
        // the profile it seeded, so every case rewrites the profiles only
        // after the host exists.
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        for case in cases {
            // Isolate each policy case from the jobs scheduled by earlier cases.
            let mut txn = env.pool.begin().await?;
            sqlx::query("DELETE FROM attestation_profiles")
                .execute(&mut *txn)
                .await?;
            sqlx::query("DELETE FROM spdm_machine_devices_attestation")
                .execute(&mut *txn)
                .await?;
            for (hardware_class, selection) in (case.reprofile)() {
                db::attestation_profile::create(
                    &mut txn,
                    &hardware_class,
                    &AttestationPolicyDocument::new(selection),
                    "test",
                )
                .await?;
            }
            txn.commit().await?;

            let response = env
                .api
                .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                    machine_id: Some(machine_id.into()),
                    redfish_timeout_secs: u32::MAX,
                }))
                .await?
                .into_inner();

            assert_eq!(case.expect_outcome, response.outcome(), "{}", case.scenario);
            assert_eq!(
                case.expect_devices, response.devices_under_attestation,
                "{}",
                case.scenario
            );
            assert_eq!(
                case.expect_fallback, response.used_any_fallback,
                "{}",
                case.scenario
            );
            assert_eq!(
                MOCK_HOST_HARDWARE_CLASS, response.resolved_hardware_class,
                "{}: the class is reported whether or not it had a profile",
                case.scenario
            );
            assert_eq!(
                case.expect_outcome != SpdmSchedulingOutcome::NoProfile,
                response.profile_version.is_some(),
                "{}: a version is reported exactly when a profile decided",
                case.scenario
            );

            // The reported instant has to be the one on the rows, or a caller
            // cannot use it to tell its own scheduling from a later one.
            let started_at: Option<DateTime<Utc>> = sqlx::query_scalar(
                "SELECT DISTINCT started_at FROM spdm_machine_devices_attestation
                 WHERE machine_id = $1",
            )
            .bind(machine_id)
            .fetch_optional(&env.pool)
            .await?;
            assert_eq!(
                started_at.map(rpc::Timestamp::from),
                response.started_at,
                "{}: the response must report the rows' own timestamp",
                case.scenario
            );

            // Check every scheduled row: external callers must not supply controller provenance.
            let origins: Vec<bool> = sqlx::query_scalar(
                "SELECT triggered_by_mh_controller FROM spdm_machine_devices_attestation
                 WHERE machine_id = $1",
            )
            .bind(machine_id)
            .fetch_all(&env.pool)
            .await?;
            assert_eq!(
                origins,
                vec![false; case.expect_devices as usize],
                "{}: gRPC-triggered devices must not be marked as controller-triggered",
                case.scenario
            );
        }

        Ok(())
    }

    /// The outcomes a profile settles on its own report the same answer over a
    /// BMC that cannot be reached, which is what makes them answerable without
    /// it. Client creation is forced to fail for the duration, so contacting
    /// the BMC at all would surface as an error instead of the outcome.
    ///
    /// Both early returns are covered: `NoProfile` leaves during profile
    /// resolution, `AttestationDisabled` after it. The site every deployment
    /// starts as, with no profiles written, is the first of them.
    #[crate::sqlx_test]
    async fn the_outcomes_a_profile_settles_never_reach_the_bmc(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        struct Case {
            scenario: &'static str,
            profiles: fn() -> Vec<(String, AttesterSelection)>,
            expect_outcome: SpdmSchedulingOutcome,
            expect_selection: SpdmAttesterSelectionStatus,
        }

        let cases = [
            Case {
                scenario: "NONE is answered from the profile alone",
                profiles: || {
                    vec![(
                        MOCK_HOST_HARDWARE_CLASS.to_string(),
                        AttesterSelection {
                            mode: AttesterSelectionMode::None,
                            component_ids: Vec::new(),
                        },
                    )]
                },
                expect_outcome: SpdmSchedulingOutcome::AttestationDisabled,
                expect_selection: SpdmAttesterSelectionStatus::Disabled,
            },
            Case {
                scenario: "a class nobody profiled has nothing to ask the BMC for",
                profiles: Vec::new,
                expect_outcome: SpdmSchedulingOutcome::NoProfile,
                expect_selection: SpdmAttesterSelectionStatus::NoProfile,
            },
        ];

        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        // After setup, which attests the host over a BMC that answers.
        env.redfish_sim
            .set_create_client_error("the BMC cannot be reached");

        for case in cases {
            let mut txn = env.pool.begin().await?;
            sqlx::query("DELETE FROM attestation_profiles")
                .execute(&mut *txn)
                .await?;
            for (hardware_class, selection) in (case.profiles)() {
                db::attestation_profile::create(
                    &mut txn,
                    &hardware_class,
                    &AttestationPolicyDocument::new(selection),
                    "test",
                )
                .await?;
            }
            txn.commit().await?;

            let response = env
                .api
                .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                    machine_id: Some(machine_id.into()),
                    redfish_timeout_secs: u32::MAX,
                }))
                .await?
                .into_inner();

            assert_eq!(case.expect_outcome, response.outcome(), "{}", case.scenario);
            let assessment = env
                .api
                .list_machine_attestation_statuses(Request::new(machine_assessment_request(
                    machine_id.into(),
                )))
                .await?
                .into_inner();
            assert_eq!(assessment.statuses.len(), 1, "{}", case.scenario);
            let row = &assessment.statuses[0];
            assert!(row.attestation_status.is_none(), "{}", case.scenario);
            assert_eq!(
                row.attester_selection_status(),
                case.expect_selection,
                "{}",
                case.scenario
            );
        }

        Ok(())
    }

    /// The other half of that contract: a profile that does need the BMC
    /// still fails when it cannot be reached, reported as the Redfish problem
    /// it is so an operator gets the reachability mitigation rather than a
    /// generic attestation failure.
    #[crate::sqlx_test]
    async fn a_profile_needing_an_unreachable_bmc_reports_a_redfish_error(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let mut txn = env.pool.begin().await?;
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(gpu_allowlist()),
            "test",
        )
        .await?;
        txn.commit().await?;

        env.redfish_sim
            .set_create_client_error("the BMC cannot be reached");

        let status = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await
            .expect_err("a profile naming attesters cannot be satisfied without the BMC");

        let assessment_error = env
            .api
            .list_machine_attestation_statuses(Request::new(machine_assessment_request(
                machine_id.into(),
            )))
            .await
            .expect_err("a definitive assessment must report BMC failures");
        for status in [status, assessment_error] {
            assert_eq!(
                Some("NICO-REDFISH-500"),
                status
                    .metadata()
                    .get("nico-error-code")
                    .map(|code| code.to_str().expect("the error code is ASCII"))
            );
        }
        Ok(())
    }

    fn gpu_allowlist() -> AttesterSelection {
        AttesterSelection {
            mode: AttesterSelectionMode::Allowlist,
            component_ids: vec![ComponentIdMatch::Prefix("HGX_IRoT_GPU".to_string())],
        }
    }

    #[crate::sqlx_test]
    async fn test_component_integrity_fails_no_attestation_started(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        // set up redfish to return no component integrities
        env.redfish_sim.set_no_component_integrities(true);

        let response = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await?;

        assert!(response.into_inner().job_id.is_none());

        // device attestations should not be created
        let machine_ids = list_machines_under_attestation(&env).await?;

        assert_eq!(0, machine_ids.len());

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_fetch_metadata_fails_state_does_not_change(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        // set up redfish to return an error in FetchMetadata state
        env.redfish_sim.set_firmware_for_component_error(true);

        let response = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await?;

        assert_eq!(3, response.into_inner().devices_under_attestation);

        // device attestations should be created
        let machine_ids = list_machines_under_attestation(&env).await?;

        assert_eq!(1, machine_ids.len());

        // redfish will return an error
        let mut txn = env.pool.begin().await.unwrap();

        let object_ids = db::attestation::spdm::find_machine_ids_for_attestation(&mut txn)
            .await
            .expect("Failed getting object ids for attestation");

        for _ in 0..5 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert!(
                matches!(attestation_state, SpdmAttestationState::FetchMetadata),
                "expected FetchMetadata, got: {:?}",
                attestation_state
            );
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_poll_evidence_fails_controller_retries_then_fails(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;

        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();

        let response = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await?;

        assert_eq!(3, response.into_inner().devices_under_attestation);

        // device attestations should be created
        let machine_ids = list_machines_under_attestation(&env).await?;

        assert_eq!(1, machine_ids.len());

        let mut txn = env.pool.begin().await.unwrap();

        // Reach polling, then model a BMC that removed tasks after cancellation
        // completed without the controller's waiting-state transition being saved.
        for _ in 0..3 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        let object_ids = db::attestation::spdm::find_machine_ids_for_attestation(&mut txn)
            .await
            .expect("Failed getting object ids for attestation");

        for (object_id, status_code) in object_ids.iter().zip(
            [http::StatusCode::NOT_FOUND, http::StatusCode::GONE]
                .into_iter()
                .cycle(),
        ) {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (state, _) = get_state_from_db(&mut txn, &machine_id, device_id, job_id).await?;
            let SpdmAttestationState::PollEvidenceCollection { task_id, .. } = state else {
                panic!("expected PollEvidenceCollection, got: {state:?}");
            };
            env.redfish_sim.set_task_http_error(&task_id, status_code);
        }
        env.run_spdm_controller_iteration_no_requeue().await;
        for SpdmObjectId(_, device_id, job_id) in &object_ids {
            let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                device_id,
                job_id,
            )
            .await?;
            assert!(matches!(
                snapshot.state,
                SpdmAttestationState::WaitForTaskCancellation { retry_count: 0, .. }
            ));
            assert!(snapshot.evidence_collection_locked);
        }

        // Missing tasks count as stopped during cleanup, consuming one retry
        // and releasing the claim before another collection can begin.
        env.run_spdm_controller_iteration_no_requeue().await;
        for SpdmObjectId(_, device_id, job_id) in &object_ids {
            let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                device_id,
                job_id,
            )
            .await?;
            assert_eq!(
                snapshot.state,
                SpdmAttestationState::TriggerEvidenceCollection {
                    retry_count: 1,
                    redfish_retry_count: 0,
                    last_redfish_failure_tstamp: None,
                }
            );
            assert!(!snapshot.evidence_collection_locked);
        }

        // Continue retrying terminal exceptions within the same collection budget.
        env.redfish_sim
            .set_get_task_trigger_evidence_returns_exception(true);
        for _ in 0..4 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert!(
                matches!(
                    attestation_state,
                    SpdmAttestationState::TriggerEvidenceCollection {
                        retry_count: 3,
                        redfish_retry_count: 0,
                        last_redfish_failure_tstamp: None,
                    }
                ),
                "expected Trigger, got: {:?}",
                attestation_state
            );
        }

        // now let's just move to the failed state
        for _ in 0..8 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert!(
                matches!(attestation_state, SpdmAttestationState::Failed { .. }),
                "expected Failed, got: {:?}",
                attestation_state
            );
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_evidence_collection_is_serialized_per_device(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        // A short custom timeout distinguishes cleanup from the one-minute retry delay.
        let mut config = spdm_enabled_config();
        config.spdm_state_controller.cancellation_timeout = chrono::Duration::seconds(30);
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(config),
                ..Default::default()
            },
        )
        .await;
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
        // Host setup already performed attestation; count only requests from
        // the two jobs below.
        let initial_requests = env.redfish_sim.evidence_collection_requests().len();
        // Keep BMC tasks "running" to force an overlap
        env.redfish_sim.set_evidence_collection_running(true);

        // Start job A and advance through metadata, certificate, and collection.
        let first_job_id = uuid::Uuid::try_from(
            env.api
                .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                    machine_id: Some(machine_id.into()),
                    redfish_timeout_secs: u32::MAX,
                }))
                .await?
                .into_inner()
                .job_id
                .unwrap(),
        )?;
        for _ in 0..3 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        let mut txn = env.db_txn().await;
        let devices =
            db::attestation::spdm::get_attestations_for_job_id(&mut txn, &first_job_id).await?;
        assert_eq!(3, devices.len());
        // Different devices can collect at the same time. Each must retain its
        // own claim while the BMC is still producing evidence.
        for device in &devices {
            let attestation = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                &device.device_id,
                &first_job_id,
            )
            .await?;
            assert!(matches!(
                attestation.state,
                SpdmAttestationState::PollEvidenceCollection { retry_count: 0, .. }
            ));
            // each device has its own lock, and, since we've set env.redfish_sim.set_evidence_collection_running(true);
            // the lock with persist as the evidence collection is ongoing.
            assert!(attestation.evidence_collection_locked);
            assert!(attestation.evidence.is_none());
        }
        assert_eq!(
            initial_requests + devices.len(),
            env.redfish_sim.evidence_collection_requests().len()
        );

        // Start job B for the same machine while A still owns all device claims.
        let second_job_id = uuid::Uuid::try_from(
            env.api
                .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                    machine_id: Some(machine_id.into()),
                    redfish_timeout_secs: u32::MAX,
                }))
                .await?
                .into_inner()
                .job_id
                .unwrap(),
        )?;
        assert_ne!(first_job_id, second_job_id);
        // Reach TriggerEvidenceCollection and attempt to acquire each held claim.
        for _ in 0..3 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }
        for device in &devices {
            let second = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                &device.device_id,
                &second_job_id,
            )
            .await?;
            assert_eq!(
                second.state,
                SpdmAttestationState::TriggerEvidenceCollection {
                    retry_count: 0,
                    redfish_retry_count: 0,
                    last_redfish_failure_tstamp: None,
                }
            );
            assert!(!second.evidence_collection_locked);
            assert!(second.evidence.is_none());
            // Prove B reached the claim check, rather than stalling on an
            // unrelated error before it could attempt collection.
            assert!(
                matches!(
                    second.state_outcome,
                    Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
                        if reason == "Evidence collection claim is unavailable or attestation state changed"
                ),
                "expected claim contention, got {:?}",
                second.state_outcome
            );
        }
        assert_eq!(
            initial_requests + devices.len(),
            env.redfish_sim.evidence_collection_requests().len(),
            "the waiting job must not send a collection request"
        );

        // Completion still wins after the polling deadline: save A's evidence
        // and release its claims as it advances to verification.
        let old_time = Utc::now() - chrono::Duration::minutes(2);
        let old_version: ConfigVersion = format!("V1-T{}", old_time.timestamp_micros()).parse()?;
        sqlx::query(
            "UPDATE spdm_machine_devices_attestation SET state_version = $2 WHERE job_id = $1",
        )
        .bind(first_job_id)
        .bind(old_version)
        .execute(&env.pool)
        .await?;
        env.redfish_sim.set_evidence_collection_running(false);
        env.run_spdm_controller_iteration_no_requeue().await;
        for device in &devices {
            let first = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                &device.device_id,
                &first_job_id,
            )
            .await?;
            assert_eq!(first.state, SpdmAttestationState::NrasVerification);
            assert!(
                first.evidence.is_some(),
                "evidence must be saved before releasing the claim"
            );
            assert!(!first.evidence_collection_locked);
        }

        // Device iteration order is unspecified: B may have started in the
        // iteration that released A's claim, or may need one more iteration.
        // Keep B's tasks running so we can inspect its newly acquired claims.
        env.redfish_sim.set_evidence_collection_running(true);
        env.run_spdm_controller_iteration_no_requeue().await;
        let requests = env.redfish_sim.evidence_collection_requests();
        assert_eq!(initial_requests + 2 * devices.len(), requests.len());
        for device in &devices {
            let second = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                &device.device_id,
                &second_job_id,
            )
            .await?;
            assert!(matches!(
                second.state,
                SpdmAttestationState::PollEvidenceCollection { retry_count: 0, .. }
            ));
            assert!(second.evidence_collection_locked);
            assert!(second.evidence.is_none());
            // The nonce identifies B's request even though both jobs use the
            // same evidence URL for this device.
            assert_eq!(
                1,
                requests
                    .iter()
                    .filter(|(url, nonce)| Some(url) == second.evidence_target.as_ref()
                        && nonce == &second.nonce_hex())
                    .count(),
                "the second job must start collection exactly once for {}",
                device.device_id
            );
        }

        // Model exhausted POST failures with no task ID while B still owns the claims.
        // Keep the simulated BMC tasks running: expiry does not establish that they stopped.
        for (scenario, time_since_failure, release_claim) in [
            (
                "cleanup grace remains",
                chrono::Duration::seconds(15),
                false,
            ),
            ("cleanup grace expired", chrono::Duration::seconds(45), true),
        ] {
            let retry_state = SpdmAttestationState::TriggerEvidenceCollection {
                retry_count: 0,
                redfish_retry_count: 6,
                last_redfish_failure_tstamp: Some(Utc::now() - time_since_failure),
            };
            sqlx::query("UPDATE spdm_machine_devices_attestation SET state = $2 WHERE job_id = $1")
                .bind(second_job_id)
                .bind(sqlx::types::Json(&retry_state))
                .execute(&env.pool)
                .await?;

            env.run_spdm_controller_iteration_no_requeue().await;
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &second_job_id,
                )
                .await?;
                let expected_state = if release_claim {
                    SpdmAttestationState::Failed(
                        "Exceeded redfish retry limit while attempting to collect evidence"
                            .to_string(),
                    )
                } else {
                    retry_state.clone()
                };
                assert_eq!(snapshot.state, expected_state, "{scenario}");
                assert_eq!(
                    snapshot.evidence_collection_locked, !release_claim,
                    "{scenario}"
                );
                if !release_claim {
                    // The persisted reason proves the timeout branch ran rather than an error path.
                    assert!(
                        matches!(snapshot.state_outcome,
                            Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
                                if reason == "Redfish retries exhausted; waiting for the cancellation timeout before releasing the evidence collection lock"),
                        "{scenario}: expected cleanup wait, got {:?}",
                        snapshot.state_outcome
                    );
                }
                assert!(snapshot.evidence.is_none(), "{scenario}");
            }
            assert_eq!(
                initial_requests + 2 * devices.len(),
                env.redfish_sim.evidence_collection_requests().len(),
                "{scenario}: exhausted retries must not start another collection"
            );
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_evidence_collection_waits_for_cancellation(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};

        struct Case {
            scenario: &'static str,
            initially_cancelling: bool,
            finish_collection: bool,
            operator_outcome: Option<SpdmAttestationState>,
            cancel_error: Option<libredfish::RedfishError>,
        }

        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;
        let client = env
            .redfish_sim
            .create_client("cancellation-fixture", None, RedfishAuth::Anonymous, None)
            .await?;

        for Case {
            scenario,
            initially_cancelling,
            finish_collection,
            operator_outcome,
            cancel_error,
        } in [
            Case {
                scenario: "a running task times out and confirmed stop permits retry",
                initially_cancelling: false,
                finish_collection: true,
                operator_outcome: None,
                cancel_error: None,
            },
            Case {
                scenario: "confirmed stop permits retry",
                initially_cancelling: true,
                finish_collection: true,
                operator_outcome: None,
                cancel_error: None,
            },
            Case {
                scenario: "cancellation request failure still observes the cleanup deadline",
                initially_cancelling: true,
                finish_collection: false,
                operator_outcome: None,
                cancel_error: Some(libredfish::RedfishError::GenericError {
                    error: "temporary cancellation failure".to_string(),
                }),
            },
            Case {
                scenario: "operator waiver wins without extending cleanup",
                initially_cancelling: true,
                finish_collection: false,
                operator_outcome: Some(SpdmAttestationState::Waived),
                cancel_error: None,
            },
            Case {
                scenario: "an earlier failure request bounds automatic cleanup",
                initially_cancelling: true,
                finish_collection: false,
                operator_outcome: Some(SpdmAttestationState::Failed("Failed by operator".into())),
                cancel_error: None,
            },
        ] {
            env.redfish_sim.set_evidence_collection_running(false);
            let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
            env.redfish_sim.set_evidence_collection_running(true);
            let job_id = uuid::Uuid::try_from(
                env.api
                    .trigger_machine_attestation(Request::new(
                        SpdmMachineAttestationTriggerRequest {
                            machine_id: Some(machine_id.into()),
                            redfish_timeout_secs: u32::MAX,
                        },
                    ))
                    .await?
                    .into_inner()
                    .job_id
                    .unwrap(),
            )?;
            for _ in 0..3 {
                env.run_spdm_controller_iteration_no_requeue().await;
            }
            let mut txn = env.db_txn().await;
            let devices =
                db::attestation::spdm::get_attestations_for_job_id(&mut txn, &job_id).await?;
            // Keep tasks Running or model a BMC already stuck in Cancelling.
            for device in &devices {
                let SpdmAttestationState::PollEvidenceCollection { task_id, .. } = &device.state
                else {
                    panic!(
                        "{scenario}: expected evidence collection, got {:?}",
                        device.state
                    );
                };
                if initially_cancelling {
                    assert!(!client.cancel_task(task_id).await?);
                }
            }
            let initial_cancellations = env.redfish_sim.task_cancellation_requests().len();
            let initial_collections = env.redfish_sim.evidence_collection_requests().len();
            let expected_wait_reason = if initially_cancelling {
                "Waiting for BMC evidence collection to stop before retrying"
            } else {
                "Measurement collection is pending 0%"
            };
            env.run_spdm_controller_iteration_no_requeue().await;
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                assert!(
                    matches!(
                        snapshot.state,
                        SpdmAttestationState::PollEvidenceCollection { retry_count: 0, .. }
                    ),
                    "{scenario}"
                );
                assert!(
                    matches!(snapshot.state_outcome, Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
                    if reason == expected_wait_reason),
                    "{scenario}"
                );
                assert!(snapshot.evidence_collection_locked, "{scenario}");
            }
            assert_eq!(
                env.redfish_sim.task_cancellation_requests().len(),
                initial_cancellations,
                "{scenario}: polling before the deadline must not request cancellation"
            );
            assert_eq!(
                env.redfish_sim.evidence_collection_requests().len(),
                initial_collections,
                "{scenario}: polling must not start another collection"
            );

            // Age the persisted polling state instead of sleeping through the grace period.
            let old_time = Utc::now() - chrono::Duration::minutes(2);
            let old_version: ConfigVersion =
                format!("V1-T{}", old_time.timestamp_micros()).parse()?;
            sqlx::query(
                "UPDATE spdm_machine_devices_attestation SET state_version = $2 WHERE job_id = $1",
            )
            .bind(job_id)
            .bind(old_version)
            .execute(&env.pool)
            .await?;
            if let Some(error) = cancel_error {
                env.redfish_sim.set_next_task_cancel_error(error);
            }
            env.run_spdm_controller_iteration_no_requeue().await;
            let cancel_requests = env.redfish_sim.task_cancellation_requests();
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                let SpdmAttestationState::PollEvidenceCollection { task_id, .. } = &device.state
                else {
                    unreachable!("the initial polling states were checked above");
                };
                assert!(
                    matches!(
                        snapshot.state,
                        SpdmAttestationState::WaitForTaskCancellation {
                            task_id: ref actual_task_id,
                            retry_count: 0,
                            ..
                        } if actual_task_id == task_id
                    ),
                    "{scenario}"
                );
                assert!(snapshot.evidence_collection_locked, "{scenario}");
                assert_eq!(
                    cancel_requests[initial_cancellations..]
                        .iter()
                        .filter(|cancelled_task_id| *cancelled_task_id == task_id)
                        .count(),
                    1,
                    "{scenario}: cancellation must target each device's own task once"
                );
            }
            assert_eq!(
                cancel_requests.len(),
                initial_cancellations + devices.len(),
                "{scenario}: expired polling requests cancellation before entering the waiting state"
            );
            let cleanup_version: ConfigVersion = sqlx::query_scalar("SELECT state_version FROM spdm_machine_devices_attestation WHERE job_id = $1 AND device_id = $2")
                .bind(job_id).bind(&devices[0].device_id).fetch_one(&env.pool).await?;
            // The waiting state only polls; it must not send another DELETE.
            for _ in 0..2 {
                env.run_spdm_controller_iteration_no_requeue().await;
            }
            assert_eq!(
                env.redfish_sim.task_cancellation_requests().len(),
                initial_cancellations + devices.len(),
                "{scenario}"
            );
            let waiting = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                &mut txn,
                &machine_id,
                &devices[0].device_id,
                &job_id,
            )
            .await?;
            assert_eq!(cleanup_version, waiting.state_version, "{scenario}");
            assert!(
                matches!(waiting.state_outcome, Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
                    if reason == "Waiting for BMC evidence collection to stop after cancellation"),
                "{scenario}"
            );
            assert_eq!(
                env.redfish_sim.evidence_collection_requests().len(),
                initial_collections,
                "a pending cancellation must not start another collection"
            );

            if finish_collection {
                env.redfish_sim.set_evidence_collection_running(false);
            } else if !matches!(operator_outcome, Some(SpdmAttestationState::Failed(_))) {
                // Cleanup has its own clock, independent of the current state version.
                sqlx::query("UPDATE spdm_machine_devices_attestation
                    SET state = jsonb_set(state, '{WaitForTaskCancellation,cleanup_started_at}', to_jsonb($2::timestamptz))
                    WHERE job_id = $1")
                    .bind(job_id).bind(old_version.timestamp()).execute(&env.pool).await?;
            }
            if let Some(outcome) = &operator_outcome {
                if *outcome == SpdmAttestationState::Waived {
                    env.api
                        .waive_attestation(Request::new(job_id.into()))
                        .await?;
                } else {
                    env.api
                        .fail_attestation(Request::new(job_id.into()))
                        .await?;
                    // Model a request accepted before the in-flight automatic cleanup
                    // iteration committed its newer cleanup start.
                    sqlx::query("UPDATE spdm_machine_devices_attestation SET cancellation_cleanup_started_at = $2, cancelled_at = $2 WHERE job_id = $1")
                        .bind(job_id).bind(old_version.timestamp()).execute(&env.pool).await?;
                }
            }
            // Request timestamps and the automatic cleanup start retain their own times.
            let waiver_requested = operator_outcome == Some(SpdmAttestationState::Waived);
            let failure_requested =
                matches!(operator_outcome, Some(SpdmAttestationState::Failed(_)));
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                assert_eq!(
                    snapshot.cancellation_requested_at.is_some(),
                    waiver_requested || failure_requested,
                    "{scenario}"
                );
                assert_eq!(
                    snapshot.cancellation_target,
                    if failure_requested {
                        Some(SpdmCancellationTarget::Fail)
                    } else if waiver_requested {
                        Some(SpdmCancellationTarget::Waive)
                    } else {
                        None
                    },
                    "{scenario}"
                );
                if let Some(requested_at) = snapshot.cancellation_requested_at {
                    if failure_requested {
                        assert_eq!(requested_at, old_version.timestamp(), "{scenario}");
                    } else {
                        assert!(requested_at > old_version.timestamp(), "{scenario}");
                    }
                }
                if operator_outcome.is_some() {
                    let SpdmAttestationState::WaitForTaskCancellation {
                        cleanup_started_at, ..
                    } = snapshot.state
                    else {
                        panic!("{scenario}: expected automatic cleanup");
                    };
                    if failure_requested {
                        assert!(cleanup_started_at > old_version.timestamp(), "{scenario}");
                    } else {
                        assert_eq!(cleanup_started_at, old_version.timestamp(), "{scenario}");
                    }
                }
            }
            env.run_spdm_controller_iteration_no_requeue().await;
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                let expected = if let Some(outcome) = &operator_outcome {
                    outcome.clone()
                } else if finish_collection {
                    SpdmAttestationState::TriggerEvidenceCollection {
                        retry_count: 1,
                        redfish_retry_count: 0,
                        last_redfish_failure_tstamp: None,
                    }
                } else {
                    SpdmAttestationState::Failed(
                        "Timed out waiting for BMC evidence collection to stop".to_string(),
                    )
                };
                assert_eq!(snapshot.state, expected, "{scenario}");
                assert!(!snapshot.evidence_collection_locked, "{scenario}");
                assert_eq!(
                    snapshot.completed_at.is_some(),
                    operator_outcome.is_some(),
                    "{scenario}"
                );
                assert!(
                    snapshot.evidence.is_none(),
                    "cleanup must not read evidence"
                );
            }
            if !finish_collection && operator_outcome.is_none() {
                // The Failed handler records completion on the following iteration.
                env.run_spdm_controller_iteration_no_requeue().await;
                for device in &devices {
                    let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                        &mut txn,
                        &machine_id,
                        &device.device_id,
                        &job_id,
                    )
                    .await?;
                    assert!(snapshot.completed_at.is_some(), "{scenario}");
                }
            }
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_operator_requests_finish_after_collection_cleanup(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        struct Case {
            scenario: &'static str,
            fail: bool,
            trigger_iterations: usize,
            cancel_error: Option<libredfish::RedfishError>,
            finish_collection: bool,
            claim_without_task: bool,
            task_missing: bool,
        }
        let cases = [
            Case {
                scenario: "waive before collection starts",
                fail: false,
                trigger_iterations: 2,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: false,
                task_missing: false,
            },
            Case {
                scenario: "waive releases the claim when the task is already missing",
                fail: false,
                trigger_iterations: 3,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: false,
                task_missing: true,
            },
            Case {
                scenario: "retry a transient cancellation failure, then wait for the BMC",
                fail: false,
                trigger_iterations: 3,
                cancel_error: Some(libredfish::RedfishError::GenericError {
                    error: "temporary cancellation failure".to_string(),
                }),
                finish_collection: true,
                claim_without_task: false,
                task_missing: false,
            },
            Case {
                scenario: "unsupported cancellation releases the lock only at the deadline",
                fail: false,
                trigger_iterations: 3,
                cancel_error: Some(libredfish::RedfishError::NotSupported(
                    "cancel task".to_string(),
                )),
                finish_collection: false,
                claim_without_task: false,
                task_missing: false,
            },
            Case {
                scenario: "a collection claim without a task ID uses the deadline",
                fail: false,
                trigger_iterations: 2,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: true,
                task_missing: false,
            },
            Case {
                scenario: "fail unfinished work before collection starts",
                fail: true,
                trigger_iterations: 2,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: false,
                task_missing: false,
            },
            Case {
                scenario: "fail releases the claim when the task is already missing",
                fail: true,
                trigger_iterations: 3,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: false,
                task_missing: true,
            },
            Case {
                scenario: "fail retains the lock until the BMC confirms cleanup",
                fail: true,
                trigger_iterations: 3,
                cancel_error: None,
                finish_collection: true,
                claim_without_task: false,
                task_missing: false,
            },
            Case {
                scenario: "fail completes at the cleanup deadline when cancellation is unsupported",
                fail: true,
                trigger_iterations: 3,
                cancel_error: Some(libredfish::RedfishError::NotSupported(
                    "cancel task".to_string(),
                )),
                finish_collection: false,
                claim_without_task: false,
                task_missing: false,
            },
        ];
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;
        // Invalid and absent jobs retain the API's distinct error contracts.
        for (value, code) in [
            ("invalid".to_string(), tonic::Code::InvalidArgument),
            (uuid::Uuid::new_v4().to_string(), tonic::Code::NotFound),
        ] {
            let error = env
                .api
                .fail_attestation(Request::new(rpc::Uuid { value }))
                .await
                .unwrap_err();
            assert_eq!(error.code(), code);
        }
        // Host setup performs its own attestation before the scenario starts.
        env.redfish_sim.set_evidence_collection_running(false);
        let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
        for case in cases {
            let expected_state = if case.fail {
                SpdmAttestationState::Failed("Failed by operator".to_string())
            } else {
                SpdmAttestationState::Waived
            };
            let initial_cancel_requests = env.redfish_sim.task_cancellation_requests().len();
            env.redfish_sim.set_evidence_collection_running(true);
            let job_id = uuid::Uuid::try_from(
                env.api
                    .trigger_machine_attestation(Request::new(
                        SpdmMachineAttestationTriggerRequest {
                            machine_id: Some(machine_id.into()),
                            redfish_timeout_secs: u32::MAX,
                        },
                    ))
                    .await?
                    .into_inner()
                    .job_id
                    .expect("scheduled attestation has a job ID"),
            )?;
            for _ in 0..case.trigger_iterations {
                env.run_spdm_controller_iteration_no_requeue().await;
            }
            if case.task_missing {
                // Exercise both missing-task responses for each operator target before cleanup begins.
                let mut txn = env.db_txn().await;
                let devices =
                    db::attestation::spdm::get_attestations_for_job_id(&mut txn, &job_id).await?;
                for (device, status_code) in devices.iter().zip(
                    [http::StatusCode::NOT_FOUND, http::StatusCode::GONE]
                        .into_iter()
                        .cycle(),
                ) {
                    let SpdmAttestationState::PollEvidenceCollection { task_id, .. } =
                        &device.state
                    else {
                        panic!("{}: expected evidence collection polling", case.scenario);
                    };
                    env.redfish_sim.set_task_http_error(task_id, status_code);
                }
            }
            let completed_device = if case.trigger_iterations == 2 && !case.claim_without_task {
                // A terminal result in a mixed job must survive either operator request.
                let completed_state = if case.fail {
                    SpdmAttestationState::Passed
                } else {
                    SpdmAttestationState::Failed("verification failed".to_string())
                };
                let device_id: String = sqlx::query_scalar("UPDATE spdm_machine_devices_attestation
                    SET state = $2, completed_at = now()
                    WHERE job_id = $1 AND device_id = (
                        SELECT min(device_id) FROM spdm_machine_devices_attestation WHERE job_id = $1)
                    RETURNING device_id")
                    .bind(job_id).bind(sqlx::types::Json(&completed_state))
                    .fetch_one(&env.pool).await?;
                Some((device_id, completed_state))
            } else {
                None
            };
            let expected_for_device = |device_id: &str| {
                completed_device
                    .as_ref()
                    .filter(|(completed_id, _)| completed_id == device_id)
                    .map_or(&expected_state, |(_, state)| state)
            };
            if case.claim_without_task {
                // Model a claim committed before a collection POST whose result was lost.
                sqlx::query("UPDATE spdm_machine_devices_attestation SET evidence_collection_locked = TRUE WHERE job_id = $1")
                    .bind(job_id).execute(&env.pool).await?;
            }
            let transient_failure = matches!(
                case.cancel_error,
                Some(libredfish::RedfishError::GenericError { .. })
            );
            if let Some(error) = case.cancel_error {
                env.redfish_sim.set_next_task_cancel_error(error);
            }
            if case.fail {
                if case.trigger_iterations == 2 {
                    // A later failure request replaces the pending waiver.
                    env.api
                        .waive_attestation(Request::new(job_id.into()))
                        .await?;
                }
                env.api
                    .fail_attestation(Request::new(job_id.into()))
                    .await?;
            } else {
                env.api
                    .waive_attestation(Request::new(job_id.into()))
                    .await?;
            }
            if case.trigger_iterations == 3 {
                // Include time spent waiting for the controller in the cleanup timeout.
                sqlx::query(
                    "UPDATE spdm_machine_devices_attestation
                    SET cancelled_at = cancelled_at - INTERVAL '1 second',
                        cancellation_cleanup_started_at = cancellation_cleanup_started_at - INTERVAL '1 second'
                    WHERE job_id = $1",
                )
                .bind(job_id)
                .execute(&env.pool)
                .await?;
            }
            env.run_spdm_controller_iteration_no_requeue().await;

            let mut txn = env.db_txn().await;
            let devices =
                db::attestation::spdm::get_attestations_for_job_id(&mut txn, &job_id).await?;
            assert_eq!(devices.len(), 3, "{}", case.scenario);
            let cancelling = case.trigger_iterations == 3 && !case.task_missing;
            let waiting = cancelling || case.claim_without_task;
            let expected_task_ids = devices
                .iter()
                .filter_map(|device| {
                    if let SpdmAttestationState::WaitForTaskCancellation { task_id, .. }
                    | SpdmAttestationState::PollEvidenceCollection { task_id, .. } =
                        &device.state
                    {
                        Some(task_id.clone())
                    } else {
                        None
                    }
                })
                .collect::<std::collections::HashSet<_>>();
            let requests =
                env.redfish_sim.task_cancellation_requests()[initial_cancel_requests..].to_vec();
            assert_eq!(
                requests.len(),
                if cancelling { 3 } else { 0 },
                "{}",
                case.scenario
            );
            if cancelling {
                // Distinct task IDs prove cancellation targets each component's own task.
                assert_eq!(expected_task_ids.len(), 3, "{}", case.scenario);
                assert_eq!(
                    requests
                        .iter()
                        .cloned()
                        .collect::<std::collections::HashSet<_>>(),
                    expected_task_ids
                );
            }
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                assert_eq!(
                    snapshot.evidence_collection_locked, waiting,
                    "{}",
                    case.scenario
                );
                assert_eq!(
                    snapshot.completed_at.is_none(),
                    waiting,
                    "{}",
                    case.scenario
                );
            }
            if !waiting {
                assert!(
                    devices
                        .iter()
                        .all(|device| &device.state == expected_for_device(&device.device_id))
                );
                let response = env
                    .api
                    .get_attestation(Request::new(job_id.into()))
                    .await?
                    .into_inner();
                for device in response.attestations_details {
                    assert_eq!(
                        device.state,
                        format!("{:?}", expected_for_device(&device.device_id))
                    );
                }
                if !case.fail {
                    // Terminal overrides need no controller pass and preserve the attestation history.
                    let snapshot_query = "SELECT jsonb_agg(to_jsonb(a) - 'cancelled_at' - 'cancellation_target' ORDER BY device_id)
                        FROM spdm_machine_devices_attestation a WHERE job_id = $1";
                    let before: serde_json::Value = sqlx::query_scalar(snapshot_query)
                        .bind(job_id)
                        .fetch_one(&env.pool)
                        .await?;
                    env.api
                        .fail_attestation(Request::new(job_id.into()))
                        .await?;
                    let mut status_txn = env.db_txn().await;
                    assert_eq!(
                        db::attestation::spdm::get_attestation_job_status(&mut status_txn, &job_id)
                            .await?,
                        model::attestation::spdm::SpdmAttestationStatus::Failed
                    );
                    env.api
                        .waive_attestation(Request::new(job_id.into()))
                        .await?;
                    assert_eq!(
                        db::attestation::spdm::get_attestation_job_status(&mut status_txn, &job_id)
                            .await?,
                        model::attestation::spdm::SpdmAttestationStatus::Waived
                    );
                    let after: serde_json::Value = sqlx::query_scalar(snapshot_query)
                        .bind(job_id)
                        .fetch_one(&env.pool)
                        .await?;
                    assert_eq!(before, after);
                }
                continue;
            }

            // Retry only the injected failure; accepted and unsupported requests must
            // remain persisted across subsequent controller iterations.
            for _ in 0..2 {
                env.run_spdm_controller_iteration_no_requeue().await;
            }
            assert_eq!(
                env.redfish_sim.task_cancellation_requests().len(),
                initial_cancel_requests + requests.len() + usize::from(transient_failure),
                "{}",
                case.scenario
            );
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                assert!(snapshot.evidence_collection_locked, "{}", case.scenario);
                if cancelling {
                    let SpdmAttestationState::WaitForTaskCancellation {
                        cleanup_started_at, ..
                    } = snapshot.state
                    else {
                        panic!("{}: expected cancellation cleanup", case.scenario);
                    };
                    assert_eq!(
                        Some(cleanup_started_at),
                        snapshot.cancellation_cleanup_started_at,
                        "{}",
                        case.scenario
                    );
                }
            }

            if case.finish_collection {
                env.redfish_sim.set_evidence_collection_running(false);
            } else {
                // Advance the persisted deadline without a real two-minute sleep.
                sqlx::query("UPDATE spdm_machine_devices_attestation
                    SET cancelled_at = CASE WHEN cancelled_at IS NOT NULL THEN NOW() - INTERVAL '2 minutes' END,
                        cancellation_cleanup_started_at = CASE WHEN cancellation_cleanup_started_at IS NOT NULL THEN NOW() - INTERVAL '2 minutes' END,
                        state = CASE WHEN state ? 'WaitForTaskCancellation'
                            THEN jsonb_set(state, '{WaitForTaskCancellation,cleanup_started_at}', to_jsonb(NOW() - INTERVAL '2 minutes'))
                            ELSE state END
                    WHERE job_id = $1")
                    .bind(job_id).execute(&env.pool).await?;
            }
            let request_time_query = "SELECT cancelled_at
                FROM spdm_machine_devices_attestation WHERE job_id = $1 LIMIT 1";
            let requested_at: chrono::DateTime<Utc> = sqlx::query_scalar(request_time_query)
                .bind(job_id)
                .fetch_one(&env.pool)
                .await?;
            // Repeated user requests must not postpone the original deadline.
            if case.fail {
                env.api
                    .fail_attestation(Request::new(job_id.into()))
                    .await?;
            } else {
                env.api
                    .waive_attestation(Request::new(job_id.into()))
                    .await?;
            }
            let repeated_requested_at: chrono::DateTime<Utc> =
                sqlx::query_scalar(request_time_query)
                    .bind(job_id)
                    .fetch_one(&env.pool)
                    .await?;
            assert!(repeated_requested_at > requested_at, "{}", case.scenario);
            env.run_spdm_controller_iteration_no_requeue().await;
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                assert_eq!(snapshot.state, expected_state, "{}", case.scenario);
                assert!(!snapshot.evidence_collection_locked, "{}", case.scenario);
                assert!(snapshot.completed_at.is_some(), "{}", case.scenario);
                assert!(
                    snapshot.evidence.is_none(),
                    "resolved jobs must not read evidence"
                );
            }
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn attestation_jobs_rpc_paginates_and_validates(pool: sqlx::PgPool) -> eyre::Result<()> {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use carbide_test_support::Outcome::FailsWith;
        use carbide_test_support::{Case, check_cases_async};
        use carbide_uuid::machine::{MachineIdSource, MachineType};

        let mut config = get_config();
        config.max_find_by_ids = 2;
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(config),
                ..TestEnvOverrides::no_network_segments()
            },
        )
        .await;
        let mut machines = [0x81, 0x82].map(|seed| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [seed; 32],
                MachineType::Host,
            )
        });
        machines.sort();
        for machine_id in machines {
            sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
                .bind(machine_id)
                .execute(&env.pool)
                .await?;
        }
        let jobs = [10, 20, 30, 40, 50].map(uuid::Uuid::from_u128);
        // Insert out of order and interleave other statuses. Each matching job has
        // two machines, so pagination must follow complete job aggregation.
        for id in [50, 45, 40, 30, 25, 20, 10, 5] {
            for (index, machine_id) in machines.iter().enumerate() {
                let state = if index == 1 {
                    SpdmAttestationState::Passed
                } else if id % 10 == 0 {
                    SpdmAttestationState::FetchMetadata
                } else {
                    SpdmAttestationState::Failed("verification failed".into())
                };
                sqlx::query(
                    "INSERT INTO spdm_machine_devices_attestation
                     (machine_id, device_id, nonce, state, job_id, started_at)
                     VALUES ($1, 'GPU_0', gen_random_uuid(), $2, $3, now())",
                )
                .bind(machine_id)
                .bind(sqlx::types::Json(state))
                .bind(uuid::Uuid::from_u128(id))
                .execute(&env.pool)
                .await?;
            }
        }

        let mut listed_jobs = Vec::new();
        let mut page_token = String::new();
        let mut first_token = String::new();
        // Zero defaults to the cap, oversized requests are capped, and the final
        // continuation changes page size. The last full page needs no token.
        for (page_size, expected_len) in [(0, 2), (u32::MAX, 2), (1, 1)] {
            let response = env
                .api
                .list_attestation(Request::new(SpdmListAttestationRequest {
                    page_size,
                    page_token,
                    ..Default::default()
                }))
                .await?
                .into_inner();
            assert_eq!(response.statuses.len(), expected_len);
            if first_token.is_empty() {
                first_token.clone_from(&response.next_page_token);
            }
            for status in response.statuses {
                listed_jobs.push(uuid::Uuid::try_from(status.job_id.unwrap())?);
                assert_eq!(status.machine_ids, machines);
                assert_eq!(
                    status.attestation_status,
                    i32::from(SpdmAttestationStatus::SpdmAttInProgress)
                );
            }
            page_token = response.next_page_token;
            assert_eq!(page_token.is_empty(), listed_jobs.len() == jobs.len());
        }
        assert_eq!(listed_jobs, jobs);

        let empty = env
            .api
            .list_attestation(Request::new(SpdmListAttestationRequest {
                selector: SpdmListAttestationRequestSelector::SpdmListWaived.into(),
                ..Default::default()
            }))
            .await?
            .into_inner();
        assert!(empty.statuses.is_empty());
        assert!(empty.next_page_token.is_empty());

        let invalid_requests = [
            (
                "unknown selector",
                SpdmListAttestationRequest {
                    selector: -1,
                    ..Default::default()
                },
            ),
            (
                "malformed base64",
                SpdmListAttestationRequest {
                    page_token: "!".into(),
                    ..Default::default()
                },
            ),
            (
                "invalid token schema",
                SpdmListAttestationRequest {
                    page_token: URL_SAFE_NO_PAD.encode(b"{}"),
                    ..Default::default()
                },
            ),
            (
                "unsupported token version",
                SpdmListAttestationRequest {
                    page_token: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
                        "version": 2, "after_job_id": jobs[0], "selector": 0,
                    }))?),
                    ..Default::default()
                },
            ),
            (
                "changed selector",
                SpdmListAttestationRequest {
                    selector: SpdmListAttestationRequestSelector::SpdmListPassed.into(),
                    page_token: first_token,
                    ..Default::default()
                },
            ),
        ];
        check_cases_async(
            invalid_requests.map(|(scenario, input)| Case {
                scenario,
                input,
                expect: FailsWith(tonic::Code::InvalidArgument),
            }),
            |request| {
                let api = &env.api;
                async move {
                    api.list_attestation(Request::new(request))
                        .await
                        .map(|_| ())
                        .map_err(|error| error.code())
                }
            },
        )
        .await;
        Ok(())
    }

    #[crate::sqlx_test]
    async fn predicted_machine_attestation_rpc_paginates_and_validates(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        use rpc::forge::{MachineType as RpcMachineType, SpdmMachineAttestationFleetRequest};

        let host_type = Some(RpcMachineType::Host as i32);
        let dpu_type = Some(RpcMachineType::Dpu as i32);

        let fleet_request = |request| ListMachineAttestationStatusesRequest {
            target: Some(Target::Fleet(request)),
        };
        let mut config = get_config();
        config.max_find_by_ids = 2;
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(config),
                ..Default::default()
            },
        )
        .await;
        let mut machines = [0x91, 0x92, 0x93, 0x94, 0x95].map(|seed| {
            MachineId::new(
                MachineIdSource::ProductBoardChassisSerial,
                [seed; 32],
                MachineType::Host,
            )
        });
        machines.sort();
        let dpu = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0x96; 32],
            MachineType::Dpu,
        );
        sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
            .bind(dpu)
            .execute(&env.pool)
            .await?;
        let job_id = uuid::Uuid::new_v4();
        for (index, id) in machines.iter().enumerate() {
            sqlx::query("INSERT INTO machines (id, dpf) VALUES ($1, '{}'::jsonb)")
                .bind(id)
                .execute(&env.pool)
                .await?;
            // Interleaved unknown machines prove filtering precedes the page limit.
            if index % 2 == 0 {
                sqlx::query(
                    "INSERT INTO spdm_machine_devices_attestation
                     (machine_id, device_id, nonce, state, job_id, started_at, triggered_by_mh_controller)
                     VALUES ($1, 'GPU_0', gen_random_uuid(), $2, $3, now(), true)",
                )
                .bind(id).bind(sqlx::types::Json(SpdmAttestationState::Passed))
                .bind(job_id).execute(&env.pool).await?;
            }
        }
        // Fleet listing remains available without a BMC client or enabled SPDM controller.
        env.redfish_sim
            .set_create_client_error("fleet listing must not contact BMCs");
        // Omitting the type filter preserves all-machine membership.
        let all_machines = env
            .api
            .list_machine_attestation_statuses(Request::new(fleet_request(
                SpdmMachineAttestationFleetRequest::default(),
            )))
            .await?
            .into_inner();
        assert_eq!(all_machines.statuses[0].machine_id, Some(dpu));
        assert!(all_machines.statuses[0].attestation_status.is_none());
        // All-machine tokens keep the encoding accepted by older servers.
        let all_token: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&all_machines.next_page_token)?)?;
        assert!(all_token.get("machine_type").is_none());
        // Existing tokens without a machine type still resume an all-machine search.
        let legacy_page = env
            .api
            .list_machine_attestation_statuses(Request::new(fleet_request(
                SpdmMachineAttestationFleetRequest {
                    page_token: URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
                        "version": 1, "after_machine_id": dpu, "attestation_status": null,
                    }))?),
                    ..Default::default()
                },
            )))
            .await?
            .into_inner();
        assert_eq!(
            legacy_page
                .statuses
                .iter()
                .map(|row| row.machine_id.unwrap())
                .collect::<Vec<_>>(),
            machines[..2]
        );
        // Selecting DPUs exercises the second supported type through the public RPC.
        let dpu_page = env
            .api
            .list_machine_attestation_statuses(Request::new(fleet_request(
                SpdmMachineAttestationFleetRequest {
                    machine_type: dpu_type,
                    ..Default::default()
                },
            )))
            .await?
            .into_inner();
        assert_eq!(dpu_page.statuses.len(), 1);
        assert_eq!(dpu_page.statuses[0].machine_id, Some(dpu));
        assert!(dpu_page.statuses[0].attestation_status.is_none());
        assert!(dpu_page.next_page_token.is_empty());
        let mut statuses = Vec::new();
        let mut page_token = String::new();
        // Zero uses the configured cap, oversized values are capped, and continuation
        // permits changing page size. Five machines must produce pages of 2, 2, 1.
        for (page_size, expected_len) in [(0, 2), (u32::MAX, 2), (1, 1)] {
            let response = env
                .api
                .list_machine_attestation_statuses(Request::new(fleet_request(
                    SpdmMachineAttestationFleetRequest {
                        attestation_status: None,
                        page_size,
                        page_token,
                        machine_type: host_type,
                    },
                )))
                .await?
                .into_inner();
            assert_eq!(response.statuses.len(), expected_len);
            page_token = response.next_page_token;
            statuses.extend(response.statuses);
            assert_eq!(page_token.is_empty(), statuses.len() == machines.len());
        }
        assert_eq!(
            statuses
                .iter()
                .map(|row| row.machine_id.unwrap())
                .collect::<Vec<_>>(),
            machines
        );
        assert_eq!(
            statuses[0].attestation_status,
            Some(SpdmAttestationStatus::SpdmAttPassed.into())
        );
        assert!(statuses[1].attestation_status.is_none());
        assert!(
            statuses
                .iter()
                .all(|row| row.attester_selection_status.is_none())
        );

        let passed = Some(SpdmAttestationStatus::SpdmAttPassed.into());
        let first = env
            .api
            .list_machine_attestation_statuses(Request::new(fleet_request(
                SpdmMachineAttestationFleetRequest {
                    attestation_status: passed,
                    page_size: 1,
                    page_token: String::new(),
                    machine_type: host_type,
                },
            )))
            .await?
            .into_inner();
        assert_eq!(first.statuses[0].machine_id, Some(machines[0]));
        assert!(!first.next_page_token.is_empty());
        let last = env
            .api
            .list_machine_attestation_statuses(Request::new(fleet_request(
                SpdmMachineAttestationFleetRequest {
                    attestation_status: passed,
                    page_size: 2,
                    page_token: first.next_page_token.clone(),
                    machine_type: host_type,
                },
            )))
            .await?
            .into_inner();
        assert_eq!(
            last.statuses
                .iter()
                .map(|row| row.machine_id.unwrap())
                .collect::<Vec<_>>(),
            vec![machines[2], machines[4]]
        );
        assert!(
            last.next_page_token.is_empty(),
            "an exactly full final page needs no continuation"
        );
        let empty = env
            .api
            .list_machine_attestation_statuses(Request::new(fleet_request(
                SpdmMachineAttestationFleetRequest {
                    attestation_status: Some(SpdmAttestationStatus::SpdmAttFailed.into()),
                    ..Default::default()
                },
            )))
            .await?
            .into_inner();
        assert!(empty.statuses.is_empty() && empty.next_page_token.is_empty());

        let error = env
            .api
            .list_machine_attestation_statuses(Request::new(
                ListMachineAttestationStatusesRequest::default(),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);

        // Reject malformed encodings and changed search semantics at the RPC boundary.
        let invalid_requests = [
            ("invalid status", host_type, Some(-1), String::new()),
            ("invalid base64", host_type, None, "%%%".to_string()),
            ("invalid token payload", host_type, None, URL_SAFE_NO_PAD.encode(b"{}")),
            ("changed status", host_type, None, first.next_page_token.clone()),
            ("changed machine type", dpu_type, passed, first.next_page_token),
            ("unknown machine type", Some(RpcMachineType::Unknown as i32), None, String::new()),
            ("unsupported machine type", Some(RpcMachineType::PowerShelf as i32), None, String::new()),
            ("invalid machine type", Some(-1), None, String::new()),
            (
                "unsupported token version",
                host_type,
                passed,
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
                    "version": 2, "after_machine_id": machines[0], "attestation_status": passed, "machine_type": host_type,
                }))?),
            ),
            (
                "invalid machine cursor",
                host_type,
                passed,
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
                    "version": 1, "after_machine_id": "invalid", "attestation_status": passed, "machine_type": host_type,
                }))?),
            ),
        ];
        for (scenario, machine_type, attestation_status, page_token) in invalid_requests {
            let error = env
                .api
                .list_machine_attestation_statuses(Request::new(fleet_request(
                    SpdmMachineAttestationFleetRequest {
                        attestation_status,
                        page_token,
                        machine_type,
                        ..Default::default()
                    },
                )))
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument, "{scenario}");
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn definitive_machine_assessment_uses_current_selection_without_writes(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        use rpc::forge::SpdmMachineAttestationFleetRequest;

        async fn snapshot(
            pool: &sqlx::PgPool,
            id: MachineId,
        ) -> Result<serde_json::Value, sqlx::Error> {
            sqlx::query_scalar("SELECT COALESCE(jsonb_agg(to_jsonb(a) ORDER BY device_id, job_id), '[]'::jsonb) FROM spdm_machine_devices_attestation a WHERE machine_id = $1")
                .bind(id).fetch_one(pool).await
        }

        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
                ..Default::default()
            },
        )
        .await;
        let (host_id, _) = create_managed_host(&env).await.into();
        let machine_id: MachineId = host_id.into();
        let mut txn = env.pool.begin().await?;
        sqlx::query("DELETE FROM spdm_machine_devices_attestation WHERE machine_id = $1")
            .bind(machine_id)
            .execute(&mut *txn)
            .await?;
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(gpu_allowlist()),
            "test",
        )
        .await?;
        let controller_job = uuid::Uuid::new_v4();
        let external_job = uuid::Uuid::new_v4();
        let time = Utc::now();
        // GPU_2 is selected by the live inventory but has no stored result. GPU_0's
        // external pass supersedes its controller failure; the obsolete device is excluded.
        for (device, state, job, seconds, controller) in [
            (
                "HGX_IRoT_GPU_0",
                SpdmAttestationState::Failed("superseded failure".to_string()),
                controller_job,
                -2,
                true,
            ),
            (
                "HGX_IRoT_GPU_1",
                SpdmAttestationState::Passed,
                controller_job,
                -2,
                true,
            ),
            (
                "obsolete-device",
                SpdmAttestationState::Failed("obsolete failure".to_string()),
                controller_job,
                -2,
                true,
            ),
            (
                "HGX_IRoT_GPU_0",
                SpdmAttestationState::Passed,
                external_job,
                -1,
                false,
            ),
        ] {
            sqlx::query("INSERT INTO spdm_machine_devices_attestation (machine_id, device_id, nonce, state, job_id, started_at, triggered_by_mh_controller) VALUES ($1, $2, gen_random_uuid(), $3, $4, $5, $6)")
                .bind(machine_id).bind(device).bind(sqlx::types::Json(state)).bind(job).bind(time + chrono::Duration::seconds(seconds)).bind(controller).execute(&mut *txn).await?;
        }
        txn.commit().await?;
        let fleet = env
            .api
            .list_machine_attestation_statuses(Request::new(
                ListMachineAttestationStatusesRequest {
                    target: Some(Target::Fleet(SpdmMachineAttestationFleetRequest {
                        attestation_status: Some(SpdmAttestationStatus::SpdmAttFailed.into()),
                        ..Default::default()
                    })),
                },
            ))
            .await?
            .into_inner();
        assert!(
            fleet
                .statuses
                .iter()
                .any(|row| row.machine_id == Some(machine_id))
        );

        for expected in [
            SpdmAttestationStatus::SpdmAttInProgress,
            SpdmAttestationStatus::SpdmAttPassed,
        ] {
            let before = snapshot(&env.pool, machine_id).await?;
            let response = env
                .api
                .list_machine_attestation_statuses(Request::new(machine_assessment_request(
                    machine_id,
                )))
                .await?
                .into_inner();
            assert_eq!(response.statuses.len(), 1);
            assert!(response.next_page_token.is_empty());
            let row = &response.statuses[0];
            assert_eq!(row.machine_id, Some(machine_id));
            assert_eq!(row.attestation_status, Some(expected.into()));
            assert_eq!(
                row.attester_selection_status(),
                SpdmAttesterSelectionStatus::Selected
            );
            assert_eq!(
                snapshot(&env.pool, machine_id).await?,
                before,
                "assessment must not change any attestation row"
            );
            if expected == SpdmAttestationStatus::SpdmAttInProgress {
                sqlx::query("INSERT INTO spdm_machine_devices_attestation (machine_id, device_id, nonce, state, job_id, started_at, triggered_by_mh_controller) VALUES ($1, 'HGX_IRoT_GPU_2', gen_random_uuid(), $2, $3, now(), false)")
                    .bind(machine_id).bind(sqlx::types::Json(SpdmAttestationState::Passed)).bind(uuid::Uuid::new_v4()).execute(&env.pool).await?;
            }
        }
        // Partial policy coverage stays explicit even when all selected devices passed.
        let mut txn = env.pool.begin().await?;
        sqlx::query("DELETE FROM attestation_profiles")
            .execute(&mut *txn)
            .await?;
        let mut selection = gpu_allowlist();
        selection
            .component_ids
            .push(ComponentIdMatch::Exact("NOT_PRESENT".to_string()));
        db::attestation_profile::create(
            &mut txn,
            MOCK_HOST_HARDWARE_CLASS,
            &AttestationPolicyDocument::new(selection),
            "test",
        )
        .await?;
        txn.commit().await?;
        let partial = env
            .api
            .list_machine_attestation_statuses(Request::new(machine_assessment_request(machine_id)))
            .await?
            .into_inner();
        assert_eq!(
            partial.statuses[0].attestation_status,
            Some(SpdmAttestationStatus::SpdmAttPassed.into())
        );
        assert_eq!(
            partial.statuses[0].attester_selection_status(),
            SpdmAttesterSelectionStatus::PartiallySatisfied
        );

        let missing = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0xf1; 32],
            MachineType::Host,
        );
        for (scenario, machine_id, timeout, expected) in [
            ("missing ID", None, None, tonic::Code::InvalidArgument),
            (
                "zero timeout",
                Some(machine_id),
                Some(0),
                tonic::Code::InvalidArgument,
            ),
            (
                "unknown machine",
                Some(missing),
                None,
                tonic::Code::NotFound,
            ),
        ] {
            let error = env
                .api
                .list_machine_attestation_statuses(Request::new(
                    ListMachineAttestationStatusesRequest {
                        target: Some(Target::Machine(SpdmMachineAttestationMachineRequest {
                            machine_id,
                            redfish_timeout_secs: timeout,
                        })),
                    },
                ))
                .await
                .unwrap_err();
            assert_eq!(error.code(), expected, "{scenario}");
        }
        // The deadline covers database waits too; inject one rather than relying on BMC timing.
        let mut blocker = env.pool.begin().await?;
        sqlx::query("LOCK TABLE machines IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await?;
        let result = env
            .api
            .list_machine_attestation_statuses(Request::new(
                ListMachineAttestationStatusesRequest {
                    target: Some(Target::Machine(SpdmMachineAttestationMachineRequest {
                        machine_id: Some(machine_id),
                        redfish_timeout_secs: Some(1),
                    })),
                },
            ))
            .await;
        blocker.rollback().await?;
        assert_eq!(result.unwrap_err().code(), tonic::Code::DeadlineExceeded);
        Ok(())
    }

    async fn get_state_from_db(
        txn: &mut PgConnection,
        machine_id: &MachineId,
        device_id: &str,
        job_id: &uuid::Uuid,
    ) -> Result<(SpdmAttestationState, Option<chrono::DateTime<chrono::Utc>>), sqlx::Error> {
        let query = r#"
            SELECT state, completed_at
            FROM spdm_machine_devices_attestation
            WHERE machine_id = $1 AND device_id = $2 AND job_id = $3
        "#;

        let query_result: (
            sqlx::types::Json<SpdmAttestationState>,
            Option<chrono::DateTime<chrono::Utc>>,
        ) = sqlx::query_as(query)
            .bind(machine_id)
            .bind(device_id)
            .bind(job_id)
            .fetch_one(txn)
            .await?;
        Ok((query_result.0.0, query_result.1))
    }

    async fn list_machines_under_attestation(
        env: &TestEnv,
    ) -> Result<Vec<SpdmAttestationJobStatus>, tonic::Status> {
        env.api
            .list_attestation(Request::new(SpdmListAttestationRequest {
                selector: SpdmListAttestationRequestSelector::SpdmListInProgress.into(),
                ..Default::default()
            }))
            .await
            .map(|response| response.into_inner().statuses)
    }
}
