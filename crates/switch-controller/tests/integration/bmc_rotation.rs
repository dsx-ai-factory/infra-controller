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

//! End-to-end coverage for switch-controller BMC credential rotation:
//! with the site-wide flag enabled, a staged target drives a Ready switch
//! through `SwitchControllerState::RotatingBmc` and back to Ready, converging
//! the device and persisting the rotated per-device secret. Mirrors the
//! machine-controller integration test `ready_host_converges_bmc_to_site_target`.

use carbide_credential_rotation::RotationGate;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use carbide_switch_controller::context::SwitchStateHandlerServices;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use db::credential_rotation::{
    CredentialRotationType, device_rotation_status, record_device_converged,
    set_next_target_version,
};
use db::switch as db_switch;
use mac_address::MacAddress;
use model::bmc_suppression::BmcSuppressionSubsystem;
use model::switch::{Switch, SwitchControllerState};

use crate::common::{
    ControllerEnv, default_switch_mtls_services, new_switch, transition_switch_controller_state,
};
use crate::state_controller::run_switch_controller_with_services;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const BMC: CredentialRotationType = CredentialRotationType::Bmc;

/// Stand in for site-explorer acknowledging every pending BMC suppression -- the
/// barrier the rotation gate waits on before changing a credential. This test
/// drives only the switch controller, so it plays the site-explorer ack pass
/// itself.
async fn ack_all_site_explorer_suppressions(pool: &sqlx::PgPool) {
    let mut conn = pool.acquire().await.expect("acquire connection");
    let macs: Vec<MacAddress> = db::bmc_suppression::find_all_by_subsystem(
        &mut *conn,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await
    .expect("read suppressions")
    .into_iter()
    .map(|s| s.bmc_mac_address)
    .collect();
    db::bmc_suppression::acknowledge_unacknowledged(
        &mut conn,
        &macs,
        BmcSuppressionSubsystem::SiteExplorer,
    )
    .await
    .expect("acknowledge suppressions");
}

fn per_device_key(mac: MacAddress) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: mac,
        },
    }
}

fn rotate_to_key(version: u32) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::site_wide_root(version),
    }
}

fn creds(username: &str, password: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: username.to_string(),
        password: password.to_string(),
    }
}

/// Services with the passive BMC-rotation gate toggled by `bmc_rotation_enabled`
/// and no component manager, so a Ready switch is not diverted to NVOS
/// reconciliation before reaching the BMC gate. A fresh [`RotationGate`]
/// refreshes its cached aggregate live on first use each iteration.
fn switch_services(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
    bmc_rotation_enabled: bool,
) -> SwitchStateHandlerServices {
    SwitchStateHandlerServices {
        db_pool: pool.clone(),
        component_manager: None,
        credential_manager: env.test_credential_manager.clone(),
        switch_mtls_services: default_switch_mtls_services(),
        per_object_metrics_registry: env.per_object_metrics_registry.clone(),
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: RotationGate::new_for_family(CredentialRotationType::Bmc),
        bmc_rotation_enabled,
    }
}

async fn load_switch(
    pool: &sqlx::PgPool,
    switch_id: &carbide_uuid::switch::SwitchId,
) -> TestResult<Switch> {
    let mut conn = pool.acquire().await?;
    Ok(db_switch::find_by_id(&mut conn, switch_id)
        .await?
        .expect("switch should exist"))
}

/// When the switch BMC password change lands but persisting the new per-device
/// secret to the store fails, the hardware is AHEAD of the store. The switch
/// must hold in `RotatingBmc` (keeping the site-explorer suppression) rather
/// than settling to Ready with a stale stored secret, and keep retrying until
/// the store reconciles.
#[sqlx_test]
async fn switch_store_persist_failure_holds_in_rotating_bmc_until_reconciled(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;

    let switch_id = new_switch(&env, None, None).await?;
    let bmc_mac = db_switch::find_switch_endpoints_by_ids(&pool, &[switch_id])
        .await?
        .first()
        .expect("switch endpoint row")
        .bmc_mac;

    {
        let mut txn = pool.begin().await?;
        transition_switch_controller_state(txn.as_mut(), &switch_id, SwitchControllerState::Ready)
            .await?;
        txn.commit().await?;
    }

    env.redfish_sim.seed_user("root", "old");
    env.test_credential_manager
        .set_credentials(&per_device_key(bmc_mac), &creds("root", "old"))
        .await
        .expect("staging the per-device secret should succeed");
    {
        let mut conn = pool.acquire().await?;
        record_device_converged(&mut conn, bmc_mac, BMC).await?;
        set_next_target_version(&mut conn, BMC, 0, serde_json::json!({}))
            .await?
            .expect("target must advance from version 0");
    }
    env.test_credential_manager
        .set_credentials(&rotate_to_key(1), &creds("root", "new"))
        .await
        .expect("staging the rotate-to secret should succeed");

    // Enter RotatingBmc, let the gate record the suppression, and acknowledge it.
    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        switch_services(&env, &pool, true),
    )
    .await; // Ready -> RotatingBmc
    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        switch_services(&env, &pool, true),
    )
    .await; // gate records the suppression and waits
    ack_all_site_explorer_suppressions(&pool).await;

    // The store write now fails: the hardware change will land but the persist
    // will not, leaving the BMC ahead of the store.
    env.test_credential_manager
        .set_set_credentials_failure(true);

    // The password change succeeds on the hardware but the persist fails, so the
    // switch holds in RotatingBmc instead of returning to Ready.
    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        switch_services(&env, &pool, true),
    )
    .await;
    let switch = load_switch(&pool, &switch_id).await?;
    assert!(
        matches!(
            switch.controller_state.value,
            SwitchControllerState::RotatingBmc { .. }
        ),
        "a persist failure must hold in RotatingBmc, got {:?}",
        switch.controller_state.value,
    );
    assert!(
        db::bmc_suppression::is_suppressed(&pool, bmc_mac, BmcSuppressionSubsystem::SiteExplorer)
            .await?,
        "the rotation suppression must be retained while the store lags"
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, bmc_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            !status.converged,
            "hardware ahead of store is not converged"
        );
        assert!(
            !status.quarantined,
            "a persist failure must not record a backoff"
        );
    }
    assert_eq!(
        env.redfish_sim.user_password("root").as_deref(),
        Some("new"),
        "the hardware change must have landed even though the persist failed"
    );
    assert_eq!(
        env.test_credential_manager
            .get_credentials(&per_device_key(bmc_mac))
            .await
            .expect("reading the per-device secret should succeed")
            .expect("per-device secret should still be set"),
        creds("root", "old"),
        "the stored secret must still lag after the failed persist"
    );

    // The store becomes writable again: the next tick reconciles and returns to
    // Ready.
    env.test_credential_manager
        .set_set_credentials_failure(false);
    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        switch_services(&env, &pool, true),
    )
    .await;
    let switch = load_switch(&pool, &switch_id).await?;
    assert!(
        matches!(switch.controller_state.value, SwitchControllerState::Ready),
        "expected Ready once the store reconciles, got {:?}",
        switch.controller_state.value,
    );
    assert!(
        !db::bmc_suppression::is_suppressed(&pool, bmc_mac, BmcSuppressionSubsystem::SiteExplorer)
            .await?,
        "resume must delete the rotation suppression once reconciled"
    );
    {
        let mut conn = pool.acquire().await?;
        let status = device_rotation_status(&mut conn, BMC, bmc_mac)
            .await?
            .expect("device rotation row should exist");
        assert!(
            status.converged,
            "device should converge once the store reconciles"
        );
        assert_eq!(status.current_version, Some(1));
    }
    assert_eq!(
        env.test_credential_manager
            .get_credentials(&per_device_key(bmc_mac))
            .await
            .expect("reading the per-device secret should succeed")
            .expect("per-device secret should still be set"),
        creds("root", "new"),
        "the store is reconciled to the new password once the write succeeds"
    );

    Ok(())
}
