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

//! Switch maintenance admission, completion, and request replacement.

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_switch_controller::context::{
    SwitchStateHandlerContextObjects, SwitchStateHandlerServices,
};
use carbide_switch_controller::handler::SwitchStateHandler;
use carbide_switch_controller::metrics::SwitchMetrics;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use carbide_uuid::rack::RackProfileId;
use carbide_uuid::switch::SwitchId;
use component_manager::mock::MockNvSwitchManager;
use component_manager::nv_switch_manager::ConfigureSwitchCertificateJobStatus;
use db::switch as db_switch;
use librms::protos::rack_manager as rms;
use model::component_manager::ConfigureSwitchCertificateState;
use model::switch::{
    ConfigureCertificateState, Switch, SwitchControllerState, SwitchMaintenanceOperation,
};
use model::test_support::TEST_RMS_RACK_PROFILE_ID;
use rpc::common::SystemPowerControl;
use rpc::forge::component_power_control_request::Target;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ComponentPowerControlRequest, SwitchIdList, SwitchesByIdsRequest};
use state_controller::db_write_batch::DbWriteBatch;
use state_controller::state_handler::{StateHandler, StateHandlerContext, StateHandlerOutcome};
use tonic::Request;

use crate::common::{
    ControllerEnv, default_switch_mtls_services, new_switch, set_switch_controller_state,
    set_switch_rack_id,
};
use crate::state_controller::{
    build_test_component_manager, mock_component_manager, run_switch_controller_with_services,
};

fn cm_power_action(operation: SwitchMaintenanceOperation) -> SystemPowerControl {
    match operation {
        SwitchMaintenanceOperation::PowerOn => SystemPowerControl::On,
        SwitchMaintenanceOperation::PowerOff => SystemPowerControl::ForceOff,
        SwitchMaintenanceOperation::Reset => SystemPowerControl::ForceRestart,
        SwitchMaintenanceOperation::ReconfigureCertificate => SystemPowerControl::On,
    }
}

async fn request_switch_maintenance_via_cm(
    env: &ControllerEnv,
    switch_id: &SwitchId,
    operation: SwitchMaintenanceOperation,
) {
    let response = env
        .api
        .component_power_control(Request::new(ComponentPowerControlRequest {
            target: Some(Target::SwitchIds(SwitchIdList {
                ids: vec![*switch_id],
            })),
            action: cm_power_action(operation) as i32,
            bypass_state_controller: false,
        }))
        .await
        .expect("component_power_control should succeed")
        .into_inner();
    assert_eq!(response.results.len(), 1);
    assert_eq!(
        response.results[0].status(),
        rpc::forge::ComponentManagerStatusCode::Success,
        "{:?}",
        response.results
    );
}

async fn load_switch(env: &ControllerEnv, id: &SwitchId) -> Switch {
    let switches = env
        .api
        .find_switches_by_ids(Request::new(SwitchesByIdsRequest {
            switch_ids: vec![*id],
        }))
        .await
        .expect("find_switches_by_ids should succeed")
        .into_inner()
        .switches;
    assert_eq!(switches.len(), 1, "switch should exist");

    // The state handler operates on the full model, including fields such as
    // `switch_maintenance_requested` that are not exposed on the RPC Switch.
    let mut conn = env.pool.acquire().await.unwrap();
    db_switch::find_by_id(conn.as_mut(), id)
        .await
        .unwrap()
        .expect("switch should exist")
}

async fn run_handler(
    services: &mut SwitchStateHandlerServices,
    state: &mut Switch,
) -> StateHandlerOutcome<SwitchControllerState> {
    let handler = SwitchStateHandler::default();
    let mut metrics = SwitchMetrics::default();
    let mut writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<SwitchStateHandlerContextObjects> {
        services,
        metrics: &mut metrics,
        pending_db_writes: &mut writes,
    };
    let controller_state = state.controller_state.value.clone();
    let switch_id = state.id;
    handler
        .handle_object_state(&switch_id, state, &controller_state, &mut ctx)
        .await
        .expect("state handler should not return an error result")
}

async fn commit_and_extract_transition(
    mut outcome: StateHandlerOutcome<SwitchControllerState>,
) -> Option<SwitchControllerState> {
    if let Some(txn) = outcome.take_transaction() {
        txn.commit().await.unwrap();
    }
    match outcome {
        StateHandlerOutcome::Transition { next_state, .. } => Some(next_state),
        _ => None,
    }
}

fn services_without_component_manager(env: &ControllerEnv) -> SwitchStateHandlerServices {
    SwitchStateHandlerServices {
        db_pool: env.pool.clone(),
        component_manager: None,
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

async fn services_with_component_manager(env: &ControllerEnv) -> SwitchStateHandlerServices {
    SwitchStateHandlerServices {
        db_pool: env.pool.clone(),
        component_manager: build_test_component_manager(env, env.rms_sim.as_rms_client()).await,
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

#[sqlx_test]
async fn ready_transitions_to_maintenance_when_request_is_set(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    request_switch_maintenance_via_cm(&env, &switch_id, SwitchMaintenanceOperation::PowerOff).await;

    {
        let mut txn = pool.acquire().await?;
        set_switch_controller_state(&mut txn, &switch_id, SwitchControllerState::Ready).await?;
    }

    let mut switch = load_switch(&env, &switch_id).await;
    let mut services = services_without_component_manager(&env);
    let outcome = run_handler(&mut services, &mut switch).await;

    assert!(matches!(
        outcome,
        StateHandlerOutcome::Transition {
            next_state: SwitchControllerState::Maintenance {
                operation: SwitchMaintenanceOperation::PowerOff,
                configure_certificate: None,
                ..
            },
            ..
        }
    ));

    Ok(())
}

/// `Ready` must not dispatch maintenance power operations while idling.
#[sqlx_test]
async fn ready_state_does_not_invoke_power_control(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await?;

    {
        let mut txn = pool.acquire().await?;
        set_switch_controller_state(&mut txn, &switch_id, SwitchControllerState::Ready).await?;
    }

    let mut services = services_with_component_manager(&env).await;
    let mut switch = load_switch(&env, &switch_id).await;
    let outcome = run_handler(&mut services, &mut switch).await;
    let _ = commit_and_extract_transition(outcome).await;

    let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
    assert!(
        calls.is_empty(),
        "Ready state must not call batch_set_power_state"
    );

    Ok(())
}

#[sqlx_test]
async fn preserves_replacement_maintenance_after_power_completion(pool: sqlx::PgPool) {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, None, None).await.unwrap();
    let rack = env
        .harness
        .create_rack(RackProfileId::new(TEST_RMS_RACK_PROFILE_ID))
        .await;
    let bmc_mac_address = load_switch(&env, &switch_id).await.bmc_mac_address.unwrap();
    for key in [
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        },
        CredentialKey::SwitchNvosAdmin { bmc_mac_address },
    ] {
        env.test_credential_manager
            .set_credentials(
                &key,
                &Credentials::UsernamePassword {
                    username: "admin".into(),
                    password: "password".into(),
                },
            )
            .await
            .unwrap();
    }
    let mut txn = pool.begin().await.unwrap();
    set_switch_rack_id(&mut txn, &switch_id, &rack.id)
        .await
        .unwrap();
    set_switch_controller_state(&mut txn, &switch_id, SwitchControllerState::Ready)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    for succeeds in [true, false] {
        request_switch_maintenance_via_cm(&env, &switch_id, SwitchMaintenanceOperation::PowerOn)
            .await;
        env.run_switch_controller_iteration().await;
        let original = load_switch(&env, &switch_id)
            .await
            .switch_maintenance_requested
            .unwrap();
        assert_eq!(
            load_switch(&env, &switch_id).await.controller_state.value,
            SwitchControllerState::maintenance_for_request(original.clone())
        );
        env.rms_sim
            .queue_batch_set_power_state_response(Ok(rms::BatchSetPowerStateResponse {
                response: Some(rms::NodeBatchResponse {
                    status: if succeeds {
                        rms::ReturnCode::Success
                    } else {
                        rms::ReturnCode::Failure
                    } as i32,
                    message: "injected power response".into(),
                    ..Default::default()
                }),
            }))
            .await;
        let (arrival, release) = env.rms_sim.block_next_batch_set_power_state().await;
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(env.run_switch_controller_iteration(), async {
                arrival.await.unwrap();
                request_switch_maintenance_via_cm(
                    &env,
                    &switch_id,
                    SwitchMaintenanceOperation::PowerOff,
                )
                .await;
                release.send(()).unwrap();
            });
        })
        .await
        .expect("blocked power call and replacement request should finish");

        let completed = load_switch(&env, &switch_id).await;
        assert!(matches!(
            (succeeds, completed.controller_state.value),
            (true, SwitchControllerState::Ready) | (false, SwitchControllerState::Error { .. })
        ));
        let replacement = completed.switch_maintenance_requested.unwrap();
        assert_ne!(original, replacement);
        assert_eq!(replacement.operation, SwitchMaintenanceOperation::PowerOff);

        env.run_switch_controller_iteration().await;
        assert_eq!(
            load_switch(&env, &switch_id).await.controller_state.value,
            SwitchControllerState::maintenance_for_request(replacement)
        );
        env.rms_sim
            .queue_batch_set_power_state_response(Ok(rms::BatchSetPowerStateResponse {
                response: Some(rms::NodeBatchResponse {
                    status: rms::ReturnCode::Success as i32,
                    ..Default::default()
                }),
            }))
            .await;
        env.run_switch_controller_iteration().await;
        let completed = load_switch(&env, &switch_id).await;
        assert_eq!(
            completed.controller_state.value,
            SwitchControllerState::Ready
        );
        assert!(completed.switch_maintenance_requested.is_none());
        let calls = env.rms_sim.submitted_batch_set_power_state_requests().await;
        assert_eq!(
            calls.last().unwrap().operation,
            rms::PowerOperation::Off as i32
        );
    }
}

#[sqlx_test]
async fn certificate_completion_preserves_replacement_after_controller_reload(pool: sqlx::PgPool) {
    let env = ControllerEnv::new(pool.clone()).await;
    let switch_id = new_switch(&env, Some("Switch4".into()), None)
        .await
        .unwrap();
    let bmc_mac_address = load_switch(&env, &switch_id).await.bmc_mac_address.unwrap();
    for key in [
        CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
        },
        CredentialKey::SwitchNvosAdmin { bmc_mac_address },
    ] {
        env.test_credential_manager
            .set_credentials(
                &key,
                &Credentials::UsernamePassword {
                    username: "admin".into(),
                    password: "password".into(),
                },
            )
            .await
            .unwrap();
    }
    let services = |job_state| {
        let mut services = services_without_component_manager(&env);
        services.component_manager = Some(mock_component_manager(std::sync::Arc::new(
            MockNvSwitchManager::default().with_certificate_job_status(
                ConfigureSwitchCertificateJobStatus {
                    state: job_state,
                    error: Some("injected certificate failure".into()),
                },
            ),
        )));
        services
    };

    for (job_state, legacy) in [
        (ConfigureSwitchCertificateState::Completed, false),
        (ConfigureSwitchCertificateState::Failed, false),
        (ConfigureSwitchCertificateState::Completed, true),
    ] {
        let mut txn = pool.begin().await.unwrap();
        set_switch_rack_id(&mut txn, &switch_id, &"rack-id-1".into())
            .await
            .unwrap();
        set_switch_controller_state(&mut txn, &switch_id, SwitchControllerState::Ready)
            .await
            .unwrap();
        db_switch::set_switch_maintenance_requested(
            &mut txn,
            switch_id,
            "certificate-test",
            SwitchMaintenanceOperation::ReconfigureCertificate,
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        for _ in 0..2 {
            run_switch_controller_with_services(
                pool.clone(),
                env.api.work_lock_manager_handle(),
                services(job_state),
            )
            .await;
        }
        let started = load_switch(&env, &switch_id).await;
        let original = started.switch_maintenance_requested.unwrap();
        assert_eq!(
            started.controller_state.value,
            SwitchControllerState::Maintenance {
                operation: SwitchMaintenanceOperation::ReconfigureCertificate,
                request: Some(original),
                configure_certificate: Some(ConfigureCertificateState::WaitForComplete {
                    job_id: "mock-switch-cert-job".into()
                }),
            }
        );
        if legacy {
            // This is the persisted representation before requests were saved
            // with certificate jobs. The current pending request cannot fill it in.
            sqlx::query(
                "UPDATE switches SET controller_state = controller_state - 'request' WHERE id = $1",
            )
            .bind(switch_id)
            .execute(&pool)
            .await
            .unwrap();
        }
        request_switch_maintenance_via_cm(&env, &switch_id, SwitchMaintenanceOperation::PowerOff)
            .await;
        let replacement = load_switch(&env, &switch_id)
            .await
            .switch_maintenance_requested
            .unwrap();
        run_switch_controller_with_services(
            pool.clone(),
            env.api.work_lock_manager_handle(),
            services(job_state),
        )
        .await;
        let completed = load_switch(&env, &switch_id).await;
        assert_eq!(
            completed.switch_maintenance_requested,
            Some(replacement.clone())
        );
        assert!(matches!(
            (job_state, completed.controller_state.value),
            (
                ConfigureSwitchCertificateState::Completed,
                SwitchControllerState::Ready
            ) | (
                ConfigureSwitchCertificateState::Failed,
                SwitchControllerState::Error { .. }
            )
        ));

        run_switch_controller_with_services(
            pool.clone(),
            env.api.work_lock_manager_handle(),
            services(job_state),
        )
        .await;
        assert_eq!(
            load_switch(&env, &switch_id).await.controller_state.value,
            SwitchControllerState::maintenance_for_request(replacement)
        );
        run_switch_controller_with_services(
            pool.clone(),
            env.api.work_lock_manager_handle(),
            services(job_state),
        )
        .await;
        let completed = load_switch(&env, &switch_id).await;
        assert_eq!(
            completed.controller_state.value,
            SwitchControllerState::Ready
        );
        assert!(completed.switch_maintenance_requested.is_none());
    }
}
