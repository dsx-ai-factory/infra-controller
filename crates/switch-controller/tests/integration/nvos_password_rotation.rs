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

use std::sync::Arc;

use carbide_secrets::credentials::{
    CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use carbide_switch_controller::context::{
    SwitchStateHandlerContextObjects, SwitchStateHandlerServices,
};
use carbide_switch_controller::metrics::SwitchMetrics;
use carbide_switch_controller::nvos_password_rotation::{
    NvosPasswordRotationOutcome, reconcile_nvos_password_rotation,
};
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use component_manager::mock::MockNvSwitchManager;
use component_manager::nv_switch_manager::SwitchPasswordRotationState;
use db::switch as db_switch;
use model::switch::{ConfiguringState, Switch, SwitchControllerState};
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::StateHandlerContext;

use crate::common::{
    ControllerEnv, default_switch_mtls_services, new_switch, transition_switch_controller_state,
};
use crate::state_controller::{mock_component_manager, run_switch_controller_with_services};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CURRENT_PASSWORD: &str = "Current-Nvos-Password-0!";
const TARGET_PASSWORD: &str = "Next-Nvos-Password-1!";

async fn create_switch(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
    name: Option<String>,
) -> TestResult<(carbide_uuid::switch::SwitchId, mac_address::MacAddress)> {
    let switch_id = new_switch(env, name, None).await?;

    let bmc_mac_address = db_switch::find_switch_endpoints_by_ids(pool, &[switch_id])
        .await?
        .first()
        .expect("switch endpoint row")
        .bmc_mac;

    env.test_credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            &Credentials::UsernamePassword {
                username: "nvos-admin".to_string(),
                password: CURRENT_PASSWORD.to_string(),
            },
        )
        .await
        .expect("failed to seed per-switch NVOS credential");

    Ok((switch_id, bmc_mac_address))
}

async fn publish_target(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
    current_version: Option<i32>,
    password: &str,
) -> TestResult {
    let target_version = current_version.map_or(0, |version| version + 1);
    let target_version_u32 = u32::try_from(target_version)?;

    env.test_credential_manager
        .create_credentials(
            &CredentialKey::switch_nvos_site_admin(target_version_u32),
            &Credentials::UsernamePassword {
                username: String::new(),
                password: password.to_string(),
            },
        )
        .await
        .expect("failed to seed NVOS target credential");

    let mut txn = pool.begin().await?;

    let staged = match current_version {
        Some(current_version) => {
            db::credential_rotation::set_next_target_version(
                txn.as_mut(),
                db::credential_rotation::CredentialRotationType::Nvos,
                current_version,
                serde_json::json!({}),
            )
            .await?
        }
        None => {
            db::credential_rotation::set_initial_target_version(
                txn.as_mut(),
                db::credential_rotation::CredentialRotationType::Nvos,
                serde_json::json!({}),
            )
            .await?
        }
    };

    txn.commit().await?;

    assert_eq!(
        staged.map(|target| target.target_version),
        Some(target_version)
    );

    Ok(())
}

async fn insert_current_version(
    pool: &sqlx::PgPool,
    bmc_mac_address: mac_address::MacAddress,
    version: i32,
) -> TestResult {
    sqlx::query(
        "INSERT INTO device_credential_rotation \
             (device_mac, credential_type, current_version) \
         VALUES ($1, 'nvos', $2)",
    )
    .bind(bmc_mac_address)
    .bind(version)
    .execute(pool)
    .await?;

    Ok(())
}

async fn stage_submitted_rotation(
    pool: &sqlx::PgPool,
    bmc_mac_address: mac_address::MacAddress,
    target_version: i32,
    job_id: &str,
) -> TestResult<i32> {
    let mut txn = pool.begin().await?;

    let attempt = db::credential_rotation::record_device_rotation_started(
        txn.as_mut(),
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Nvos,
        target_version,
    )
    .await?
    .expect("stage rotation");

    let submitted = db::credential_rotation::record_device_rotation_submitted(
        txn.as_mut(),
        bmc_mac_address,
        db::credential_rotation::CredentialRotationType::Nvos,
        target_version,
        attempt,
        job_id,
    )
    .await?;

    txn.commit().await?;
    assert!(submitted);
    Ok(attempt)
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

async fn operation_state(
    pool: &sqlx::PgPool,
    bmc_mac_address: mac_address::MacAddress,
) -> TestResult<db::credential_rotation::DeviceRotationOperationState> {
    let mut conn = pool.acquire().await?;

    Ok(db::credential_rotation::device_rotation_operation_state(
        &mut *conn,
        db::credential_rotation::CredentialRotationType::Nvos,
        bmc_mac_address,
    )
    .await?
    .expect("NVOS operation state should exist"))
}

async fn reconcile(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
    switch_id: &carbide_uuid::switch::SwitchId,
    manager: MockNvSwitchManager,
) -> TestResult<NvosPasswordRotationOutcome> {
    let switch = load_switch(pool, switch_id).await?;

    let mut services = switch_services(env, pool, manager);

    let mut metrics = SwitchMetrics::default();
    let mut pending_db_writes = DbWriteBatch::new();

    let mut ctx = StateHandlerContext::<SwitchStateHandlerContextObjects> {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };

    Ok(reconcile_nvos_password_rotation(switch_id, &switch, &mut ctx).await?)
}

fn switch_services(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
    manager: MockNvSwitchManager,
) -> SwitchStateHandlerServices {
    SwitchStateHandlerServices {
        db_pool: pool.clone(),
        component_manager: Some(mock_component_manager(Arc::new(manager))),
        credential_manager: env.test_credential_manager.clone(),
        switch_mtls_services: default_switch_mtls_services(),
        per_object_metrics_registry: env.per_object_metrics_registry.clone(),
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
            db::credential_rotation::CredentialRotationType::Bmc,
        ),
        bmc_rotation_enabled: false,
    }
}

async fn run_controller(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
    manager: MockNvSwitchManager,
    passes: usize,
) {
    for _ in 0..passes {
        run_switch_controller_with_services(
            pool.clone(),
            env.api.work_lock_manager_handle(),
            switch_services(env, pool, manager.clone()),
        )
        .await;
    }
}

async fn prepare_version_one_rotation(
    env: &ControllerEnv,
    pool: &sqlx::PgPool,
) -> TestResult<(carbide_uuid::switch::SwitchId, mac_address::MacAddress)> {
    publish_target(env, pool, None, CURRENT_PASSWORD).await?;
    let (switch_id, bmc_mac_address) = create_switch(env, pool, None).await?;

    insert_current_version(pool, bmc_mac_address, 0).await?;
    publish_target(env, pool, Some(0), TARGET_PASSWORD).await?;
    Ok((switch_id, bmc_mac_address))
}

#[sqlx_test]
async fn configuring_skips_credentials_when_rotation_is_not_actionable(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;

    let cases = [
        (
            "uninitialized target",
            false,
            true,
            MockNvSwitchManager::default().with_password_rotation_enabled(),
        ),
        (
            "unsupported backend",
            true,
            false,
            MockNvSwitchManager::default(),
        ),
    ];

    for (index, (case, publish, clear_bmc, manager)) in cases.into_iter().enumerate() {
        if publish {
            publish_target(&env, &pool, None, TARGET_PASSWORD).await?;
        }

        let (switch_id, bmc_mac_address) =
            create_switch(&env, &pool, Some(format!("Switch{}", index + 1))).await?;

        sqlx::query(
            "UPDATE expected_switches \
             SET nvos_username = 'nvos-admin', nvos_password = NULL \
             WHERE bmc_mac_address = $1",
        )
        .bind(bmc_mac_address)
        .execute(&pool)
        .await?;

        if clear_bmc {
            sqlx::query("UPDATE switches SET bmc_mac_address = NULL WHERE id = $1")
                .bind(switch_id)
                .execute(&pool)
                .await?;
        }

        env.test_credential_manager
            .delete_credentials(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
            .await
            .expect("failed to remove the per-switch NVOS credential");

        let mut txn = pool.begin().await?;

        transition_switch_controller_state(
            txn.as_mut(),
            &switch_id,
            SwitchControllerState::Configuring {
                config_state: ConfiguringState::RotateOsPassword,
            },
        )
        .await?;

        txn.commit().await?;

        run_controller(&env, &pool, manager, 1).await;

        let switch = load_switch(&pool, &switch_id).await?;

        assert!(
            matches!(
                switch.controller_state.value,
                SwitchControllerState::FetchInfo
                    | SwitchControllerState::Validating { .. }
                    | SwitchControllerState::BomValidating { .. }
                    | SwitchControllerState::Ready
            ),
            "{case} should bypass malformed credential data"
        );
    }

    Ok(())
}

#[sqlx_test]
async fn ready_switch_submits_version_zero_target(pool: sqlx::PgPool) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;

    publish_target(&env, &pool, None, TARGET_PASSWORD).await?;
    let (switch_id, bmc_mac_address) = create_switch(&env, &pool, None).await?;

    sqlx::query(
        "UPDATE expected_switches \
         SET nvos_username = 'corrected-admin', \
             nvos_password = 'Corrected-Nvos-Password-0!' \
         WHERE bmc_mac_address = $1",
    )
    .bind(bmc_mac_address)
    .execute(&pool)
    .await?;

    let mut txn = pool.begin().await?;

    transition_switch_controller_state(txn.as_mut(), &switch_id, SwitchControllerState::Ready)
        .await?;

    txn.commit().await?;

    let manager = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_expected_password_rotation_password(TARGET_PASSWORD)
        .with_password_rotation_outcome_unknown("lost response");

    run_controller(&env, &pool, manager, 2).await;

    let first = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(first.current_version, None);
    assert_eq!(first.rotating_to_version, Some(0));
    assert_eq!(first.rotate_job_id, None);
    assert_eq!(first.rotate_attempts, 1);

    sqlx::query(
        "UPDATE expected_switches SET nvos_password = 'Replacement-Nvos-Password-0!' \
         WHERE bmc_mac_address = $1",
    )
    .bind(bmc_mac_address)
    .execute(&pool)
    .await?;

    let retry = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_expected_password_rotation_password(TARGET_PASSWORD)
        .with_password_rotation_job("retry-job");

    reconcile(&env, &pool, &switch_id, retry).await?;

    let current = env
        .test_credential_manager
        .get_credentials_from_writer(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
        .await
        .expect("staged rotation should be `Ok`")
        .expect("staged rotation should retain the captured current credential");

    assert_eq!(
        current,
        Credentials::UsernamePassword {
            username: "corrected-admin".to_string(),
            password: "Corrected-Nvos-Password-0!".to_string(),
        }
    );

    let second = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(second.current_version, None);
    assert_eq!(second.rotating_to_version, Some(0));
    assert_eq!(second.rotate_job_id.as_deref(), Some("retry-job"));
    assert_eq!(second.rotate_attempts, 2);

    Ok(())
}

#[sqlx_test]
async fn lost_submission_response_retries_original_target_after_later_publication(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let (switch_id, bmc_mac_address) = prepare_version_one_rotation(&env, &pool).await?;

    let unknown = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_expected_password_rotation_password(TARGET_PASSWORD)
        .with_password_rotation_outcome_unknown("lost response");

    assert!(matches!(
        reconcile(&env, &pool, &switch_id, unknown).await?,
        NvosPasswordRotationOutcome::Waiting(_)
    ));

    let first = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(first.rotating_to_version, Some(1));
    assert_eq!(first.rotate_job_id, None);
    assert_eq!(first.rotate_attempts, 1);

    assert_eq!(first.rotate_last_error_redacted, None);

    publish_target(&env, &pool, Some(1), "Recovered-Nvos-Password-2!").await?;

    reconcile(
        &env,
        &pool,
        &switch_id,
        MockNvSwitchManager::default()
            .with_password_rotation_enabled()
            .with_expected_password_rotation_password(TARGET_PASSWORD)
            .with_password_rotation_job("recovered-job"),
    )
    .await?;

    let second = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(second.rotating_to_version, Some(1));
    assert_eq!(second.rotate_job_id.as_deref(), Some("recovered-job"));
    assert_eq!(second.rotate_attempts, 2);
    assert_eq!(second.rotate_last_error_redacted, None);

    Ok(())
}

#[sqlx_test]
async fn job_observations_preserve_or_retry_staged_target(pool: sqlx::PgPool) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;

    publish_target(&env, &pool, None, CURRENT_PASSWORD).await?;
    publish_target(&env, &pool, Some(0), TARGET_PASSWORD).await?;

    let retry = |status| {
        MockNvSwitchManager::default()
            .with_password_rotation_enabled()
            .with_expected_password_rotation_password(TARGET_PASSWORD)
            .with_password_rotation_job("retry-job")
            .with_password_rotation_job_status(status)
    };

    let cases = [
        ("failed", retry(SwitchPasswordRotationState::Failed), true),
        (
            "not-found",
            retry(SwitchPasswordRotationState::NotFound),
            true,
        ),
        ("unknown", retry(SwitchPasswordRotationState::Unknown), true),
        (
            "poll-unavailable",
            MockNvSwitchManager::default()
                .with_password_rotation_enabled()
                .with_expected_password_rotation_password(TARGET_PASSWORD)
                .with_password_rotation_job("retry-job")
                .with_password_rotation_job_status_unavailable("temporary polling failure"),
            false,
        ),
        (
            "pending",
            retry(SwitchPasswordRotationState::Pending),
            false,
        ),
    ];

    for (index, (case, manager, should_replay)) in cases.into_iter().enumerate() {
        let (switch_id, bmc_mac_address) =
            create_switch(&env, &pool, Some(format!("Switch{}", index + 1))).await?;

        insert_current_version(&pool, bmc_mac_address, 0).await?;
        let old_job = format!("old-job-{case}");
        stage_submitted_rotation(&pool, bmc_mac_address, 1, &old_job).await?;

        assert!(matches!(
            reconcile(&env, &pool, &switch_id, manager).await?,
            NvosPasswordRotationOutcome::Waiting(_)
        ));

        let retried = operation_state(&pool, bmc_mac_address).await?;

        let expected_job_id = if should_replay {
            "retry-job"
        } else {
            old_job.as_str()
        };

        let expected_attempts = if should_replay { 2 } else { 1 };

        assert_eq!(retried.current_version, Some(0));
        assert_eq!(retried.rotating_to_version, Some(1));
        assert_eq!(retried.rotate_job_id.as_deref(), Some(expected_job_id));
        assert_eq!(retried.rotate_attempts, expected_attempts);
        assert_eq!(retried.rotate_last_error_redacted, None);
    }

    Ok(())
}

#[sqlx_test]
async fn corrected_target_supersedes_rejected_submission(pool: sqlx::PgPool) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let (switch_id, bmc_mac_address) = prepare_version_one_rotation(&env, &pool).await?;

    let rejected = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_expected_password_rotation_password("Different-Nvos-Password-1!");

    let error = reconcile(&env, &pool, &switch_id, rejected)
        .await
        .expect_err("rejected submission should return an error");
    assert!(
        error
            .to_string()
            .contains("NVOS submission is not retryable without correction")
    );

    let rejected = operation_state(&pool, bmc_mac_address).await?;
    assert_eq!(rejected.rotating_to_version, Some(1));
    assert_eq!(rejected.rotate_job_id, None);
    assert!(rejected.rotate_last_error_redacted.is_some());

    let corrected_password = "Recovered-Nvos-Password-2!";
    publish_target(&env, &pool, Some(1), corrected_password).await?;

    let recovered = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_expected_password_rotation_password(corrected_password)
        .with_password_rotation_job("recovered-job");

    assert!(matches!(
        reconcile(&env, &pool, &switch_id, recovered).await?,
        NvosPasswordRotationOutcome::Waiting(_)
    ));

    let recovered = operation_state(&pool, bmc_mac_address).await?;
    assert_eq!(recovered.current_version, Some(0));
    assert_eq!(recovered.rotating_to_version, Some(2));
    assert_eq!(recovered.rotate_job_id.as_deref(), Some("recovered-job"));
    assert_eq!(recovered.rotate_attempts, 2);
    assert_eq!(recovered.rotate_last_error_redacted, None);
    Ok(())
}

#[sqlx_test]
async fn staged_target_credential_survives_recovery_before_promotion(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;
    let (switch_id, bmc_mac_address) = prepare_version_one_rotation(&env, &pool).await?;

    stage_submitted_rotation(&pool, bmc_mac_address, 1, "pending-job").await?;

    let target_credentials = Credentials::UsernamePassword {
        username: "nvos-admin".to_string(),
        password: TARGET_PASSWORD.to_string(),
    };

    env.test_credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            &target_credentials,
        )
        .await
        .expect("failed to stage target NVOS credential");

    let manager = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_password_rotation_job_status(SwitchPasswordRotationState::Pending);

    assert!(matches!(
        reconcile(&env, &pool, &switch_id, manager).await?,
        NvosPasswordRotationOutcome::Waiting(_)
    ));

    let operation = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(operation.current_version, Some(0));
    assert_eq!(operation.rotating_to_version, Some(1));

    let stored = env
        .test_credential_manager
        .get_credentials_from_writer(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
        .await
        .expect("failed to read staged target NVOS credential")
        .expect("staged target credential should remain available");

    assert_eq!(stored, target_credentials);

    Ok(())
}

#[sqlx_test]
async fn controller_converges_directly_to_latest_revision(pool: sqlx::PgPool) -> TestResult {
    let env = ControllerEnv::new(pool.clone()).await;

    publish_target(&env, &pool, None, CURRENT_PASSWORD).await?;
    let (switch_id, bmc_mac_address) = create_switch(&env, &pool, None).await?;

    insert_current_version(&pool, bmc_mac_address, 0).await?;
    publish_target(&env, &pool, Some(0), "Nvos-Target-1!").await?;
    publish_target(&env, &pool, Some(1), "Nvos-Target-2!").await?;
    publish_target(&env, &pool, Some(2), "Nvos-Target-3!").await?;

    let manager = MockNvSwitchManager::default()
        .with_password_rotation_enabled()
        .with_expected_password_rotation_password("Nvos-Target-3!")
        .with_password_rotation_job("revision-3-job")
        .with_password_rotation_job_status(SwitchPasswordRotationState::Completed);

    reconcile(&env, &pool, &switch_id, manager.clone()).await?;

    let staged = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(staged.rotating_to_version, Some(3));
    assert_eq!(staged.rotate_attempts, 1);

    reconcile(&env, &pool, &switch_id, manager.clone()).await?;

    let converged = operation_state(&pool, bmc_mac_address).await?;

    assert_eq!(converged.current_version, Some(3));
    assert_eq!(converged.rotating_to_version, None);

    let stored = env
        .test_credential_manager
        .get_credentials(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
        .await
        .expect("read completed per-device NVOS credential")
        .expect("completed per-device credential");

    assert_eq!(
        stored,
        Credentials::UsernamePassword {
            username: "nvos-admin".to_string(),
            password: "Nvos-Target-3!".to_string(),
        }
    );

    env.test_credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            &Credentials::UsernamePassword {
                username: "nvos-admin".to_string(),
                password: "Stale-Nvos-Password!".to_string(),
            },
        )
        .await
        .expect("replace per-device NVOS credential with a stale password");

    let mut txn = pool.begin().await?;

    transition_switch_controller_state(txn.as_mut(), &switch_id, SwitchControllerState::Ready)
        .await?;

    txn.commit().await?;

    run_controller(&env, &pool, manager, 2).await;

    let restored = env
        .test_credential_manager
        .get_credentials(&CredentialKey::SwitchNvosAdmin { bmc_mac_address })
        .await
        .expect("read restored per-device NVOS credential")
        .expect("confirmed NVOS credential should be restored");

    assert_eq!(restored, stored);

    Ok(())
}
