// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_test_harness::prelude::*;
use carbide_uuid::rack::RackProfileId;
use librms::protos::rack_manager as rms;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::metadata::Metadata;
use model::power_shelf::{PowerShelfControllerState, PowerShelfStatus};
use model::test_support::{TEST_RMS_RACK_PROFILE_ID, power_shelf_config, rms_rack_profiles};

use crate::common::{
    ControllerEnv, load_power_shelf, seed_pmc_endpoint, set_power_shelf_controller_state,
};

#[sqlx_test]
async fn ready_persists_observations_and_preserves_them_after_failed_polls(pool: PgPool) {
    let mut rack_profiles = rms_rack_profiles();
    let capabilities = &mut rack_profiles
        .rack_profiles
        .get_mut(TEST_RMS_RACK_PROFILE_ID)
        .unwrap()
        .rack_capabilities;
    capabilities.compute.count = 0;
    capabilities.switch.count = 0;
    capabilities.power_shelf.count = 1;
    let env = ControllerEnv::with_rack_profiles(pool.clone(), rack_profiles).await;
    let shelf_id = env
        .harness
        .create_power_shelf(power_shelf_config("observed shelf"))
        .await
        .id;
    let rack = env
        .harness
        .create_rack(RackProfileId::new(TEST_RMS_RACK_PROFILE_ID))
        .await;
    let pmc_mac = seed_pmc_endpoint(&pool, shelf_id).await.unwrap();
    let mut txn = pool.begin().await.unwrap();
    db::expected_power_shelf::create(
        &mut txn,
        ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: pmc_mac,
            bmc_username: "root".into(),
            bmc_password: "password".into(),
            serial_number: "observed-shelf".into(),
            bmc_ip_address: None,
            metadata: Metadata::default(),
            rack_id: Some(rack.id.clone()),
            bmc_retain_credentials: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE power_shelves SET rack_id = $1, bmc_mac_address = $2 WHERE id = $3")
        .bind(&rack.id)
        .bind(pmc_mac)
        .bind(shelf_id)
        .execute(txn.as_mut())
        .await
        .unwrap();
    set_power_shelf_controller_state(&mut txn, &shelf_id, PowerShelfControllerState::Ready)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    env.credential_manager
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: pmc_mac,
                },
            },
            &Credentials::UsernamePassword {
                username: "root".into(),
                password: "password".into(),
            },
        )
        .await
        .unwrap();

    let mut shelf = load_power_shelf(&pool, &shelf_id).await;
    shelf.status = Some(PowerShelfStatus {
        shelf_name: "retained shelf name".into(),
        power_state: "on".into(),
        health_status: "retained health".into(),
    });
    db::power_shelf::update(&shelf, pool.acquire().await.unwrap().as_mut())
        .await
        .unwrap();

    let observation = |power_state: Option<&str>| {
        Ok(rms::BatchGetPowerStateResponse {
            response: Some(rms::NodeBatchResponse {
                status: rms::ReturnCode::Success as i32,
                ..Default::default()
            }),
            node_power_states: power_state
                .map(|power_state| rms::NodePowerState {
                    node_id: shelf_id.to_string(),
                    pstate: power_state.into(),
                })
                .into_iter()
                .collect(),
        })
    };
    for (iteration, (scenario, response, expected_state)) in [
        (
            "unknown is an observation",
            observation(Some("UNKNOWN")),
            "unknown",
        ),
        (
            "transition remains distinct",
            observation(Some("PoweringOff")),
            "poweringoff",
        ),
        (
            "missing state preserves observation",
            observation(None),
            "poweringoff",
        ),
        (
            "failed read preserves observation",
            Err(librms::RackManagerError::ApiInvocationError(
                tonic::Status::unavailable("injected read failure"),
            )),
            "poweringoff",
        ),
        (
            "later read retries successfully",
            observation(Some("ON")),
            "on",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        env.rms_sim
            .queue_batch_get_power_state_response(response)
            .await;
        env.run_controller_iteration().await;
        let stored = load_power_shelf(&pool, &shelf_id).await;
        assert_eq!(
            stored.controller_state.value,
            PowerShelfControllerState::Ready,
            "{scenario}"
        );
        let status = stored.status.expect("poll should retain the shelf status");
        assert_eq!(status.power_state, expected_state, "{scenario}");
        assert_eq!(status.shelf_name, "retained shelf name", "{scenario}");
        assert_eq!(status.health_status, "retained health", "{scenario}");
        let reads = env.rms_sim.submitted_batch_get_power_state_requests().await;
        assert_eq!(
            reads.len(),
            iteration + 1,
            "{scenario}: Ready must reach RMS"
        );
        let nodes = &reads[iteration].nodes.as_ref().unwrap().nodes;
        assert_eq!(nodes.len(), 1, "{scenario}");
        assert_eq!(nodes[0].node_id, shelf_id.to_string(), "{scenario}");
    }
    assert!(
        env.rms_sim
            .submitted_batch_set_power_state_requests()
            .await
            .is_empty()
    );
}
