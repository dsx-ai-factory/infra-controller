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

use super::*;

async fn ready_fixture(
    pool: &sqlx::PgPool,
) -> Result<(TestEnv, RackId, Vec<SwitchId>), Box<dyn std::error::Error>> {
    let mut config = config_with_nmx_cluster_profile();
    let profile = config
        .rack_profiles
        .rack_profiles
        .get_mut("NmxCluster")
        .unwrap();
    profile.rack_capabilities.compute.count = 0;
    profile.rack_capabilities.power_shelf.count = 0;
    profile.rack_capabilities.switch.count = 2;
    let (env, rack, switches) = create_configure_nmx_cluster_test_rack(
        pool,
        TestEnvOverrides {
            config: Some(Box::new(config)),
            ..Default::default()
        },
    )
    .await?;
    let mut txn = pool.acquire().await?;
    db_rack::update(&mut txn, &rack, &RackConfig::default()).await?;
    crate::fixtures::rack::set_rack_controller_state(&mut txn, &rack, RackState::Ready).await?;
    for switch in &switches {
        set_switch_state(
            &mut txn,
            switch,
            model::switch::SwitchControllerState::Ready,
        )
        .await;
    }
    drop(txn);
    Ok((env, rack, switches))
}

async fn observe(env: &TestEnv, rack_id: &RackId) -> Result<(), Box<dyn std::error::Error>> {
    let mut rack = get_db_rack(&env.pool, rack_id).await;
    let mut services = env.rack_state_handler_services();
    let mut metrics = RackMetrics::default();
    let mut writes = DbWriteBatch::default();
    let mut ctx = StateHandlerContext::<RackStateHandlerContextObjects> {
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(90),
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut writes,
    };
    let outcome = RackStateHandler::default()
        .handle_object_state(rack_id, &mut rack, &RackState::Ready, &mut ctx)
        .await?;
    assert!(matches!(
        outcome,
        StateHandlerOutcome::Wait { .. } | StateHandlerOutcome::DoNothing { .. }
    ));
    assert!(matches!(
        get_db_rack(&env.pool, rack_id).await.controller_state.value,
        RackState::Ready
    ));
    Ok(())
}

async fn queue_observations(env: &TestEnv, ids: &[SwitchId], omit_service: bool) {
    env.rms_sim
        .queue_get_scale_up_fabric_status_response(Ok(rms::GetScaleUpFabricStatusResponse {
            status: rms::ReturnCode::Success as i32,
            fabric_status: Some(rms::ScaleUpFabricStatus {
                // Deliberately differs from the fixture's declared 72-GPU profile.
                topology_type: "vr_nvl8r1_c2g4_rtf_topology".to_string(),
                switches: ids
                    .iter()
                    .enumerate()
                    .map(|(index, id)| rms::ScaleUpFabricSwitchStatus {
                        node_id: id.to_string(),
                        enabled: index == 1,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await;
    env.rms_sim.queue_batch_get_scale_up_fabric_service_status_response(Ok(rms::BatchGetScaleUpFabricServiceStatusResponse {
        status: rms::ReturnCode::Success as i32,
        service_statuses: ids.iter().take(if omit_service { 1 } else { ids.len() }).map(|id| (id.to_string(), rms::ScaleUpFabricServiceStatusEntry {
            status_json: format!(r#"{{"status":"ok","addition-info":"{CONTROL_PLANE_STATE_CONFIGURED}"}}"#), ..Default::default()
        })).collect(), ..Default::default()
    })).await;
}

async fn assert_no_hardware_requests(env: &TestEnv) {
    assert!(
        env.rms_sim
            .submitted_configure_scale_up_fabric_manager_requests()
            .await
            .is_empty()
    );
    assert!(
        env.rms_sim
            .submitted_configure_scale_up_fabric_manager_v2_requests()
            .await
            .is_empty()
    );
    assert!(
        env.rms_sim
            .submitted_configure_switch_certificate_requests()
            .await
            .is_empty()
    );
    assert!(
        env.rms_sim
            .submitted_batch_set_scale_up_fabric_state_requests()
            .await
            .is_empty()
    );
    assert!(
        env.rms_sim
            .submitted_batch_set_power_state_requests()
            .await
            .is_empty()
    );
    assert!(env.rms_sim.submitted_firmware_requests().await.is_empty());
    assert!(
        env.rms_sim
            .submitted_apply_firmware_object_requests()
            .await
            .is_empty()
    );
    assert!(
        env.rms_sim
            .submitted_apply_switch_system_image_requests()
            .await
            .is_empty()
    );
}

async fn assert_missing(env: &TestEnv, ids: &[SwitchId]) {
    for id in ids {
        let switch = db_switch::find_by_id(&env.pool, id).await.unwrap().unwrap();
        assert!(!switch.is_primary);
        assert!(switch.fabric_manager_status.is_none());
    }
}

#[sqlx_test]
async fn ready_fabric_observation_restores_metadata_without_configuration(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, rack, ids) = ready_fixture(&pool).await?;
    queue_observations(&env, &ids, false).await;
    observe(&env, &rack).await?;
    for (index, id) in ids.iter().enumerate() {
        let switch = db_switch::find_by_id(&pool, id).await?.unwrap();
        assert_eq!(switch.is_primary, index == 1);
        let status = switch.fabric_manager_status.unwrap();
        assert_eq!(status.fabric_manager_state, FabricManagerState::Ok);
        assert_eq!(
            status.addition_info.as_deref(),
            Some(CONTROL_PLANE_STATE_CONFIGURED)
        );
    }
    let requests = env
        .rms_sim
        .submitted_get_scale_up_fabric_status_requests()
        .await;
    assert_eq!(requests.len(), 1);
    assert_node_set_contains_switches(requests[0].nodes.as_ref(), &ids);
    let services = env
        .rms_sim
        .submitted_batch_get_scale_up_fabric_service_status_requests()
        .await;
    assert_eq!(services.len(), 1);
    assert_node_set_contains_switches(services[0].nodes.as_ref(), &ids);
    // Already populated observations do not create a periodic configuration path.
    observe(&env, &rack).await?;
    assert_eq!(
        env.rms_sim
            .submitted_get_scale_up_fabric_status_requests()
            .await
            .len(),
        1
    );
    assert_no_hardware_requests(&env).await;
    Ok(())
}

#[sqlx_test]
async fn ready_fabric_observation_retries_transient_failure(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, rack, ids) = ready_fixture(&pool).await?;
    env.rms_sim
        .queue_get_scale_up_fabric_status_response(Err(RackManagerError::ApiInvocationError(
            tonic::Status::unavailable("temporary status failure"),
        )))
        .await;
    observe(&env, &rack).await?;
    assert_missing(&env, &ids).await;
    assert_eq!(
        env.rms_sim
            .submitted_get_scale_up_fabric_status_requests()
            .await
            .len(),
        1
    );
    assert!(
        env.rms_sim
            .submitted_batch_get_scale_up_fabric_service_status_requests()
            .await
            .is_empty()
    );
    queue_observations(&env, &ids, false).await;
    observe(&env, &rack).await?;
    assert!(
        db_switch::find_by_id(&pool, &ids[1])
            .await?
            .unwrap()
            .is_primary
    );
    assert_eq!(
        env.rms_sim
            .submitted_get_scale_up_fabric_status_requests()
            .await
            .len(),
        2
    );
    assert_no_hardware_requests(&env).await;
    Ok(())
}

#[sqlx_test]
async fn ready_fabric_observation_rejects_partial_service_coverage(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, rack, ids) = ready_fixture(&pool).await?;
    queue_observations(&env, &ids, true).await;
    observe(&env, &rack).await?;
    assert_missing(&env, &ids).await;
    assert_eq!(
        env.rms_sim
            .submitted_batch_get_scale_up_fabric_service_status_requests()
            .await
            .len(),
        1
    );
    assert_no_hardware_requests(&env).await;
    Ok(())
}

#[sqlx_test]
async fn ready_fabric_observation_rechecks_concurrent_lifecycle_and_membership(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The environment owns shared resource pools and one set of expected switch
    // identities. Reuse it while giving each race its own Ready rack.
    let (env, mut rack, ids) = ready_fixture(&pool).await?;
    for (index, change) in [
        "rack maintenance",
        "switch rotation",
        "membership",
        "new member",
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            rack = new_rack_id();
            let mut connection = pool.acquire().await?;
            db_rack::create(
                &mut connection,
                &rack,
                Some(&RackProfileId::new("NmxCluster")),
                &RackConfig::default(),
                None,
            )
            .await?;
            crate::fixtures::rack::set_rack_controller_state(
                &mut connection,
                &rack,
                RackState::Ready,
            )
            .await?;
            for id in &ids {
                sqlx::query("UPDATE switches SET rack_id = $1, bmc_credential_rotation_requested = false WHERE id = $2")
                    .bind(&rack)
                    .bind(id)
                    .execute(connection.as_mut())
                    .await?;
            }
        }
        queue_observations(&env, &ids, false).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();
        env.rms_sim
            .block_next_get_scale_up_fabric_status(entered_tx, resume_rx)
            .await;
        let modify = async {
            entered_rx.await.unwrap();
            match change {
                "rack maintenance" => {
                    db_rack::update(
                        pool.acquire().await.unwrap().as_mut(),
                        &rack,
                        &RackConfig {
                            maintenance_requested: Some(MaintenanceScope {
                                switch_ids: ids.clone(),
                                activities: vec![MaintenanceActivity::ConfigureNmxCluster],
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                }
                "switch rotation" => {
                    sqlx::query("UPDATE switches SET bmc_credential_rotation_requested = true WHERE id = $1").bind(ids[0]).execute(&pool).await.unwrap();
                }
                "new member" => {
                    // A new unobserved member must not receive old fabric observations.
                    db_switch::create(
                        pool.acquire().await.unwrap().as_mut(),
                        &model::switch::NewSwitch {
                            id: SwitchId::from(uuid::Uuid::new_v4()),
                            config: model::switch::SwitchConfig {
                                name: "unobserved-switch".into(),
                                enable_nmxc: false,
                                fabric_manager_config: None,
                            },
                            bmc_mac_address: None,
                            metadata: None,
                            rack_id: Some(rack.clone()),
                            slot_number: None,
                            tray_index: None,
                        },
                    )
                    .await
                    .unwrap();
                }
                "membership" => {
                    sqlx::query("UPDATE switches SET rack_id = NULL WHERE id = $1")
                        .bind(ids[0])
                        .execute(&pool)
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            resume_tx.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(observe(&env, &rack), modify);
        result?;
        let current_ids = db_switch::find_ids(
            &pool,
            model::switch::SwitchSearchFilter {
                rack_id: Some(rack.clone()),
                ..Default::default()
            },
        )
        .await?;
        assert_missing(&env, &current_ids).await;
        assert_missing(&env, &ids).await;
        assert_eq!(
            env.rms_sim
                .submitted_get_scale_up_fabric_status_requests()
                .await
                .len(),
            index + 1,
            "{change}"
        );
        assert_no_hardware_requests(&env).await;
    }
    Ok(())
}
