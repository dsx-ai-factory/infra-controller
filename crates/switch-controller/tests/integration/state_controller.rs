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
use std::time::Duration;

use carbide_secrets::credentials::{CredentialKey, CredentialWriter, Credentials};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_switch_controller::context::SwitchStateHandlerServices;
use carbide_switch_controller::handler::SwitchStateHandler;
use carbide_switch_controller::io::SwitchStateControllerIO;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use component_manager::compute_tray_manager::Backend as ComputeBackend;
use component_manager::config::ComponentManagerConfig;
use component_manager::mock::MockNvSwitchManager;
use component_manager::nv_switch_manager::{
    Backend as NvSwitchBackend, ConfigureSwitchCertificateJobStatus,
};
use component_manager::power_shelf_manager::Backend as PowerShelfBackend;
use db::switch as db_switch;
use model::component_manager::ConfigureSwitchCertificateState;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::switch::{
    ConfigureCertificateState, ConfiguringState, SwitchControllerState, SwitchDecommissioningState,
};
use model::test_support::rms_rack_profiles;
use rpc::forge::forge_server::Forge;
use rpc::forge::{AdminForceDeleteSwitchRequest, DecommissionSwitchRequest};
use state_controller::config::IterationConfig;
use state_controller::controller::StateController;
use tokio_util::sync::CancellationToken;
use tonic::Request;

use crate::common::{
    ControllerEnv, configure_certificate_start_state, configure_certificate_wait_state,
    default_switch_mtls_services, mark_switch_as_deleted, new_switch, set_switch_rack_id,
    transition_switch_controller_state,
};

fn firmware_only_activities() -> Vec<model::rack::MaintenanceActivity> {
    vec![model::rack::MaintenanceActivity::FirmwareUpgrade {
        firmware_version: None,
        components: vec![],
        force_update: false,
    }]
}

fn nvos_and_nmxc_activities() -> Vec<model::rack::MaintenanceActivity> {
    vec![
        model::rack::MaintenanceActivity::FirmwareUpgrade {
            firmware_version: None,
            components: vec![],
            force_update: false,
        },
        model::rack::MaintenanceActivity::NvosUpdate {
            config_json: String::new(),
        },
        model::rack::MaintenanceActivity::ConfigureNmxCluster,
    ]
}

/// Empty activities means all phases, matching rack `MaintenanceScope::should_run`.
fn all_phases_activities() -> Vec<model::rack::MaintenanceActivity> {
    vec![]
}

#[sqlx_test]
async fn decommission_request_enters_rms_workflow(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    set_switch_rack_id(txn.as_mut(), &switch_id, &"rack-id-1".into()).await?;
    transition_switch_controller_state(txn.as_mut(), &switch_id, SwitchControllerState::Ready)
        .await?;
    txn.commit().await?;

    env.api
        .decommission_switch(Request::new(DecommissionSwitchRequest {
            switch_id: Some(switch_id),
        }))
        .await?;

    let mut connection = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut connection, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(switch.decommission_requested);
    drop(connection);

    env.run_switch_controller_iteration().await;
    let mut connection = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut connection, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(!switch.decommission_requested);
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Decommissioning {
            decommissioning_state: SwitchDecommissioningState::SuppressingSiteExplorer,
        }
    ));
    let bmc_mac = switch
        .bmc_mac_address
        .expect("switch fixture should have a BMC MAC");
    drop(connection);

    env.run_switch_controller_iteration().await;
    let mut connection = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut connection, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Decommissioning {
            decommissioning_state: SwitchDecommissioningState::SuppressingSiteExplorer,
        }
    ));
    let suppression = db::bmc_suppression::find(
        &mut *connection,
        bmc_mac,
        model::bmc_suppression::BmcSuppressionSubsystem::SiteExplorer,
    )
    .await?
    .expect("Site Explorer suppression should be requested");
    assert!(suppression.acknowledged_at.is_none());
    drop(connection);

    let mut txn = pool.begin().await?;
    assert!(
        db::bmc_suppression::acknowledge(
            txn.as_mut(),
            bmc_mac,
            model::bmc_suppression::BmcSuppressionSubsystem::SiteExplorer,
        )
        .await?
    );
    txn.commit().await?;

    env.run_switch_controller_iteration().await;
    let mut connection = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut connection, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Decommissioning {
            decommissioning_state: SwitchDecommissioningState::SuppressingNvosDhcp,
        }
    ));

    Ok(())
}

#[sqlx_test]
async fn force_delete_switch_clears_associated_mac_state(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    env.api
        .admin_force_delete_switch(Request::new(AdminForceDeleteSwitchRequest {
            switch_id: Some(switch_id),
            delete_interfaces: true,
            delete_bmc_suppressions: true,
        }))
        .await?;

    let mut connection = pool.acquire().await?;
    assert!(
        db_switch::find_by_id(&mut connection, &switch_id)
            .await?
            .is_none()
    );

    Ok(())
}

/// Builds a test component manager using RMS when a client is provided.
pub(super) async fn build_test_component_manager(
    env: &ControllerEnv,
    rms_client: Option<Arc<dyn librms::RmsApi>>,
) -> Option<Arc<component_manager::component_manager::ComponentManager>> {
    let config = ComponentManagerConfig {
        nv_switch_backend: if rms_client.is_some() {
            NvSwitchBackend::Rms
        } else {
            NvSwitchBackend::Mock
        },
        power_shelf_backend: PowerShelfBackend::Mock,
        compute_tray_backend: ComputeBackend::Mock,
        nv_switch_use_state_controller: true,
        ..Default::default()
    };
    let component_manager = component_manager::component_manager::build_component_manager(
        &config,
        rms_rack_profiles(),
        rms_client,
        None,
        Some(env.pool.clone()),
        None,
    )
    .await
    .expect("test component manager should build");

    Some(Arc::new(component_manager))
}

/// Runs one switch controller iteration with the provided services.
pub(super) async fn run_switch_controller_with_services(
    pool: sqlx::PgPool,
    work_lock_manager_handle: db::work_lock_manager::WorkLockManagerHandle,
    services: SwitchStateHandlerServices,
) {
    let cancel_token = CancellationToken::new();
    let mut controller = StateController::<SwitchStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: Duration::from_millis(50),
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool, work_lock_manager_handle)
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(services.into())
        .state_handler(Arc::new(SwitchStateHandler::default()))
        .build_for_manual_iterations(cancel_token)
        .unwrap();
    controller.run_single_iteration().await;
}

/// Builds a component manager around the provided NVSwitch manager.
pub(super) fn mock_component_manager(
    nv_switch: Arc<dyn component_manager::nv_switch_manager::NvSwitchManager>,
) -> Arc<component_manager::component_manager::ComponentManager> {
    Arc::new(component_manager::component_manager::ComponentManager::new(
        nv_switch,
        Arc::new(component_manager::mock::MockPowerShelfManager),
        Arc::new(component_manager::mock::MockComputeTrayManager),
        false,
        false,
        false,
    ))
}

#[sqlx_test]
async fn test_configure_certificate_start_skips_without_rack_id(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_start_state(),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::RotateOsPassword,
        }
    ));
    assert!(switch.rack_id.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_configure_certificate_start_skips_without_component_manager(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;
    let rack_id = "rack-id-1".into();

    let mut txn = pool.begin().await?;
    set_switch_rack_id(txn.as_mut(), &switch_id, &rack_id).await?;
    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_start_state(),
    )
    .await?;
    txn.commit().await?;

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        SwitchStateHandlerServices {
            db_pool: pool.clone(),
            component_manager: None,
            credential_manager: env.test_credential_manager.clone(),
            switch_mtls_services: default_switch_mtls_services(),
            per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
                Vec::new(),
                std::time::Duration::from_secs(60),
            ),
            redfish_client_pool: env.redfish_sim.clone(),
            bmc_credential_ops: env.redfish_sim.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            bmc_rotation_enabled: false,
        },
    )
    .await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::RotateOsPassword,
        }
    ));

    Ok(())
}

#[sqlx_test]
async fn test_configure_certificate_start_transitions_to_wait_for_complete_with_rack_id(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;

    let switch_id = new_switch(&env, Some("Switch4".to_string()), None).await?;

    let bmc_mac_address = db_switch::find_switch_endpoints_by_ids(&pool, &[switch_id])
        .await?
        .first()
        .expect("switch endpoint row")
        .bmc_mac;

    let credential_key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };

    let imported_credentials = Credentials::UsernamePassword {
        username: "nvos-admin".to_string(),
        password: "nvos-secret".to_string(),
    };

    env.test_credential_manager
        .set_credentials(&credential_key, &imported_credentials)
        .await
        .expect("failed to seed NVOS credentials");

    let mut txn = pool.begin().await?;
    set_switch_rack_id(txn.as_mut(), &switch_id, &"rack-id-1".into()).await?;
    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_start_state(),
    )
    .await?;
    txn.commit().await?;

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        SwitchStateHandlerServices {
            db_pool: pool.clone(),
            component_manager: Some(mock_component_manager(Arc::new(
                MockNvSwitchManager::default(),
            ))),
            credential_manager: env.test_credential_manager.clone(),
            switch_mtls_services: default_switch_mtls_services(),
            per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
                Vec::new(),
                std::time::Duration::from_secs(60),
            ),
            redfish_client_pool: env.redfish_sim.clone(),
            bmc_credential_ops: env.redfish_sim.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            bmc_rotation_enabled: false,
        },
    )
    .await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::ConfigureCertificate {
                configure_certificate: ConfigureCertificateState::WaitForComplete {
                    ref job_id
                },
            },
        } if job_id == "mock-switch-cert-job"
    ));

    assert_eq!(switch.rack_id.as_ref(), Some(&"rack-id-1".into()));

    assert_eq!(
        env.test_credential_manager
            .get_credentials_from_writer(&credential_key)
            .await
            .expect("failed to read imported NVOS credentials"),
        Some(imported_credentials)
    );

    Ok(())
}

#[sqlx_test]
async fn test_configure_certificate_start_seeds_expected_switch_credentials(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;

    let switch_id = new_switch(&env, Some("Switch4".to_string()), None).await?;

    let bmc_mac_address = db_switch::find_switch_endpoints_by_ids(&pool, &[switch_id])
        .await?
        .first()
        .expect("switch endpoint row")
        .bmc_mac;

    let credential_key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };

    assert_eq!(
        env.test_credential_manager
            .get_credentials_from_writer(&credential_key)
            .await
            .expect("failed to check for existing NVOS credentials"),
        None
    );

    let mut txn = pool.begin().await?;
    set_switch_rack_id(txn.as_mut(), &switch_id, &"rack-id-1".into()).await?;

    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_start_state(),
    )
    .await?;

    txn.commit().await?;

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        SwitchStateHandlerServices {
            db_pool: pool.clone(),
            component_manager: Some(mock_component_manager(Arc::new(
                MockNvSwitchManager::default(),
            ))),
            credential_manager: env.test_credential_manager.clone(),
            switch_mtls_services: default_switch_mtls_services(),
            per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
                Vec::new(),
                std::time::Duration::from_secs(60),
            ),
            redfish_client_pool: env.redfish_sim.clone(),
            bmc_credential_ops: env.redfish_sim.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            bmc_rotation_enabled: false,
        },
    )
    .await;

    let mut txn = pool.acquire().await?;

    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");

    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::ConfigureCertificate {
                configure_certificate: ConfigureCertificateState::WaitForComplete { .. },
            },
        }
    ));

    assert_eq!(
        env.test_credential_manager
            .get_credentials_from_writer(&credential_key)
            .await
            .expect("failed to read seeded NVOS credentials"),
        Some(Credentials::UsernamePassword {
            username: "nvos_admin1".to_string(),
            password: "nvos_pass1".to_string(),
        })
    );

    Ok(())
}

#[sqlx_test]
async fn test_configure_certificate_start_retries_after_credential_import(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let bmc_mac_address = db_switch::find_switch_endpoints_by_ids(&pool, &[switch_id])
        .await?
        .first()
        .expect("switch endpoint row")
        .bmc_mac;

    let credential_key = CredentialKey::SwitchNvosAdmin { bmc_mac_address };

    let mut txn = pool.begin().await?;
    set_switch_rack_id(txn.as_mut(), &switch_id, &"rack-id-1".into()).await?;

    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_start_state(),
    )
    .await?;

    txn.commit().await?;

    let services = || SwitchStateHandlerServices {
        db_pool: pool.clone(),
        component_manager: Some(mock_component_manager(Arc::new(
            MockNvSwitchManager::default(),
        ))),
        credential_manager: env.test_credential_manager.clone(),
        switch_mtls_services: default_switch_mtls_services(),
        per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
            Vec::new(),
            std::time::Duration::from_secs(60),
        ),
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
            db::credential_rotation::CredentialRotationType::Bmc,
        ),
        bmc_rotation_enabled: false,
    };

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        services(),
    )
    .await;

    let mut txn = pool.acquire().await?;

    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");

    assert_eq!(
        switch.controller_state.value,
        configure_certificate_start_state()
    );

    assert!(matches!(
        switch.controller_state_outcome.as_ref(),
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == &format!("switch {switch_id}: waiting for NVOS admin credentials")
    ));

    drop(txn);

    env.test_credential_manager
        .set_credentials(
            &credential_key,
            &Credentials::UsernamePassword {
                username: "imported-admin".to_string(),
                password: "imported-secret".to_string(),
            },
        )
        .await
        .expect("failed to import NVOS credentials");

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        services(),
    )
    .await;

    let mut txn = pool.acquire().await?;

    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");

    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::ConfigureCertificate {
                configure_certificate: ConfigureCertificateState::WaitForComplete { .. },
            },
        }
    ));

    Ok(())
}

#[sqlx_test]
async fn test_configure_certificate_wait_for_complete_transitions_to_rotate_os_password(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_wait_state("mock-switch-cert-job"),
    )
    .await?;
    txn.commit().await?;

    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        SwitchStateHandlerServices {
            db_pool: pool.clone(),
            component_manager: Some(mock_component_manager(Arc::new(
                MockNvSwitchManager::default(),
            ))),
            credential_manager: env.test_credential_manager.clone(),
            switch_mtls_services: default_switch_mtls_services(),
            per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
                Vec::new(),
                std::time::Duration::from_secs(60),
            ),
            redfish_client_pool: env.redfish_sim.clone(),
            bmc_credential_ops: env.redfish_sim.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            bmc_rotation_enabled: false,
        },
    )
    .await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Configuring {
            config_state: ConfiguringState::RotateOsPassword,
        }
    ));

    Ok(())
}

#[sqlx_test]
async fn test_configure_certificate_wait_for_complete_transitions_to_error_on_failure(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    transition_switch_controller_state(
        txn.as_mut(),
        &switch_id,
        configure_certificate_wait_state("mock-switch-cert-job"),
    )
    .await?;
    txn.commit().await?;

    let failing_mock = MockNvSwitchManager::default().with_certificate_job_status(
        ConfigureSwitchCertificateJobStatus {
            state: ConfigureSwitchCertificateState::Failed,
            error: Some("cert install failed".to_string()),
        },
    );
    run_switch_controller_with_services(
        pool.clone(),
        env.api.work_lock_manager_handle(),
        SwitchStateHandlerServices {
            db_pool: pool.clone(),
            component_manager: Some(mock_component_manager(Arc::new(failing_mock))),
            credential_manager: env.test_credential_manager.clone(),
            switch_mtls_services: default_switch_mtls_services(),
            per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
                Vec::new(),
                std::time::Duration::from_secs(60),
            ),
            redfish_client_pool: env.redfish_sim.clone(),
            bmc_credential_ops: env.redfish_sim.clone(),
            bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
                db::credential_rotation::CredentialRotationType::Bmc,
            ),
            bmc_rotation_enabled: false,
        },
    )
    .await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Error { ref cause } if cause == "cert install failed"
    ));

    Ok(())
}

#[sqlx_test]
async fn test_rotate_os_password_transitions_to_fetch_info(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

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

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::FetchInfo
    ));

    Ok(())
}

#[sqlx_test]
async fn test_switch_deletion_with_state_controller(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;

    // Create a switch
    let switch_id = new_switch(
        &env,
        Some("Switch1".to_string()),
        Some("Data Center A, Rack 1".to_string()),
    )
    .await?;

    // Start the state controller
    let switch_handler = Arc::new(SwitchStateHandler::default());
    const ITERATION_TIME: Duration = Duration::from_millis(50);

    let handler_services = Arc::new(SwitchStateHandlerServices {
        db_pool: pool.clone(),
        component_manager: None,
        credential_manager: Arc::new(TestCredentialManager::default()),
        switch_mtls_services: default_switch_mtls_services(),
        per_object_metrics_registry: carbide_health_metrics::PerObjectMetricsRegistry::new(
            Vec::new(),
            std::time::Duration::from_secs(60),
        ),
        redfish_client_pool: env.redfish_sim.clone(),
        bmc_credential_ops: env.redfish_sim.clone(),
        bmc_rotation_gate: carbide_credential_rotation::RotationGate::new_for_family(
            db::credential_rotation::CredentialRotationType::Bmc,
        ),
        bmc_rotation_enabled: false,
    });

    let cancel_token = CancellationToken::new();
    let mut controller = StateController::<SwitchStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: ITERATION_TIME,
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool.clone(), env.api.work_lock_manager_handle())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(handler_services.clone())
        .state_handler(switch_handler.clone())
        .build_for_manual_iterations(cancel_token.clone())
        .unwrap();

    // Walk through state machine
    for _ in 0..20 {
        controller.run_single_iteration().await;
    }

    let switch = env
        .api
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id],
        }))
        .await?
        .into_inner()
        .switches
        .remove(0);
    assert_eq!(switch.controller_state, r#"{"state":"ready"}"#.to_string());

    // Mark the switch as deleted
    mark_switch_as_deleted(pool.acquire().await?.as_mut(), &switch_id).await?;

    // Walk through state machine
    for _ in 0..20 {
        controller.run_single_iteration().await;
    }

    // Verify that the DB object is gone
    let switches = env
        .api
        .find_switches_by_ids(tonic::Request::new(rpc::forge::SwitchesByIdsRequest {
            switch_ids: vec![switch_id],
        }))
        .await?
        .into_inner()
        .switches;
    assert!(switches.is_empty());

    Ok(())
}

/// Tests the entire Switch ControllerState transition flow: Initializing -> Configuring
/// (ConfigureCertificate) -> Configuring (RotateOsPassword) -> FetchInfo
/// -> Validating (ValidationComplete) -> BomValidating (BomValidationComplete) -> Ready.
/// state handler performs its transition.
#[sqlx_test]
async fn test_switch_entire_state_transition_flow(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;

    let switch_id = new_switch(
        &env,
        Some("Switch3".to_string()),
        Some("Data Center A, Rack 1".to_string()),
    )
    .await?;

    // Verify initial state is Initializing
    {
        let mut txn = pool.acquire().await?;
        let switch = db_switch::find_by_id(&mut txn, &switch_id).await?;
        let switch = switch.expect("switch should exist");
        assert!(
            matches!(
                switch.controller_state.value,
                SwitchControllerState::Created
            ),
            "initial state should be Created, got {:?}",
            switch.controller_state.value
        );
    }

    // Start the state controller with the real handler
    let switch_handler = Arc::new(SwitchStateHandler::default());
    const ITERATION_TIME: Duration = Duration::from_millis(50);

    let cancel_token = CancellationToken::new();
    let mut controller = StateController::<SwitchStateControllerIO>::builder()
        .iteration_config(IterationConfig {
            iteration_time: ITERATION_TIME,
            processor_dispatch_interval: Duration::from_millis(10),
            ..Default::default()
        })
        .database(pool.clone(), env.api.work_lock_manager_handle())
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(
            SwitchStateHandlerServices {
                db_pool: pool.clone(),
                component_manager: build_test_component_manager(&env, env.rms_sim.as_rms_client())
                    .await,
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
            .into(),
        )
        .state_handler(switch_handler.clone())
        .build_for_manual_iterations(cancel_token.clone())
        .unwrap();

    // iterate a few times
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;

    // Final assertion: state is Ready
    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id).await?;
    let switch = switch.expect("switch should exist");
    assert!(
        matches!(switch.controller_state.value, SwitchControllerState::Ready),
        "expected Ready, got {:?}",
        switch.controller_state.value
    );

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_rack_firmware_upgrade_waits_for_terminal_status(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        },
    )
    .await?;
    db_switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_id,
        Some(&model::rack::RackFirmwareUpgradeStatus {
            task_id: "rack-job".to_string(),
            status: model::rack::RackFirmwareUpgradeState::InProgress,
            started_at: Some(requested_at),
            ended_at: None,
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        }
    ));
    assert!(switch.switch_reprovisioning_requested.is_some());

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_rack_firmware_upgrade_transitions_to_waiting_for_nvos_on_completion(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        },
    )
    .await?;
    db_switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_id,
        Some(&model::rack::RackFirmwareUpgradeStatus {
            task_id: "rack-job".to_string(),
            status: model::rack::RackFirmwareUpgradeState::Completed,
            started_at: Some(requested_at),
            ended_at: Some(chrono::Utc::now()),
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNVOSUpgrade,
        }
    ));
    assert!(switch.switch_reprovisioning_requested.is_some());

    Ok(())
}

/// Empty activities must keep the same all-phases meaning as `should_run`, so
/// firmware completion advances to WaitingForNVOSUpgrade rather than skipping
/// NVOS for ConfigureNmxCluster.
#[sqlx_test]
async fn test_switch_waiting_for_rack_firmware_upgrade_next_state_by_activities(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_test_support::Check;
    use model::switch::ReProvisioningState;

    #[derive(Clone)]
    struct CaseInput {
        activities: Vec<model::rack::MaintenanceActivity>,
        /// Distinct expected-switch fixture name so each case gets a unique PK.
        switch_name: &'static str,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Expect {
        ReProvisioning(ReProvisioningState),
        Ready,
    }

    let env = ControllerEnv::new(pool.clone()).await;
    let cases = [
        Check {
            scenario: "empty activities advance to WaitingForNVOSUpgrade",
            input: CaseInput {
                activities: all_phases_activities(),
                switch_name: "Switch1",
            },
            expect: Expect::ReProvisioning(ReProvisioningState::WaitingForNVOSUpgrade),
        },
        Check {
            scenario: "explicit NVOS+NMXC advance to WaitingForNVOSUpgrade",
            input: CaseInput {
                activities: nvos_and_nmxc_activities(),
                switch_name: "Switch2",
            },
            expect: Expect::ReProvisioning(ReProvisioningState::WaitingForNVOSUpgrade),
        },
        Check {
            scenario: "firmware-only returns Ready",
            input: CaseInput {
                activities: firmware_only_activities(),
                switch_name: "Switch3",
            },
            expect: Expect::Ready,
        },
    ];

    for case in cases {
        let switch_id = new_switch(&env, Some(case.input.switch_name.to_string()), None).await?;

        let mut txn = pool.begin().await?;
        db_switch::set_switch_reprovisioning_requested(
            txn.as_mut(),
            switch_id,
            "rack-test",
            case.input.activities.clone(),
        )
        .await?;
        let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
            .await?
            .expect("switch should exist");
        let requested_at = switch
            .switch_reprovisioning_requested
            .as_ref()
            .expect("switch reprovision request should exist")
            .requested_at;
        db_switch::try_update_controller_state(
            txn.as_mut(),
            switch_id,
            switch.controller_state.version,
            switch.controller_state.version.increment(),
            &SwitchControllerState::ReProvisioning {
                reprovisioning_state: ReProvisioningState::WaitingForRackFirmwareUpgrade,
            },
        )
        .await?;
        db_switch::update_firmware_upgrade_status(
            txn.as_mut(),
            switch_id,
            Some(&model::rack::RackFirmwareUpgradeStatus {
                task_id: "rack-job".to_string(),
                status: model::rack::RackFirmwareUpgradeState::Completed,
                started_at: Some(requested_at),
                ended_at: Some(chrono::Utc::now()),
            }),
        )
        .await?;
        txn.commit().await?;

        env.run_switch_controller_iteration().await;

        let mut txn = pool.acquire().await?;
        let switch = db_switch::find_by_id(&mut txn, &switch_id)
            .await?
            .expect("switch should exist");
        let got = match &switch.controller_state.value {
            SwitchControllerState::Ready => Expect::Ready,
            SwitchControllerState::ReProvisioning {
                reprovisioning_state,
            } => Expect::ReProvisioning(reprovisioning_state.clone()),
            other => panic!("{}: unexpected controller state {:?}", case.scenario, other),
        };
        assert_eq!(got, case.expect, "{}", case.scenario);
        match case.expect {
            Expect::Ready => assert!(
                switch.switch_reprovisioning_requested.is_none(),
                "{}: request should be cleared",
                case.scenario
            ),
            Expect::ReProvisioning(_) => assert!(
                switch.switch_reprovisioning_requested.is_some(),
                "{}: request should remain",
                case.scenario
            ),
        }
    }

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_rack_firmware_upgrade_returns_ready_for_firmware_only_request(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-test",
        firmware_only_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        },
    )
    .await?;
    db_switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_id,
        Some(&model::rack::RackFirmwareUpgradeStatus {
            task_id: "rack-job".to_string(),
            status: model::rack::RackFirmwareUpgradeState::Completed,
            started_at: Some(requested_at),
            ended_at: Some(chrono::Utc::now()),
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Ready,
    ));
    assert!(switch.switch_reprovisioning_requested.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_rack_firmware_upgrade_accepts_completion_when_only_ended_at_is_current(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        },
    )
    .await?;
    db_switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_id,
        Some(&model::rack::RackFirmwareUpgradeStatus {
            task_id: "rack-job".to_string(),
            status: model::rack::RackFirmwareUpgradeState::Completed,
            started_at: Some(requested_at - chrono::Duration::seconds(1)),
            ended_at: Some(requested_at + chrono::Duration::seconds(1)),
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNVOSUpgrade,
        }
    ));
    assert!(switch.switch_reprovisioning_requested.is_some());

    Ok(())
}

#[sqlx_test]
async fn test_switch_ready_routes_rack_requests_to_waiting_for_rack_firmware_upgrade(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::Ready,
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForRackFirmwareUpgrade,
        }
    ));

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_nvos_upgrade_transitions_to_waiting_for_nmxc_on_completion(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-nvos-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNVOSUpgrade,
        },
    )
    .await?;
    db_switch::update_nvos_update_status(
        txn.as_mut(),
        switch_id,
        Some(&model::switch::SwitchNvosUpdateStatus {
            task_id: "nvos-job".to_string(),
            firmware_id: "fw-1".to_string(),
            image_filename: "nvos-image.bin".to_string(),
            status: model::switch::SwitchNvosUpdateState::Completed,
            started_at: Some(requested_at),
            ended_at: Some(chrono::Utc::now()),
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNMXCConfigure,
        }
    ));
    assert!(switch.switch_reprovisioning_requested.is_some());

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_nvos_upgrade_waits_for_current_cycle_status(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-nvos-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNVOSUpgrade,
        },
    )
    .await?;
    db_switch::update_nvos_update_status(
        txn.as_mut(),
        switch_id,
        Some(&model::switch::SwitchNvosUpdateStatus {
            task_id: "old-nvos-job".to_string(),
            firmware_id: "old-fw".to_string(),
            image_filename: "old-nvos-image.bin".to_string(),
            status: model::switch::SwitchNvosUpdateState::Completed,
            started_at: Some(requested_at - chrono::Duration::seconds(10)),
            ended_at: Some(requested_at - chrono::Duration::seconds(1)),
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNVOSUpgrade,
        }
    ));
    assert!(switch.switch_reprovisioning_requested.is_some());

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_nvos_upgrade_transitions_to_error_on_failure(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-nvos-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    let requested_at = switch
        .switch_reprovisioning_requested
        .as_ref()
        .expect("switch reprovision request should exist")
        .requested_at;
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNVOSUpgrade,
        },
    )
    .await?;
    db_switch::update_nvos_update_status(
        txn.as_mut(),
        switch_id,
        Some(&model::switch::SwitchNvosUpdateStatus {
            task_id: "nvos-job".to_string(),
            firmware_id: "fw-1".to_string(),
            image_filename: "nvos-image.bin".to_string(),
            status: model::switch::SwitchNvosUpdateState::Failed {
                cause: "image install failed".to_string(),
            },
            started_at: Some(requested_at),
            ended_at: Some(chrono::Utc::now()),
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Error { ref cause } if cause == "image install failed"
    ));
    assert!(switch.switch_reprovisioning_requested.is_none());

    Ok(())
}

#[sqlx_test]
async fn test_switch_waiting_for_nmxc_configure_returns_ready_when_fm_is_running(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    let mut txn = pool.begin().await?;
    db_switch::set_switch_reprovisioning_requested(
        txn.as_mut(),
        switch_id,
        "rack-nmxc-test",
        nvos_and_nmxc_activities(),
    )
    .await?;
    let switch = db_switch::find_by_id(txn.as_mut(), &switch_id)
        .await?
        .expect("switch should exist");
    db_switch::try_update_controller_state(
        txn.as_mut(),
        switch_id,
        switch.controller_state.version,
        switch.controller_state.version.increment(),
        &SwitchControllerState::ReProvisioning {
            reprovisioning_state: model::switch::ReProvisioningState::WaitingForNMXCConfigure,
        },
    )
    .await?;
    db_switch::update_fabric_manager_status(
        txn.as_mut(),
        switch_id,
        Some(&model::switch::FabricManagerStatus {
            fabric_manager_state: model::switch::FabricManagerState::Ok,
            addition_info: Some("CONTROL_PLANE_STATE_CONFIGURED".to_string()),
            reason: None,
            error_message: None,
        }),
    )
    .await?;
    txn.commit().await?;

    env.run_switch_controller_iteration().await;

    let mut txn = pool.acquire().await?;
    let switch = db_switch::find_by_id(&mut txn, &switch_id)
        .await?
        .expect("switch should exist");
    assert!(matches!(
        switch.controller_state.value,
        SwitchControllerState::Ready
    ));
    assert!(switch.switch_reprovisioning_requested.is_none());

    Ok(())
}
