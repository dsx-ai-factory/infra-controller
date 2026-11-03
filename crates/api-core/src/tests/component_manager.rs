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

use std::collections::HashMap;
use std::sync::Arc;

use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, Credentials};
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::SwitchId;
use component_manager::mock::MockNvSwitchManager;
use mac_address::MacAddress;
use model::rack::{
    FirmwareProgressState, FirmwareUpgradeDeviceStatus, FirmwareUpgradeJob, MaintenanceActivity,
    RackConfig, RackFirmwareUpgradeState, RackFirmwareUpgradeStatus,
};
use model::switch::{NewSwitch, SwitchConfig};
use rpc::forge as rpc;
use tonic::Request;

use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::site_explorer::create_expected_switch;
use crate::tests::common::api_fixtures::{TestEnv, create_test_env};

#[crate::sqlx_test]
async fn switch_firmware_status_uses_only_current_cycle_persistence(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let switch_ids = (0..3)
        .map(|_| SwitchId::from(uuid::Uuid::new_v4()))
        .collect::<Vec<_>>();
    let firmware_activity = MaintenanceActivity::FirmwareUpgrade {
        firmware_version: Some("firmware-object-json".into()),
        components: vec![],
        force_update: false,
    };

    let mut txn = pool.begin().await?;
    for (index, switch_id) in switch_ids.iter().enumerate() {
        db::switch::create(
            txn.as_mut(),
            &NewSwitch {
                id: *switch_id,
                config: SwitchConfig {
                    name: format!("firmware-status-switch-{index}"),
                    enable_nmxc: false,
                    fabric_manager_config: None,
                },
                bmc_mac_address: None,
                metadata: None,
                rack_id: None,
                slot_number: None,
                tray_index: None,
            },
        )
        .await?;
        db::switch::set_switch_reprovisioning_requested(
            txn.as_mut(),
            *switch_id,
            "rack-test",
            vec![firmware_activity.clone()],
        )
        .await?;
    }

    let switches = db::switch::find_by(
        txn.as_mut(),
        db::ObjectColumnFilter::List(db::switch::IdColumn, &switch_ids),
    )
    .await?;
    let requested_at_by_id = switches
        .into_iter()
        .map(|switch| {
            let requested_at = switch
                .switch_reprovisioning_requested
                .expect("test request must be persisted")
                .requested_at;
            (switch.id, requested_at)
        })
        .collect::<HashMap<_, _>>();

    let stale_requested_at = requested_at_by_id[&switch_ids[0]];
    db::switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_ids[0],
        Some(&RackFirmwareUpgradeStatus {
            task_id: "stale-task".into(),
            status: RackFirmwareUpgradeState::Completed,
            started_at: Some(stale_requested_at - chrono::Duration::minutes(2)),
            ended_at: Some(stale_requested_at - chrono::Duration::minutes(1)),
        }),
    )
    .await?;

    let current_requested_at = requested_at_by_id[&switch_ids[1]];
    db::switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_ids[1],
        Some(&RackFirmwareUpgradeStatus {
            task_id: "current-task".into(),
            status: RackFirmwareUpgradeState::Failed {
                cause: "current backend failure".into(),
            },
            started_at: Some(current_requested_at),
            ended_at: Some(current_requested_at),
        }),
    )
    .await?;
    txn.commit().await?;

    // Deliberately omit Component Manager: every switch has an active persisted
    // request, so status selection must be fully answerable from the database.
    let api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .build();
    let response = crate::handlers::component_manager::get_component_firmware_status(
        &api,
        Request::new(rpc::GetComponentFirmwareStatusRequest {
            target: Some(
                rpc::get_component_firmware_status_request::Target::SwitchIds(rpc::SwitchIdList {
                    ids: switch_ids.clone(),
                }),
            ),
        }),
    )
    .await?
    .into_inner();
    let status_by_id = response
        .statuses
        .into_iter()
        .map(|status| {
            let component_id = status
                .result
                .as_ref()
                .expect("every firmware status must carry a result")
                .component_id
                .clone()
                .expect("a switch-id target always reports the component id");
            (component_id, status)
        })
        .collect::<HashMap<_, _>>();

    assert_eq!(
        status_by_id[&switch_ids[0].to_string()].state,
        rpc::FirmwareUpdateState::FwStateQueued as i32,
        "a stale terminal result must not complete the new request"
    );
    let current = &status_by_id[&switch_ids[1].to_string()];
    assert_eq!(
        current.state,
        rpc::FirmwareUpdateState::FwStateFailed as i32
    );
    assert_eq!(
        current
            .result
            .as_ref()
            .expect("current status must carry a result")
            .error,
        "current backend failure"
    );
    assert_eq!(
        status_by_id[&switch_ids[2].to_string()].state,
        rpc::FirmwareUpdateState::FwStateQueued as i32,
        "an accepted request without a persisted device result is still queued"
    );

    for switch_id in &switch_ids {
        assert_eq!(
            status_by_id[&switch_id.to_string()].target_version,
            "firmware-object-json",
            "the active request supplies the target version the backend never persists"
        );
    }

    Ok(())
}

async fn create_terminal_rack_switch_firmware_failure(
    pool: sqlx::PgPool,
) -> Result<(TestEnv, RackId, SwitchId, MacAddress), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let started_at = chrono::Utc::now() - chrono::Duration::minutes(2);
    let completed_at = chrono::Utc::now() - chrono::Duration::minutes(1);
    let mut txn = pool.begin().await?;

    db::rack::create(
        txn.as_mut(),
        &rack_id,
        Some(&RackProfileId::new("NVL72")),
        &RackConfig::default(),
        None,
    )
    .await?;

    let expected_switch = create_expected_switch(txn.as_mut(), 20).await;

    let switch_id = model::switch::switch_id::from_hardware_info(
        &expected_switch.serial_number,
        "NVIDIA",
        "Switch",
        carbide_uuid::switch::SwitchIdSource::ProductBoardChassisSerial,
        carbide_uuid::switch::SwitchType::NvLink,
    )?;

    db::switch::create(
        txn.as_mut(),
        &NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: expected_switch.metadata.name.clone(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(expected_switch.bmc_mac_address),
            metadata: None,
            rack_id: Some(rack_id.clone()),
            slot_number: Some(0),
            tray_index: Some(0),
        },
    )
    .await?;

    db::rack::update_firmware_upgrade_job(
        txn.as_mut(),
        &rack_id,
        Some(&FirmwareUpgradeJob {
            job_id: Some("rack-job".into()),
            firmware_id: Some("fw-42".into()),
            status: Some(FirmwareProgressState::Failed),
            started_at: Some(started_at),
            completed_at: Some(completed_at),
            switches: vec![FirmwareUpgradeDeviceStatus {
                node_id: "switch-node".into(),
                mac: expected_switch.bmc_mac_address.to_string(),
                bmc_ip: String::new(),
                status: FirmwareProgressState::Failed,
                job_id: Some("switch-job".into()),
                parent_job_id: Some("rack-job".into()),
                error_message: Some("rack firmware failed".into()),
            }],
            ..Default::default()
        }),
    )
    .await?;

    db::switch::update_firmware_upgrade_status(
        txn.as_mut(),
        switch_id,
        Some(&RackFirmwareUpgradeStatus {
            task_id: "switch-job".into(),
            status: RackFirmwareUpgradeState::Failed {
                cause: "rack firmware failed".into(),
            },
            started_at: Some(started_at),
            ended_at: Some(completed_at),
        }),
    )
    .await?;

    txn.commit().await?;

    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: expected_switch.bmc_mac_address,
                },
            },
            &Credentials::UsernamePassword {
                username: "root".into(),
                password: "notforprod".into(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: expected_switch.bmc_mac_address,
            },
            &Credentials::UsernamePassword {
                username: "nvos-admin".into(),
                password: "nvos-pass".into(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok((env, rack_id, switch_id, expected_switch.bmc_mac_address))
}

async fn get_switch_firmware_status(
    api: &crate::api::Api,
    switch_id: SwitchId,
) -> Result<rpc::FirmwareUpdateStatus, Box<dyn std::error::Error>> {
    let status = crate::handlers::component_manager::get_component_firmware_status(
        api,
        Request::new(rpc::GetComponentFirmwareStatusRequest {
            target: Some(
                rpc::get_component_firmware_status_request::Target::SwitchIds(rpc::SwitchIdList {
                    ids: vec![switch_id],
                }),
            ),
        }),
    )
    .await?
    .into_inner()
    .statuses
    .into_iter()
    .next()
    .ok_or_else(|| eyre::eyre!("the switch status response must not be empty"))?;

    Ok(status)
}

async fn get_switch_firmware_status_by_bmc_mac(
    api: &crate::api::Api,
    bmc_mac: MacAddress,
) -> Result<rpc::FirmwareUpdateStatus, Box<dyn std::error::Error>> {
    let status = crate::handlers::component_manager::get_component_firmware_status(
        api,
        Request::new(rpc::GetComponentFirmwareStatusRequest {
            target: Some(
                rpc::get_component_firmware_status_request::Target::SwitchBmcMacs(
                    rpc::MacAddressList {
                        mac_addresses: vec![bmc_mac.to_string()],
                    },
                ),
            ),
        }),
    )
    .await?
    .into_inner()
    .statuses
    .into_iter()
    .next()
    .ok_or_else(|| eyre::eyre!("the switch status response must not be empty"))?;

    Ok(status)
}

#[crate::sqlx_test]
async fn terminal_rack_switch_status_survives_request_cleanup(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _, switch_id, _) = create_terminal_rack_switch_firmware_failure(pool).await?;
    let retained = get_switch_firmware_status(&env.api, switch_id).await?;

    assert_eq!(
        retained.state,
        rpc::FirmwareUpdateState::FwStateFailed as i32
    );

    assert_eq!(retained.target_version, "fw-42");

    let retained_result = retained
        .result
        .as_ref()
        .ok_or_else(|| eyre::eyre!("the retained status must include a component result"))?;

    assert_eq!(
        retained_result.status,
        rpc::ComponentManagerStatusCode::InternalError as i32
    );

    assert_eq!(retained_result.error, "rack firmware failed");

    Ok(())
}

#[crate::sqlx_test]
async fn terminal_rack_switch_status_survives_endpoint_resolution_failure(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _, switch_id, _) = create_terminal_rack_switch_firmware_failure(pool.clone()).await?;

    let component_manager = env
        .test_component_manager
        .clone()
        .ok_or_else(|| eyre::eyre!("the test environment must include a component manager"))?;

    let api_without_credentials = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_component_manager(component_manager)
    .build();

    let retained = get_switch_firmware_status(&api_without_credentials, switch_id).await?;

    let result = retained
        .result
        .as_ref()
        .ok_or_else(|| eyre::eyre!("the retained status must include a component result"))?;

    assert_eq!(
        retained.state,
        rpc::FirmwareUpdateState::FwStateFailed as i32
    );

    assert_eq!(retained.target_version, "fw-42");
    assert_eq!(result.error, "rack firmware failed");

    Ok(())
}

#[crate::sqlx_test]
async fn tracked_backend_switch_job_supersedes_terminal_rack_status(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, _, switch_id, _) = create_terminal_rack_switch_firmware_failure(pool.clone()).await?;

    let mut component_manager = env
        .test_component_manager
        .as_deref()
        .ok_or_else(|| eyre::eyre!("the test environment must include a component manager"))?
        .clone();

    component_manager.nv_switch = Arc::new(MockNvSwitchManager::default());

    let direct_api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_credential_manager(env.api.credential_manager.clone())
    .with_component_manager(Arc::new(component_manager))
    .build();

    let superseding = get_switch_firmware_status(&direct_api, switch_id).await?;

    assert_eq!(
        superseding.state,
        rpc::FirmwareUpdateState::FwStateCompleted as i32
    );

    assert_eq!(superseding.target_version, "mock-1.0.0");

    assert_eq!(
        superseding
            .result
            .as_ref()
            .ok_or_else(|| eyre::eyre!("the backend status must include a component result"))?
            .status,
        rpc::ComponentManagerStatusCode::Success as i32
    );

    Ok(())
}

#[crate::sqlx_test]
async fn untracked_backend_switch_job_is_unknown_for_every_switch_target(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (env, rack_id, switch_id, bmc_mac) =
        create_terminal_rack_switch_firmware_failure(pool.clone()).await?;
    let mut txn = pool.begin().await?;

    db::rack::update_firmware_upgrade_job(txn.as_mut(), &rack_id, None).await?;
    db::switch::update_firmware_upgrade_status(txn.as_mut(), switch_id, None).await?;
    txn.commit().await?;

    let by_switch_id = get_switch_firmware_status(&env.api, switch_id).await?;
    let by_ingested_bmc_mac = get_switch_firmware_status_by_bmc_mac(&env.api, bmc_mac).await?;

    let mut txn = pool.begin().await?;
    db::switch::final_delete(switch_id, txn.as_mut()).await?;
    txn.commit().await?;

    let by_pre_ingestion_bmc_mac = get_switch_firmware_status_by_bmc_mac(&env.api, bmc_mac).await?;

    for (target, status) in [
        ("switch ID", by_switch_id),
        ("ingested BMC MAC", by_ingested_bmc_mac),
        ("pre-ingestion BMC MAC", by_pre_ingestion_bmc_mac),
    ] {
        assert_eq!(
            status.state,
            rpc::FirmwareUpdateState::FwStateUnknown as i32,
            "{target}"
        );

        let result = status
            .result
            .as_ref()
            .ok_or_else(|| eyre::eyre!("the {target} status must include a component result"))?;

        assert_eq!(
            result.status,
            rpc::ComponentManagerStatusCode::InternalError as i32,
            "{target}"
        );

        assert_eq!(
            result.error, "no firmware job tracked for this switch",
            "{target}"
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn component_reset_dispatches_valid_targets_and_polls_by_identity(pool: sqlx::PgPool) {
    use ::rpc::forge::forge_server::Forge;
    use api_test_helper::mock_rms::MockRmsApi;
    use component_manager::rms::RmsBackend;
    use librms::protos::rack_manager as rms;
    use rpc::component_reset_request::Target;

    let env = create_test_env(pool.clone()).await;
    let mock = Arc::new(MockRmsApi::new());
    let make_api = || {
        let mut cm = env.test_component_manager.as_deref().unwrap().clone();
        cm.nv_switch = Arc::new(RmsBackend::new(
            mock.clone(),
            None,
            pool.clone(),
            Arc::new(
                crate::tests::common::api_fixtures::get_config_with_rack_profiles().rack_profiles,
            ),
            false,
        ));
        TestApiBuilder::new(
            pool.clone(),
            env.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .with_credential_manager(env.api.credential_manager.clone())
        .with_component_manager(Arc::new(cm))
        .build()
    };
    let api = make_api();
    let unknown_id = SwitchId::from(uuid::Uuid::new_v4());
    let valid_structure = rpc::ComponentResetRequest {
        target: Some(Target::SwitchIds(rpc::SwitchIdList {
            ids: vec![unknown_id],
        })),
        action: rpc::ComponentResetAction::SdnFactoryReset as i32,
        domain_name: None,
    };
    let cases = [
        (
            "absent target",
            rpc::ComponentResetRequest {
                target: None,
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "empty targets",
            rpc::ComponentResetRequest {
                target: Some(Target::SwitchIds(rpc::SwitchIdList { ids: vec![] })),
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "duplicate targets",
            rpc::ComponentResetRequest {
                target: Some(Target::SwitchIds(rpc::SwitchIdList {
                    ids: vec![unknown_id, unknown_id],
                })),
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "same MAC in different casing",
            rpc::ComponentResetRequest {
                target: Some(Target::SwitchBmcMacs(rpc::MacAddressList {
                    mac_addresses: vec!["AA:BB:CC:00:00:01".into(), "aa:bb:cc:00:00:01".into()],
                })),
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "unspecified action",
            rpc::ComponentResetRequest {
                action: 0,
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "unrecognized action",
            rpc::ComponentResetRequest {
                action: 99,
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "unsupported action",
            rpc::ComponentResetRequest {
                action: rpc::ComponentResetAction::BmcReset as i32,
                ..valid_structure.clone()
            },
            tonic::Code::Unimplemented,
        ),
        (
            "blank domain",
            rpc::ComponentResetRequest {
                domain_name: Some(" ".into()),
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "domain on BMC reset",
            rpc::ComponentResetRequest {
                action: rpc::ComponentResetAction::BmcReset as i32,
                domain_name: Some("domain".into()),
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
        (
            "SDN reset on compute",
            rpc::ComponentResetRequest {
                target: Some(Target::ComputeBmcMacs(rpc::MacAddressList {
                    mac_addresses: vec!["aa:bb:cc:00:00:01".into()],
                })),
                ..valid_structure.clone()
            },
            tonic::Code::InvalidArgument,
        ),
    ];
    for (label, request, code) in cases {
        assert_eq!(
            api.component_reset(Request::new(request))
                .await
                .unwrap_err()
                .code(),
            code,
            "{label}"
        );
    }
    assert!(
        mock.batch_reset_switch_sdn_factory_default_calls()
            .await
            .is_empty()
    );

    let rack_id = RackId::new("sdn-reset-rack");
    let mut txn = pool.begin().await.unwrap();
    crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await
        .unwrap();
    let switch = db::test_support::switch::create_seeded_discovered(&mut txn, 61, "sdn-reset")
        .await
        .unwrap();
    let second_switch =
        db::test_support::switch::create_seeded_discovered(&mut txn, 63, "second-sdn-reset")
            .await
            .unwrap();
    let rejected_switch =
        db::test_support::switch::create_seeded_discovered(&mut txn, 64, "rejected-sdn-reset")
            .await
            .unwrap();
    // Its endpoint resolves, but neither the switch nor its expected inventory
    // has a rack. The RMS backend returns this failure before submitted switches, so
    // the API must correlate backend results by MAC instead of their position.
    let unresolved_switch =
        db::test_support::switch::create_seeded_discovered(&mut txn, 65, "unresolved-sdn-reset")
            .await
            .unwrap();
    for id in [switch.id, second_switch.id, rejected_switch.id] {
        sqlx::query("UPDATE switches SET rack_id = $1 WHERE id = $2")
            .bind(&rack_id)
            .bind(id)
            .execute(&mut *txn)
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();
    let bmc_mac_address = switch.bmc_mac_address.unwrap();
    let second_mac = second_switch.bmc_mac_address.unwrap();
    let rejected_mac = rejected_switch.bmc_mac_address.unwrap();
    let unresolved_mac = unresolved_switch.bmc_mac_address.unwrap();
    let credentials: Vec<_> = [bmc_mac_address, second_mac, rejected_mac, unresolved_mac]
        .into_iter()
        .flat_map(|bmc_mac_address| {
            [
                CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
                },
                CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            ]
        })
        .collect();
    for key in &credentials {
        env.api
            .credential_manager
            .set_credentials(key, &Credentials::new("reset-operator", "test-password"))
            .await
            .unwrap();
    }
    mock.enqueue_batch_reset_switch_sdn_factory_default(Ok(
        rms::BatchResetSwitchSdnFactoryDefaultResponse {
            response: Some(rms::NodeBatchResponse {
                job_id: "parent".into(),
                status: rms::ReturnCode::Failure as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 3,
                    successful_nodes: 2,
                    failed_nodes: 1,
                }),
                node_results: vec![rms::NodeOperationResult {
                    node_id: rejected_switch.id.to_string(),
                    status: rms::ReturnCode::Failure as i32,
                    error_message: "certificate domain missing".into(),
                }],
                ..Default::default()
            }),
        },
    ))
    .await;
    let submitted = api
        .component_reset(Request::new(rpc::ComponentResetRequest {
            target: Some(Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![
                    switch.id,
                    unknown_id,
                    unresolved_switch.id,
                    rejected_switch.id,
                    second_switch.id,
                ],
            })),
            domain_name: Some("certificate-domain".into()),
            ..valid_structure
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(submitted.results.len(), 5);
    for (result, (id, mac, status)) in submitted.results.iter().zip([
        (
            switch.id,
            Some(bmc_mac_address),
            rpc::ComponentManagerStatusCode::Success,
        ),
        (unknown_id, None, rpc::ComponentManagerStatusCode::NotFound),
        (
            unresolved_switch.id,
            Some(unresolved_mac),
            rpc::ComponentManagerStatusCode::InternalError,
        ),
        (
            rejected_switch.id,
            Some(rejected_mac),
            rpc::ComponentManagerStatusCode::InternalError,
        ),
        (
            second_switch.id,
            Some(second_mac),
            rpc::ComponentManagerStatusCode::Success,
        ),
    ]) {
        assert_eq!(result.component_id, Some(id.to_string()));
        assert_eq!(result.mac_address, mac.map(|mac| mac.to_string()));
        assert_eq!(result.status, status as i32, "{id}: {}", result.error);
    }
    assert!(
        submitted.results[3]
            .error
            .contains("certificate domain missing")
    );
    let calls = mock.batch_reset_switch_sdn_factory_default_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].domain.as_deref(), Some("certificate-domain"));
    assert_eq!(
        calls[0]
            .nodes
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>(),
        [
            switch.id.to_string(),
            rejected_switch.id.to_string(),
            second_switch.id.to_string()
        ]
    );
    for (mac, expected) in [
        (bmc_mac_address, Some("parent")),
        (second_mac, Some("parent")),
        (rejected_mac, None),
        (unresolved_mac, None),
    ] {
        assert_eq!(
            db::direct_dispatch_rms_job::get(
                &pool,
                mac,
                db::direct_dispatch_rms_job::RmsJobKind::SwitchSdnFactoryReset
            )
            .await
            .unwrap()
            .as_deref(),
            expected,
        );
    }

    mock.enqueue_batch_reset_switch_sdn_factory_default(Err(
        librms::RackManagerError::ApiInvocationError(tonic::Status::unavailable("reply lost")),
    ))
    .await;
    let uncertain = api
        .component_reset(Request::new(rpc::ComponentResetRequest {
            target: Some(Target::SwitchIds(rpc::SwitchIdList {
                ids: vec![switch.id, unknown_id, second_switch.id],
            })),
            action: rpc::ComponentResetAction::SdnFactoryReset as i32,
            domain_name: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .results;
    assert_eq!(
        uncertain
            .iter()
            .map(|result| result.status)
            .collect::<Vec<_>>(),
        [
            rpc::ComponentManagerStatusCode::InternalError as i32,
            rpc::ComponentManagerStatusCode::NotFound as i32,
            rpc::ComponentManagerStatusCode::InternalError as i32,
        ]
    );
    for result in [&uncertain[0], &uncertain[2]] {
        assert!(result.error.contains("reply lost"));
    }
    let calls = mock.batch_reset_switch_sdn_factory_default_calls().await;
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].nodes.as_ref().unwrap().nodes.len(), 2);

    for key in &credentials {
        env.api
            .credential_manager
            .delete_credentials(key)
            .await
            .unwrap();
    }
    // MAC resolution retains the ingested ID when endpoint credentials are missing.
    let rejected = api
        .component_reset(Request::new(rpc::ComponentResetRequest {
            target: Some(Target::SwitchBmcMacs(rpc::MacAddressList {
                mac_addresses: vec![bmc_mac_address.to_string().to_uppercase()],
            })),
            action: rpc::ComponentResetAction::SdnFactoryReset as i32,
            domain_name: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .results;
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].component_id, Some(switch.id.to_string()));
    assert_eq!(rejected[0].mac_address, Some(bmc_mac_address.to_string()));
    assert_eq!(
        rejected[0].status,
        rpc::ComponentManagerStatusCode::NotFound as i32
    );
    assert_eq!(
        mock.batch_reset_switch_sdn_factory_default_calls()
            .await
            .len(),
        2
    );

    // A fresh backend retrieves persisted references without endpoint credentials
    // or rack membership; status resolution needs only the switch's current ID.
    sqlx::query("UPDATE switches SET rack_id = NULL WHERE id = $1")
        .bind(switch.id)
        .execute(&pool)
        .await
        .unwrap();
    let restarted_api = make_api();
    let parent_status = rms::GetJobStatusResponse {
        job_states: vec![
            rms::JobStatus {
                job_id: "parent".into(),
                child_job_ids: vec!["child-one".into(), "child-two".into()],
                execution_state: rms::JobExecutionState::Completed as i32,
                ..Default::default()
            },
            rms::JobStatus {
                job_id: "child-two".into(),
                parent_job_id: Some("parent".into()),
                node_id: Some(second_switch.id.to_string()),
                execution_state: rms::JobExecutionState::Failed as i32,
                error_message: "reset failed".into(),
                ..Default::default()
            },
            rms::JobStatus {
                job_id: "child-one".into(),
                parent_job_id: Some("parent".into()),
                node_id: Some(switch.id.to_string()),
                execution_state: rms::JobExecutionState::Completed as i32,
                ..Default::default()
            },
        ],
    };
    for target in [
        rpc::get_component_reset_status_request::Target::SwitchIds(rpc::SwitchIdList {
            ids: vec![switch.id, rejected_switch.id, second_switch.id],
        }),
        rpc::get_component_reset_status_request::Target::SwitchBmcMacs(rpc::MacAddressList {
            mac_addresses: vec![
                bmc_mac_address.to_string(),
                rejected_mac.to_string(),
                second_mac.to_string(),
            ],
        }),
    ] {
        for _ in 0..2 {
            mock.enqueue_get_job_status(Ok(parent_status.clone())).await;
        }
        let response = restarted_api
            .get_component_reset_status(Request::new(rpc::GetComponentResetStatusRequest {
                target: Some(target),
                action: rpc::ComponentResetAction::SdnFactoryReset as i32,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.statuses.len(), 3);
        for (status, (id, mac, expected_state, expected_status)) in response.statuses.iter().zip([
            (
                switch.id,
                bmc_mac_address,
                rpc::ComponentResetState::Completed,
                rpc::ComponentManagerStatusCode::Success,
            ),
            (
                rejected_switch.id,
                rejected_mac,
                rpc::ComponentResetState::Unknown,
                rpc::ComponentManagerStatusCode::NotFound,
            ),
            (
                second_switch.id,
                second_mac,
                rpc::ComponentResetState::Failed,
                rpc::ComponentManagerStatusCode::Success,
            ),
        ]) {
            assert_eq!(status.state, expected_state as i32);
            assert_eq!(
                status.action,
                rpc::ComponentResetAction::SdnFactoryReset as i32
            );
            let result = status.result.as_ref().unwrap();
            assert_eq!(result.component_id, Some(id.to_string()));
            assert_eq!(result.mac_address, Some(mac.to_string()));
            assert_eq!(result.status, expected_status as i32);
            if expected_state == rpc::ComponentResetState::Failed {
                assert!(result.error.contains("reset failed"));
            }
        }
    }
    let calls = mock.get_job_status_calls().await;
    assert_eq!(calls.len(), 4);
    assert!(calls.iter().all(|call| call.job_id == "parent"));
    mock.enqueue_get_job_status(Ok(Default::default())).await;
    let statuses = restarted_api
        .get_component_reset_status(Request::new(rpc::GetComponentResetStatusRequest {
            target: Some(
                rpc::get_component_reset_status_request::Target::SwitchBmcMacs(
                    rpc::MacAddressList {
                        mac_addresses: vec![
                            bmc_mac_address.to_string(),
                            "bad-mac".into(),
                            "aa:bb:cc:00:00:02".into(),
                        ],
                    },
                ),
            ),
            action: rpc::ComponentResetAction::SdnFactoryReset as i32,
        }))
        .await
        .unwrap()
        .into_inner()
        .statuses;
    assert_eq!(
        statuses
            .iter()
            .map(|status| status.result.as_ref().unwrap().status)
            .collect::<Vec<_>>(),
        vec![
            rpc::ComponentManagerStatusCode::OutcomeUnknown as i32,
            rpc::ComponentManagerStatusCode::InvalidArgument as i32,
            rpc::ComponentManagerStatusCode::NotFound as i32
        ]
    );
    assert!(
        statuses
            .iter()
            .all(|status| status.state == rpc::ComponentResetState::Unknown as i32)
    );
}

#[crate::sqlx_test]
async fn rms_job_status_preserves_standalone_and_child_records(pool: sqlx::PgPool) {
    use ::rpc::forge::forge_server::Forge;
    use api_test_helper::mock_rms::MockRmsApi;
    use librms::protos::rack_manager as rms;

    let env = create_test_env(pool.clone()).await;
    let mock = Arc::new(MockRmsApi::new());
    let api = TestApiBuilder::new(
        pool,
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_rms_client(mock.clone())
    .build();
    let invalid = api
        .get_rms_job_status(Request::new(rpc::GetRmsJobStatusRequest {
            job_id: " ".into(),
            include_child_job_states: false,
        }))
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert!(mock.get_job_status_calls().await.is_empty());

    for include_children in [false, true] {
        let job = rms::JobStatus {
            job_id: "reset".into(),
            parent_job_id: None,
            child_job_ids: if include_children {
                vec!["child".into()]
            } else {
                vec![]
            },
            execution_state: rms::JobExecutionState::Completed as i32,
            error_message: String::new(),
            error_code: rms::JobError::Unspecified as i32,
            result_json: r#"{"reset":true}"#.into(),
            state_description: "completed".into(),
            rack_id: Some("rack".into()),
            node_id: Some("switch".into()),
            created_at: Some(std::time::SystemTime::UNIX_EPOCH.into()),
            updated_at: Some(std::time::SystemTime::UNIX_EPOCH.into()),
        };
        let mut jobs = vec![job.clone()];
        if include_children {
            jobs.push(rms::JobStatus {
                job_id: "child".into(),
                parent_job_id: Some("reset".into()),
                execution_state: rms::JobExecutionState::Failed as i32,
                error_message: "failure details".into(),
                error_code: rms::JobError::Timeout as i32,
                ..Default::default()
            });
        }
        mock.enqueue_get_job_status(Ok(rms::GetJobStatusResponse {
            job_states: jobs.clone(),
        }))
        .await;
        let response = api
            .get_rms_job_status(Request::new(rpc::GetRmsJobStatusRequest {
                job_id: "reset".into(),
                include_child_job_states: include_children,
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.job_states.len(), jobs.len());
        for (observed, expected) in response.job_states.iter().zip(jobs) {
            assert_eq!(observed.job_id, expected.job_id);
            assert_eq!(observed.parent_job_id, expected.parent_job_id);
            assert_eq!(observed.child_job_ids, expected.child_job_ids);
            assert_eq!(observed.execution_state, expected.execution_state);
            assert_eq!(observed.error_message, expected.error_message);
            assert_eq!(observed.error_code, expected.error_code);
            assert_eq!(observed.result_json, expected.result_json);
            assert_eq!(observed.state_description, expected.state_description);
            assert_eq!(observed.rack_id, expected.rack_id);
            assert_eq!(observed.node_id, expected.node_id);
            assert_eq!(observed.created_at, expected.created_at.map(Into::into));
            assert_eq!(observed.updated_at, expected.updated_at.map(Into::into));
        }
        let calls = mock.get_job_status_calls().await;
        let last = calls.last().unwrap();
        assert_eq!(last.job_id, "reset");
        assert_eq!(last.include_child_job_states, include_children);
    }
}

#[crate::sqlx_test]
async fn component_reset_tracks_pre_ingestion_mac_after_ingestion(pool: sqlx::PgPool) {
    use ::rpc::forge::forge_server::Forge;
    use api_test_helper::mock_rms::MockRmsApi;
    use component_manager::rms::RmsBackend;
    use librms::protos::rack_manager as rms;

    let env = create_test_env(pool.clone()).await;
    let mock = Arc::new(MockRmsApi::new());
    let mut cm = env.test_component_manager.as_deref().unwrap().clone();
    cm.nv_switch = Arc::new(RmsBackend::new(
        mock.clone(),
        None,
        pool.clone(),
        Arc::new(crate::tests::common::api_fixtures::get_config_with_rack_profiles().rack_profiles),
        false,
    ));
    let api = TestApiBuilder::new(
        pool.clone(),
        env.common_pools.clone(),
        env.api.work_lock_manager_handle.clone(),
    )
    .with_credential_manager(env.api.credential_manager.clone())
    .with_component_manager(Arc::new(cm))
    .build();
    let rack_id = RackId::new("pre-ingestion-reset-rack");
    let mut txn = pool.begin().await.unwrap();
    crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await
        .unwrap();
    let switch =
        db::test_support::switch::create_seeded_discovered(&mut txn, 62, "pre-ingestion-reset")
            .await
            .unwrap();
    let bmc_mac = switch.bmc_mac_address.unwrap();
    sqlx::query("UPDATE expected_switches SET rack_id = $1 WHERE bmc_mac_address = $2")
        .bind(&rack_id)
        .bind(bmc_mac)
        .execute(&mut *txn)
        .await
        .unwrap();
    sqlx::query("DELETE FROM switches WHERE id = $1")
        .bind(switch.id)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    env.api
        .credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin {
                bmc_mac_address: bmc_mac,
            },
            &Credentials::new("reset-operator", "test-password"),
        )
        .await
        .unwrap();
    mock.enqueue_batch_reset_switch_sdn_factory_default(Ok(
        rms::BatchResetSwitchSdnFactoryDefaultResponse {
            response: Some(rms::NodeBatchResponse {
                job_id: "pre-ingestion-job".into(),
                status: rms::ReturnCode::Success as i32,
                stats: Some(rms::NodeOperationStats {
                    total_nodes: 1,
                    successful_nodes: 1,
                    failed_nodes: 0,
                }),
                ..Default::default()
            }),
        },
    ))
    .await;
    let results = api
        .component_reset(Request::new(rpc::ComponentResetRequest {
            target: Some(rpc::component_reset_request::Target::SwitchBmcMacs(
                rpc::MacAddressList {
                    mac_addresses: vec!["invalid".into(), bmc_mac.to_string()],
                },
            )),
            action: rpc::ComponentResetAction::SdnFactoryReset as i32,
            domain_name: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .results;
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].status,
        rpc::ComponentManagerStatusCode::InvalidArgument as i32
    );
    assert_eq!(
        results[1].status,
        rpc::ComponentManagerStatusCode::Success as i32
    );
    assert_eq!(results[1].component_id, None);
    assert_eq!(results[1].mac_address, Some(bmc_mac.to_string()));
    let calls = mock.batch_reset_switch_sdn_factory_default_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].nodes.as_ref().unwrap().nodes[0].node_id,
        bmc_mac.to_string()
    );
    assert_eq!(calls[0].domain, None);

    let mut txn = pool.begin().await.unwrap();
    db::switch::create(
        &mut txn,
        &NewSwitch {
            id: switch.id,
            config: switch.config,
            bmc_mac_address: Some(bmc_mac),
            metadata: None,
            rack_id: Some(rack_id),
            slot_number: Some(0),
            tray_index: Some(0),
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    env.api
        .credential_manager
        .delete_credentials(&CredentialKey::SwitchNvosAdmin {
            bmc_mac_address: bmc_mac,
        })
        .await
        .unwrap();
    mock.enqueue_get_job_status(Ok(rms::GetJobStatusResponse {
        job_states: vec![
            rms::JobStatus {
                job_id: "pre-ingestion-job".into(),
                child_job_ids: vec!["child".into()],
                execution_state: rms::JobExecutionState::Completed as i32,
                ..Default::default()
            },
            rms::JobStatus {
                job_id: "child".into(),
                parent_job_id: Some("pre-ingestion-job".into()),
                node_id: Some(bmc_mac.to_string()),
                execution_state: rms::JobExecutionState::Completed as i32,
                ..Default::default()
            },
        ],
    }))
    .await;
    let statuses = api
        .get_component_reset_status(Request::new(rpc::GetComponentResetStatusRequest {
            target: Some(rpc::get_component_reset_status_request::Target::SwitchIds(
                rpc::SwitchIdList {
                    ids: vec![switch.id],
                },
            )),
            action: rpc::ComponentResetAction::SdnFactoryReset as i32,
        }))
        .await
        .unwrap()
        .into_inner()
        .statuses;
    assert_eq!(statuses.len(), 1);
    assert_eq!(
        statuses[0].state,
        rpc::ComponentResetState::Completed as i32
    );
    assert_eq!(
        statuses[0].result.as_ref().unwrap().component_id,
        Some(switch.id.to_string())
    );
    assert_eq!(
        statuses[0].result.as_ref().unwrap().mac_address,
        Some(bmc_mac.to_string())
    );
    assert_eq!(
        mock.get_job_status_calls().await[0].job_id,
        "pre-ingestion-job"
    );
}
