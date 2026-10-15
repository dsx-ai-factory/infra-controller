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
    use config_version::ConfigVersion;
    use model::attestation::spdm::{SpdmAttestationState, SpdmObjectId};
    use model::controller_outcome::PersistentStateHandlerOutcome;
    use rpc::forge::forge_server::Forge;
    use rpc::forge::{
        SpdmAttestationJobStatus, SpdmListAttestationRequest, SpdmListAttestationRequestSelector,
        SpdmMachineAttestationTriggerRequest,
    };
    use sqlx::PgConnection;
    use state_controller::io::StateControllerIO;
    use tonic::Request;

    use crate::cfg::file::CarbideConfig;
    use crate::tests::common::api_fixtures::{
        TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
        create_test_env_with_overrides, get_config,
    };

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

        Ok(())
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

        assert!(response.into_inner().result.is_none());

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

        assert_eq!(
            3,
            response
                .into_inner()
                .result
                .expect("supported devices should produce an attestation result")
                .devices_under_attestation
        );

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

        // set up redfish to fail evidence collection with a terminal exception
        env.redfish_sim
            .set_get_task_trigger_evidence_returns_exception(true);

        let response = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await?;

        assert_eq!(
            3,
            response
                .into_inner()
                .result
                .expect("supported devices should produce an attestation result")
                .devices_under_attestation
        );

        // device attestations should be created
        let machine_ids = list_machines_under_attestation(&env).await?;

        assert_eq!(1, machine_ids.len());

        let mut txn = env.pool.begin().await.unwrap();

        // let's loop until we are triggering evidence and verify that
        for _ in 0..8 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        let object_ids = db::attestation::spdm::find_machine_ids_for_attestation(&mut txn)
            .await
            .expect("Failed getting object ids for attestation");

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert!(
                matches!(
                    attestation_state,
                    SpdmAttestationState::TriggerEvidenceCollection { retry_count: 3 }
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
                .result
                .unwrap()
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
                .result
                .unwrap()
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
                SpdmAttestationState::TriggerEvidenceCollection { retry_count: 0 }
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
    async fn test_cancelled_by_user_goes_into_cancelled(
        pool: sqlx::PgPool,
    ) -> Result<(), eyre::Error> {
        // trigger attestation - corresponding device attestations are created
        // query attestation status - should be in progress
        // run controller iterations - should be able to:
        // - fetch metadata
        // - fetch certificate,
        // - schedule evidence
        // -  cancel the whole thing - make sure it goes into cancelled state
        // verify the state in each iteration using direct db lookups

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
            .await?
            .into_inner();
        let job_id = response
            .result
            .expect("supported devices should produce an attestation result")
            .job_id
            .expect("attestation result should include a job ID");

        // device attestations should be created now
        let statuses = list_machines_under_attestation(&env).await?;

        assert_eq!(1, statuses.len());
        assert_eq!(Some(&job_id), statuses[0].job_id.as_ref());

        // check that attestation's status is InProgress
        assert_eq!(
            rpc::forge::SpdmAttestationStatus::SpdmAttInProgress,
            rpc::forge::SpdmAttestationStatus::try_from(statuses[0].attestation_status)?
        );

        // now, look at the state of the attestation and check that it is FetchMetadata
        let mut txn = env.pool.begin().await.unwrap();

        let object_ids = db::attestation::spdm::find_machine_ids_for_attestation(&mut txn)
            .await
            .expect("Failed getting object ids for attestation");

        for object_id in &object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert_eq!(SpdmAttestationState::FetchMetadata, attestation_state);
        }

        env.run_spdm_controller_iteration_no_requeue().await;

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");
            assert_eq!(SpdmAttestationState::FetchCertificate, attestation_state);
        }

        // now proceed to FetchCertificate
        env.run_spdm_controller_iteration_no_requeue().await;

        let object_ids = db::attestation::spdm::find_machine_ids_for_attestation(&mut txn)
            .await
            .expect("Failed getting object ids for attestation");
        assert_eq!(3, object_ids.len());

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, _) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert!(
                matches!(
                    attestation_state,
                    SpdmAttestationState::TriggerEvidenceCollection { .. }
                ),
                "expected TriggerEvidenceCollection, got: {:?}",
                attestation_state
            );
        }

        // now let's cancel the whole thing
        let _ = env.api.cancel_attestation(Request::new(job_id)).await?;

        env.run_spdm_controller_iteration_no_requeue().await;

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id, job_id) = object_id;
            let (attestation_state, completed_at) =
                get_state_from_db(&mut txn, &machine_id, device_id, job_id)
                    .await
                    .expect("Failed getting attestation state from the DB");

            assert!(
                matches!(attestation_state, SpdmAttestationState::Cancelled),
                "expected Cancelled, got: {:?}",
                attestation_state
            );

            // make sure the completed_at field has been populated also
            assert!(completed_at.is_some());
        }

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
