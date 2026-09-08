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

    use carbide_uuid::machine::MachineId;
    use model::attestation::spdm::{SpdmAttestationState, SpdmObjectId};
    use rpc::forge::forge_server::Forge;
    use rpc::forge::{
        SpdmListAttestationMachinesRequest, SpdmListAttestationMachinesRequestSelector,
        SpdmMachineAttestationStatus, SpdmMachineAttestationTriggerRequest,
        spdm_list_attestation_machines_request,
    };
    use sqlx::PgConnection;
    //use sqlx::PgConnection;
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

        assert_eq!(0, response.into_inner().devices_under_attestation);

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
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, _) = get_state_from_db(&mut txn, &machine_id, device_id)
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

        // set up redfish to interrupt evidence collection
        env.redfish_sim
            .set_get_task_trigger_evidence_returns_interrupted(true);

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

        // let's loop until we are triggering evidence and verify that
        for _ in 0..8 {
            env.run_spdm_controller_iteration_no_requeue().await;
        }

        let object_ids = db::attestation::spdm::find_machine_ids_for_attestation(&mut txn)
            .await
            .expect("Failed getting object ids for attestation");

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, _) = get_state_from_db(&mut txn, &machine_id, device_id)
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
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, _) = get_state_from_db(&mut txn, &machine_id, device_id)
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
        let _ = env
            .api
            .trigger_machine_attestation(Request::new(SpdmMachineAttestationTriggerRequest {
                machine_id: Some(machine_id.into()),
                redfish_timeout_secs: u32::MAX,
            }))
            .await?;

        // device attestations should be created now
        let statuses = list_machines_under_attestation(&env).await?;

        assert_eq!(1, statuses.len());

        let machine_id = statuses[0].machine_id.expect("missing machine id");

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
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, _) = get_state_from_db(&mut txn, &machine_id, device_id)
                .await
                .expect("Failed getting attestation state from the DB");

            assert_eq!(SpdmAttestationState::FetchMetadata, attestation_state);
        }

        env.run_spdm_controller_iteration_no_requeue().await;

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, _) = get_state_from_db(&mut txn, &machine_id, device_id)
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
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, _) = get_state_from_db(&mut txn, &machine_id, device_id)
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
        let _ = env
            .api
            .cancel_machine_attestation(Request::new(machine_id))
            .await?;

        env.run_spdm_controller_iteration_no_requeue().await;

        for object_id in &*object_ids {
            let SpdmObjectId(_, device_id) = object_id;
            let (attestation_state, completed_at) =
                get_state_from_db(&mut txn, &machine_id, device_id)
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
    ) -> Result<(SpdmAttestationState, Option<chrono::DateTime<chrono::Utc>>), sqlx::Error> {
        let query = r#"
            SELECT state, completed_at
            FROM spdm_machine_devices_attestation
            WHERE machine_id = $1 AND device_id = $2
        "#;

        let query_result: (
            sqlx::types::Json<SpdmAttestationState>,
            Option<chrono::DateTime<chrono::Utc>>,
        ) = sqlx::query_as(query)
            .bind(machine_id)
            .bind(device_id)
            .fetch_one(txn)
            .await?;
        Ok((query_result.0.0, query_result.1))
    }

    async fn list_machines_under_attestation(
        env: &TestEnv,
    ) -> Result<Vec<SpdmMachineAttestationStatus>, tonic::Status> {
        env.api
            .list_attestation_machines(Request::new(SpdmListAttestationMachinesRequest {
                variant: Some(spdm_list_attestation_machines_request::Variant::Selector(
                    SpdmListAttestationMachinesRequestSelector::SpdmListInProgress.into(),
                )),
            }))
            .await
            .map(|response| response.into_inner().statuses)
    }
}
