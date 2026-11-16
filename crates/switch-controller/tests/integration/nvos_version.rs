// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Persisted Ready-state NVOS refreshes through the RMS inventory reader.

use std::sync::Arc;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_switch_controller::context::SwitchStateHandlerServices;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use carbide_uuid::rack::RackProfileId;
use carbide_uuid::switch::SwitchId;
use db::switch as db_switch;
use librms::protos::rack_manager as rms;
use model::switch::{
    InitializingState, NvosVersion, Switch, SwitchControllerState, SwitchMaintenanceOperation,
};
use model::test_support::TEST_RMS_RACK_PROFILE_ID;

use crate::common::{
    ControllerEnv, default_switch_mtls_services, new_switch, set_switch_rack_id,
    transition_switch_controller_state,
};
use crate::state_controller::{build_test_component_manager, run_switch_controller_with_services};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn load_switch(pool: &sqlx::PgPool, switch_id: &SwitchId) -> TestResult<Switch> {
    let mut conn = pool.acquire().await?;
    Ok(db_switch::find_by_id(&mut conn, switch_id)
        .await?
        .expect("switch should exist"))
}

async fn ready_switch(
    env: &ControllerEnv,
    rms: Arc<MockRmsApi>,
) -> TestResult<(SwitchId, SwitchStateHandlerServices)> {
    let switch_id = new_switch(env, None, None).await?;
    let rack = env
        .harness
        .create_rack(RackProfileId::new(TEST_RMS_RACK_PROFILE_ID))
        .await;
    let bmc_mac_address = load_switch(&env.pool, &switch_id)
        .await?
        .bmc_mac_address
        .expect("switch should have a BMC MAC");
    for (key, username) in [
        (
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
            },
            "bmc-admin",
        ),
        (
            CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            "nvos-admin",
        ),
    ] {
        env.test_credential_manager
            .set_credentials(&key, &Credentials::new(username, "test-password"))
            .await
            .expect("test credentials should be stored");
    }
    let services = SwitchStateHandlerServices {
        db_pool: env.pool.clone(),
        component_manager: build_test_component_manager(env, Some(rms)).await,
        credential_manager: env.test_credential_manager.clone(),
        switch_mtls_services: default_switch_mtls_services(),
        per_object_metrics_registry: env.per_object_metrics_registry.clone(),
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_connector: bmc_backend::BmcConnector::Libredfish(env.redfish_sim.clone()),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
            db::credential_rotation::CredentialRotationType::Bmc,
        ),
        bmc_rotation_enabled: false,
    };

    let mut txn = env.pool.begin().await?;
    set_switch_rack_id(&mut txn, &switch_id, &rack.id).await?;
    transition_switch_controller_state(
        &mut txn,
        &switch_id,
        SwitchControllerState::Initializing {
            initializing_state: InitializingState::WaitForOsMachineInterface,
        },
    )
    .await?;
    txn.commit().await?;
    // Use ingestion's interface association so RMS receives the same primary
    // NVOS endpoint that a managed switch uses in production.
    run_switch_controller_with_services(
        env.pool.clone(),
        env.api.work_lock_manager_handle(),
        services.clone(),
    )
    .await;
    assert!(matches!(
        load_switch(&env.pool, &switch_id)
            .await?
            .controller_state
            .value,
        SwitchControllerState::Configuring { .. }
    ));

    let mut txn = env.pool.begin().await?;
    transition_switch_controller_state(&mut txn, &switch_id, SwitchControllerState::Ready).await?;
    txn.commit().await?;
    Ok((switch_id, services))
}

fn inventory_response(
    switch_id: SwitchId,
    version: &str,
) -> rms::BatchGetFirmwareInventoryResponse {
    rms::BatchGetFirmwareInventoryResponse {
        response: Some(rms::NodeBatchResponse {
            status: rms::ReturnCode::Success as i32,
            ..Default::default()
        }),
        nodes: vec![rms::NodeFirmwareInventory {
            node_id: switch_id.to_string(),
            firmware_list: vec![rms::FirmwareInventoryInfo {
                name: "NVOS".into(),
                version: version.into(),
                ..Default::default()
            }],
        }],
    }
}

#[sqlx_test]
async fn ready_nvos_version_refresh_retries_and_persists_observation(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let rms = Arc::new(MockRmsApi::new());
    let (switch_id, services) = ready_switch(&env, rms.clone()).await?;
    let initial = load_switch(&pool, &switch_id).await?;
    assert_eq!(initial.nvos_version, NvosVersion::default());

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        services.clone(),
    )
    .await;
    let idle = load_switch(&pool, &switch_id).await?;
    assert_eq!(idle.nvos_version, NvosVersion::default());
    assert!(rms.batch_get_firmware_inventory_calls().await.is_empty());

    let previous = NvosVersion {
        version: Some("nvos-old".into()),
        refresh_nvos_version: false,
    };
    let mut txn = pool.begin().await?;
    sqlx::query("UPDATE switches SET nvos_version = $1 WHERE id = $2")
        .bind(sqlx::types::Json(&previous))
        .bind(switch_id)
        .execute(txn.as_mut())
        .await?;
    db_switch::request_nvos_version_refresh(&mut txn, switch_id).await?;
    txn.commit().await?;
    rms.enqueue_batch_get_firmware_inventory(Err(tonic::Status::unavailable(
        "injected transient RMS outage",
    )
    .into()))
        .await;
    rms.enqueue_batch_get_firmware_inventory(Ok(inventory_response(switch_id, "nvos-25.02.4440")))
        .await;

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        services.clone(),
    )
    .await;
    let failed = load_switch(&pool, &switch_id).await?;
    assert_eq!(failed.controller_state.value, SwitchControllerState::Ready);
    assert_eq!(failed.nvos_version.version, previous.version);
    assert!(failed.nvos_version.refresh_nvos_version);
    assert_eq!(rms.batch_get_firmware_inventory_calls().await.len(), 1);

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        services.clone(),
    )
    .await;
    let refreshed = load_switch(&pool, &switch_id).await?;
    assert_eq!(
        refreshed.controller_state.value,
        SwitchControllerState::Ready
    );
    assert_eq!(
        refreshed.nvos_version,
        NvosVersion {
            version: Some("nvos-25.02.4440".into()),
            refresh_nvos_version: false,
        }
    );
    let calls = rms.batch_get_firmware_inventory_calls().await;
    assert_eq!(calls.len(), 2);
    let nodes = &calls[1].nodes.as_ref().expect("requested nodes").nodes;
    assert_eq!(nodes.len(), 1);
    let node = &nodes[0];
    assert_eq!(node.node_id, switch_id.to_string());
    assert_eq!(
        node.bmc_endpoint
            .as_ref()
            .unwrap()
            .credentials
            .as_ref()
            .unwrap()
            .auth,
        Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
            username: "bmc-admin".into(),
            password: "test-password".into(),
        }))
    );
    assert_eq!(
        node.host_endpoint
            .as_ref()
            .unwrap()
            .credentials
            .as_ref()
            .unwrap()
            .auth,
        Some(rms::credentials::Auth::UserPass(rms::UsernamePassword {
            username: "nvos-admin".into(),
            password: "test-password".into(),
        }))
    );

    run_switch_controller_with_services(pool.clone(), env.api.work_lock_manager_handle(), services)
        .await;
    assert_eq!(rms.batch_get_firmware_inventory_calls().await.len(), 2);
    assert_eq!(
        load_switch(&pool, &switch_id).await?.nvos_version,
        refreshed.nvos_version
    );
    Ok(())
}

#[sqlx_test]
async fn ready_maintenance_takes_precedence_over_nvos_version_refresh(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let rms = Arc::new(MockRmsApi::new());
    let (switch_id, services) = ready_switch(&env, rms.clone()).await?;
    let mut txn = pool.begin().await?;
    db_switch::request_nvos_version_refresh(&mut txn, switch_id).await?;
    db_switch::set_switch_maintenance_requested(
        &mut txn,
        switch_id,
        "test",
        SwitchMaintenanceOperation::PowerOff { graceful: true },
    )
    .await?;
    txn.commit().await?;
    let request = load_switch(&pool, &switch_id)
        .await?
        .switch_maintenance_requested
        .expect("maintenance should be requested");

    run_switch_controller_with_services(pool.clone(), env.api.work_lock_manager_handle(), services)
        .await;
    let switch = load_switch(&pool, &switch_id).await?;
    assert_eq!(
        switch.controller_state.value,
        SwitchControllerState::maintenance_for_request(request)
    );
    assert!(switch.nvos_version.refresh_nvos_version);
    assert_eq!(switch.nvos_version.version, None);
    assert!(rms.batch_get_firmware_inventory_calls().await.is_empty());
    Ok(())
}
