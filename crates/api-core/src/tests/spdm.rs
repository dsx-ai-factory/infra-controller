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
    use model::attestation::spdm::{SpdmAttestationState, SpdmObjectId};
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
    async fn test_spdm_controller_persistence_honors_supplied_versions(pool: sqlx::PgPool) {
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
        let env = create_test_env_with_overrides(
            pool,
            TestEnvOverrides {
                config: Some(spdm_enabled_config()),
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

        // Let A finish. The controller must save its evidence and release its
        // claims as it advances to verification.
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
        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_unexpected_evidence_collection_waits_for_cancellation(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};

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

        for (scenario, finish_collection, user_cancelled, cancel_error) in [
            ("confirmed stop permits retry", true, false, None),
            (
                "cancellation request failure still observes the cleanup deadline",
                false,
                false,
                Some(libredfish::RedfishError::GenericError {
                    error: "temporary cancellation failure".to_string(),
                }),
            ),
            (
                "user cancellation wins without extending cleanup",
                false,
                true,
                None,
            ),
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
            // Model a BMC stuck in Cancelling, an unexpected but nonterminal state.
            for device in &devices {
                let SpdmAttestationState::PollEvidenceCollection { task_id, .. } = &device.state
                else {
                    panic!(
                        "{scenario}: expected evidence collection, got {:?}",
                        device.state
                    );
                };
                assert!(!client.cancel_task(task_id).await?);
            }
            let initial_cancellations = env.redfish_sim.task_cancellation_requests().len();
            let initial_collections = env.redfish_sim.evidence_collection_requests().len();
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
                    if reason == "Waiting for BMC evidence collection to stop before retrying"),
                    "{scenario}"
                );
            }

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
                        SpdmAttestationState::WaitForTaskCancellation { retry_count: 0, .. }
                    ),
                    "{scenario}"
                );
                assert!(snapshot.evidence_collection_locked, "{scenario}");
            }
            assert_eq!(
                env.redfish_sim.task_cancellation_requests().len(),
                initial_cancellations + devices.len(),
                "unexpected polling requests cancellation before entering the waiting state"
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
            } else {
                sqlx::query("UPDATE spdm_machine_devices_attestation SET state_version = $2 WHERE job_id = $1")
                    .bind(job_id).bind(old_version).execute(&env.pool).await?;
            }
            if user_cancelled {
                env.api
                    .cancel_attestation(Request::new(job_id.into()))
                    .await?;
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
                let expected = if user_cancelled {
                    SpdmAttestationState::Cancelled
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
                    user_cancelled,
                    "{scenario}"
                );
                assert!(
                    snapshot.evidence.is_none(),
                    "cleanup must not read evidence"
                );
            }
            if !finish_collection && !user_cancelled {
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
    async fn test_cancelled_by_user_goes_into_cancelled(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        struct Case {
            scenario: &'static str,
            trigger_iterations: usize,
            cancel_error: Option<libredfish::RedfishError>,
            finish_collection: bool,
            claim_without_task: bool,
        }
        let cases = [
            Case {
                scenario: "cancel before collection starts",
                trigger_iterations: 2,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: false,
            },
            Case {
                scenario: "retry a transient cancellation failure, then wait for the BMC",
                trigger_iterations: 3,
                cancel_error: Some(libredfish::RedfishError::GenericError {
                    error: "temporary cancellation failure".to_string(),
                }),
                finish_collection: true,
                claim_without_task: false,
            },
            Case {
                scenario: "unsupported cancellation releases the lock only at the deadline",
                trigger_iterations: 3,
                cancel_error: Some(libredfish::RedfishError::NotSupported(
                    "cancel task".to_string(),
                )),
                finish_collection: false,
                claim_without_task: false,
            },
            Case {
                scenario: "a collection claim without a task ID uses the deadline",
                trigger_iterations: 2,
                cancel_error: None,
                finish_collection: false,
                claim_without_task: true,
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
        for case in cases {
            // Host setup performs its own attestation before the scenario starts.
            env.redfish_sim.set_evidence_collection_running(false);
            let (machine_id, _dpu_id) = create_managed_host(&env).await.into();
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
            env.api
                .cancel_attestation(Request::new(job_id.into()))
                .await?;
            env.run_spdm_controller_iteration_no_requeue().await;

            let mut txn = env.db_txn().await;
            let devices =
                db::attestation::spdm::get_attestations_for_job_id(&mut txn, &job_id).await?;
            assert_eq!(devices.len(), 3, "{}", case.scenario);
            let collecting = case.trigger_iterations == 3;
            let waiting = collecting || case.claim_without_task;
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
                if collecting { 3 } else { 0 },
                "{}",
                case.scenario
            );
            if collecting {
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
                        .all(|device| device.state == SpdmAttestationState::Cancelled)
                );
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
                if collecting {
                    assert!(
                        matches!(
                            snapshot.state,
                            SpdmAttestationState::WaitForTaskCancellation { .. }
                        ),
                        "{}",
                        case.scenario
                    );
                }
            }

            if case.finish_collection {
                env.redfish_sim.set_evidence_collection_running(false);
            } else {
                // Advance the persisted deadline without a real two-minute sleep.
                sqlx::query("UPDATE spdm_machine_devices_attestation SET cancelled_at = NOW() - INTERVAL '2 minutes' WHERE job_id = $1")
                    .bind(job_id).execute(&env.pool).await?;
            }
            let cancelled_at: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT cancelled_at FROM spdm_machine_devices_attestation WHERE job_id = $1 LIMIT 1")
                .bind(job_id).fetch_one(&env.pool).await?;
            // Repeated user requests must not postpone the original deadline.
            env.api
                .cancel_attestation(Request::new(job_id.into()))
                .await?;
            let repeated_cancelled_at: chrono::DateTime<Utc> = sqlx::query_scalar("SELECT cancelled_at FROM spdm_machine_devices_attestation WHERE job_id = $1 LIMIT 1")
                .bind(job_id).fetch_one(&env.pool).await?;
            assert_eq!(cancelled_at, repeated_cancelled_at, "{}", case.scenario);
            env.run_spdm_controller_iteration_no_requeue().await;
            for device in &devices {
                let snapshot = db::attestation::spdm::load_snapshot_for_machine_and_device_id(
                    &mut txn,
                    &machine_id,
                    &device.device_id,
                    &job_id,
                )
                .await?;
                assert_eq!(
                    snapshot.state,
                    SpdmAttestationState::Cancelled,
                    "{}",
                    case.scenario
                );
                assert!(!snapshot.evidence_collection_locked, "{}", case.scenario);
                assert!(snapshot.completed_at.is_some(), "{}", case.scenario);
                assert!(
                    snapshot.evidence.is_none(),
                    "cancelled jobs must not read evidence"
                );
            }
        }
        Ok(())
    }

    #[crate::sqlx_test]
    async fn predicted_machine_attestation_rpc_paginates_and_validates(
        pool: sqlx::PgPool,
    ) -> eyre::Result<()> {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        use rpc::forge::SpdmMachineAttestationFleetRequest;

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
        let invalid_tokens = [
            ("invalid enum", Some(-1), String::new()),
            ("invalid base64", None, "%%%".to_string()),
            ("invalid token payload", None, URL_SAFE_NO_PAD.encode(b"{}")),
            ("changed filter", None, first.next_page_token),
            (
                "unsupported token version",
                passed,
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
                    "version": 2, "after_machine_id": machines[0], "attestation_status": passed,
                }))?),
            ),
            (
                "invalid machine cursor",
                passed,
                URL_SAFE_NO_PAD.encode(serde_json::to_vec(&serde_json::json!({
                    "version": 1, "after_machine_id": "invalid", "attestation_status": passed,
                }))?),
            ),
        ];
        for (scenario, attestation_status, page_token) in invalid_tokens {
            let error = env
                .api
                .list_machine_attestation_statuses(Request::new(fleet_request(
                    SpdmMachineAttestationFleetRequest {
                        attestation_status,
                        page_token,
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
            }))
            .await
            .map(|response| response.into_inner().statuses)
    }
}
