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

//! Tests for DPUNode stale-label detection.
//!
//! When `verify_node_labels` returns `false` (e.g. a v1-labeled node is
//! being processed by v2 code), the handler must transition to `Failed`
//! with a `DpfProvisioning` cause. When labels are current, normal
//! provisioning proceeds.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use carbide_dpf::{DpuDeploymentType, DpuPhase};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use model::machine::{DpfState, DpuInitState, FailureCause, FailureDetails, ManagedHostState};
use tokio::time::timeout;

use super::{dpf_config, get_host_state};
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host_with_dpf, create_test_env_with_overrides, get_config,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

fn provisioning_mock_with_labels_valid(labels_valid: Arc<AtomicBool>) -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels()
        .returning(move |_, _| Ok(labels_valid.load(Ordering::SeqCst)));
    mock
}

async fn reset_host_to_provisioning(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_id: &DpuMachineId,
) {
    let state = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: HashMap::from([(
                *dpu_id,
                DpuInitState::DpfStates {
                    state: DpfState::Provisioning,
                },
            )]),
        },
    };
    let state_json = serde_json::to_value(&state).unwrap();
    let version = format!("V999-T{}", chrono::Utc::now().timestamp_micros());

    sqlx::query(
        "UPDATE machines SET \
            controller_state = $1, \
            controller_state_version = $2, \
            controller_state_outcome = NULL, \
            health_reports = '{\"merges\": {}, \"replace\": null}'::jsonb \
         WHERE id = $3",
    )
    .bind(sqlx::types::Json(&state_json))
    .bind(&version)
    .bind(host_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn reset_host_to_waiting_for_ready(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_id: &DpuMachineId,
) {
    let state = ManagedHostState::DPUInit {
        dpu_states: model::machine::DpuInitStates {
            states: HashMap::from([(
                *dpu_id,
                DpuInitState::DpfStates {
                    state: DpfState::WaitingForReady { phase_detail: None },
                },
            )]),
        },
    };
    let state_json = serde_json::to_value(&state).unwrap();
    let version = format!("V999-T{}", chrono::Utc::now().timestamp_micros());

    sqlx::query(
        "UPDATE machines SET \
            controller_state = $1, \
            controller_state_version = $2, \
            controller_state_outcome = NULL, \
            health_reports = '{\"merges\": {}, \"replace\": null}'::jsonb \
         WHERE id = $3",
    )
    .bind(sqlx::types::Json(&state_json))
    .bind(&version)
    .bind(host_id)
    .execute(pool)
    .await
    .unwrap();
}

/// A node with stale labels during Provisioning transitions to Failed
/// with a DpfProvisioning cause.
#[crate::sqlx_test]
async fn test_stale_labels_during_provisioning_fails(pool: sqlx::PgPool) {
    let labels_valid = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> =
        Arc::new(provisioning_mock_with_labels_valid(labels_valid.clone()));

    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    labels_valid.store(false, Ordering::SeqCst);
    reset_host_to_provisioning(&pool, &mh.id, &mh.dpu_ids[0]).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let host_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            host_state,
            ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::DpfProvisioning { .. },
                    ..
                },
                ..
            }
        ),
        "Stale labels during Provisioning should transition to Failed/DpfProvisioning, got: {host_state:?}"
    );
}

/// A node with stale labels during WaitingForReady transitions to Failed.
#[crate::sqlx_test]
async fn test_stale_labels_during_waiting_for_ready_fails(pool: sqlx::PgPool) {
    let labels_valid = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> =
        Arc::new(provisioning_mock_with_labels_valid(labels_valid.clone()));

    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    labels_valid.store(false, Ordering::SeqCst);
    reset_host_to_waiting_for_ready(&pool, &mh.id, &mh.dpu_ids[0]).await;

    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during state controller iteration");

    let host_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            host_state,
            ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::DpfProvisioning { .. },
                    ..
                },
                ..
            }
        ),
        "Stale labels during WaitingForReady should transition to Failed/DpfProvisioning, got: {host_state:?}"
    );
}

/// When labels are valid, provisioning proceeds normally past Provisioning.
#[crate::sqlx_test]
async fn test_current_labels_allow_provisioning_to_proceed(pool: sqlx::PgPool) {
    let labels_valid = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> =
        Arc::new(provisioning_mock_with_labels_valid(labels_valid));

    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    reset_host_to_provisioning(&pool, &mh.id, &mh.dpu_ids[0]).await;

    for _ in 0..5 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during state controller iteration");
    }

    let host_state = get_host_state(&env, &mh).await;
    assert!(
        !matches!(host_state, ManagedHostState::Failed { .. }),
        "Valid labels should not cause failure, got: {host_state:?}"
    );
}

/// Labels become stale mid-flow: initially valid for provisioning, then
/// invalidated before WaitingForReady is processed.
#[crate::sqlx_test]
async fn test_labels_become_stale_mid_provisioning(pool: sqlx::PgPool) {
    let labels_valid = Arc::new(AtomicBool::new(true));
    let dpf_sdk: Arc<dyn DpfOperations> =
        Arc::new(provisioning_mock_with_labels_valid(labels_valid.clone()));

    let mut config = get_config();
    config.dpf = dpf_config();

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    reset_host_to_provisioning(&pool, &mh.id, &mh.dpu_ids[0]).await;

    // First iteration: labels are valid, so provisioning advances.
    timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
        .await
        .expect("timed out during first iteration");

    let host_state = get_host_state(&env, &mh).await;
    assert!(
        !matches!(host_state, ManagedHostState::Failed { .. }),
        "First iteration with valid labels should not fail, got: {host_state:?}"
    );

    // Invalidate labels before the next check.
    labels_valid.store(false, Ordering::SeqCst);

    for _ in 0..3 {
        timeout(TEST_TIMEOUT, env.run_machine_state_controller_iteration())
            .await
            .expect("timed out during iteration");
    }

    let host_state = get_host_state(&env, &mh).await;
    assert!(
        matches!(
            host_state,
            ManagedHostState::Failed {
                details: FailureDetails {
                    cause: FailureCause::DpfProvisioning { .. },
                    ..
                },
                ..
            }
        ),
        "After labels become stale, host should fail with DpfProvisioning, got: {host_state:?}"
    );
}
