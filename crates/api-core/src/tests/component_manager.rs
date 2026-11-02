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
