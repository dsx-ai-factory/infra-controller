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

use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::firmware::FirmwareComponentType;
use model::machine::{
    CleanupContext, CleanupState, FailureCause, FailureDetails, FailureSource,
    HostReprovisionState, InstanceState, MachineState, ManagedHostState, MeasuringState, RetryInfo,
};
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_agent_control_response::LegacyAction;
use rpc::forge_agent_control_response::Action;

trait DbMachineCleanupExt {
    async fn clear_cleanup_time(&self, txn: &mut sqlx::PgTransaction<'_>);

    async fn update_cleanup_time(&self, txn: &mut sqlx::PgTransaction<'_>);
}

impl DbMachineCleanupExt for model::machine::Machine {
    async fn clear_cleanup_time(&self, txn: &mut sqlx::PgTransaction<'_>) {
        db::machine::clear_cleanup_time(&self.id, txn.as_mut())
            .await
            .expect("machine cleanup time should be cleared");
    }

    async fn update_cleanup_time(&self, txn: &mut sqlx::PgTransaction<'_>) {
        db::machine::update_cleanup_time(self, txn.as_mut())
            .await
            .expect("machine cleanup time should be updated");
    }
}

struct TestContext {
    env: TestHarness,
    mh: TestManagedHost,
    admin_segment: TestNetworkSegment,
}

async fn init(pool: PgPool) -> TestContext {
    let env = TestHarness::builder(pool).build().await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (managed_host, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await;
    TestContext {
        env,
        mh: managed_host,
        admin_segment,
    }
}

#[sqlx_test]
async fn host_reprovision_scout_upgrade_does_not_reset_without_cleanup_timestamp(pool: PgPool) {
    let TestContext { env, mh, .. } = init(pool).await;
    let upgrade_task_id = uuid::Uuid::new_v4().to_string();
    let task_json = serde_json::json!({
        "upgrade_task_id": &upgrade_task_id,
        "component_type": "bmc",
        "target_version": "1.2.3",
        "script": {
            "url": "http://pxe/scripts/upgrade.sh",
            "sha256": "script-sha",
        },
        "execution_timeout_seconds": 30,
        "artifact_download_timeout_seconds": 10,
        "file_artifacts": [{
            "url": "http://pxe/firmware.bin",
            "sha256": "firmware-sha",
        }],
    })
    .to_string();

    let state = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
            upgrade_task_id,
            firmware_type: FirmwareComponentType::Bmc,
            final_version: "1.2.3".to_string(),
            power_drains_needed: None,
            started_at: chrono::Utc::now(),
            deadline: chrono::Utc::now() + chrono::TimeDelta::minutes(60),
            task_json,
            result: None,
        },
        retry_count: 0,
    };

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    host.advance_state(&mut txn, state).await;
    host.clear_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    mh.host.reboot_completed().await;
    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::FirmwareUpgrade(_))));
    assert_eq!(response.legacy_action, LegacyAction::FirmwareUpgrade as i32);
}

#[sqlx_test]
async fn assigned_discovery_boot_does_not_reset_without_cleanup_timestamp(pool: PgPool) {
    let TestContext { env, mh, .. } = init(pool).await;
    let state = ManagedHostState::Assigned {
        instance_state: InstanceState::BootingWithDiscoveryImage {
            retry: RetryInfo { count: 0 },
        },
    };

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    host.advance_state(&mut txn, state).await;
    host.clear_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    assert!(host.status.last_cleanup_time.is_none());
    txn.rollback().await.unwrap();

    mh.host.reboot_completed().await;
    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(response.legacy_action, LegacyAction::Noop as i32);
}

#[sqlx_test]
async fn waiting_for_scout_upgrade_returns_task_without_cleanup_timestamp(pool: PgPool) {
    let TestContext { env, mh, .. } = init(pool).await;
    let upgrade_task_id = uuid::Uuid::new_v4().to_string();
    let task_json = serde_json::json!({
        "upgrade_task_id": &upgrade_task_id,
        "component_type": "bmc",
        "target_version": "1.2.3",
        "script": {
            "url": "http://pxe/scripts/upgrade.sh",
            "sha256": "script-sha",
        },
        "execution_timeout_seconds": 30,
        "artifact_download_timeout_seconds": 10,
        "file_artifacts": [{
            "url": "http://pxe/firmware.bin",
            "sha256": "firmware-sha",
        }],
    })
    .to_string();

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    let waiting_state = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
            upgrade_task_id: upgrade_task_id.clone(),
            firmware_type: FirmwareComponentType::Bmc,
            final_version: "1.2.3".to_string(),
            power_drains_needed: None,
            started_at: chrono::Utc::now(),
            deadline: chrono::Utc::now() + chrono::TimeDelta::minutes(60),
            task_json: task_json.clone(),
            result: None,
        },
        retry_count: 0,
    };
    host.advance_state(&mut txn, waiting_state).await;
    host.clear_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    assert!(host.status.last_cleanup_time.is_none());
    txn.rollback().await.unwrap();

    let response = mh.host.forge_agent_control().await;
    let Some(Action::FirmwareUpgrade(firmware_upgrade)) = response.action.as_ref() else {
        panic!("expected typed firmware upgrade action");
    };
    let task = firmware_upgrade.task.as_ref().expect("typed task");
    let legacy_pair = response
        .data
        .as_ref()
        .expect("legacy data")
        .pair
        .iter()
        .find(|pair| pair.key == "firmware_upgrade_task")
        .expect("legacy firmware_upgrade_task");

    assert_eq!(response.legacy_action, LegacyAction::FirmwareUpgrade as i32);
    assert_eq!(task.component_type, "bmc");
    assert_eq!(task.target_version, "1.2.3");
    assert_eq!(task.upgrade_task_id, upgrade_task_id);
    assert_eq!(
        task.script.as_ref().expect("script").url,
        "http://pxe/scripts/upgrade.sh"
    );
    assert_eq!(task.file_artifacts[0].sha256, "firmware-sha");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&legacy_pair.value).unwrap(),
        serde_json::from_str::<serde_json::Value>(&task_json).unwrap()
    );
}

#[sqlx_test]
async fn assigned_waiting_for_scout_upgrade_returns_task(pool: PgPool) {
    let TestContext { mh, .. } = init(pool).await;
    let upgrade_task_id = uuid::Uuid::new_v4().to_string();
    let task_json = serde_json::json!({
        "upgrade_task_id": &upgrade_task_id,
        "component_type": "cx7",
        "target_version": "28.47.2682",
        "script": {
            "url": "http://pxe/scripts/cx7_upgrade.sh",
            "sha256": "script-sha",
        },
        "execution_timeout_seconds": 7200,
        "artifact_download_timeout_seconds": 600,
        "file_artifacts": [{
            "url": "http://pxe/cx7.bin",
            "sha256": "firmware-sha",
        }],
    })
    .to_string();
    mh.advance_state(ManagedHostState::Assigned {
        instance_state: InstanceState::HostReprovision {
            reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
                upgrade_task_id: upgrade_task_id.clone(),
                firmware_type: FirmwareComponentType::Cx7,
                final_version: "28.47.2682".to_string(),
                power_drains_needed: Some(1),
                started_at: chrono::Utc::now(),
                deadline: chrono::Utc::now() + chrono::TimeDelta::hours(3),
                task_json,
                result: None,
            },
        },
    })
    .await;

    let response = mh.host.forge_agent_control().await;
    let Some(Action::FirmwareUpgrade(firmware_upgrade)) = response.action else {
        panic!("expected typed firmware upgrade action");
    };
    let task = firmware_upgrade.task.expect("typed task");

    assert_eq!(response.legacy_action, LegacyAction::FirmwareUpgrade as i32);
    assert_eq!(task.component_type, "cx7");
    assert_eq!(task.target_version, "28.47.2682");
    assert_eq!(task.upgrade_task_id, upgrade_task_id);
}

#[sqlx_test]
async fn invalid_json_falls_back_to_noop(pool: PgPool) {
    let TestContext { env, mh, .. } = init(pool).await;
    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    let waiting_state = ManagedHostState::HostReprovision {
        reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
            upgrade_task_id: uuid::Uuid::new_v4().to_string(),
            firmware_type: FirmwareComponentType::Bmc,
            final_version: "1.2.3".to_string(),
            power_drains_needed: None,
            started_at: chrono::Utc::now(),
            deadline: chrono::Utc::now() + chrono::TimeDelta::minutes(60),
            task_json: "{not valid json".to_string(),
            result: None,
        },
        retry_count: 0,
    };
    host.advance_state(&mut txn, waiting_state).await;
    txn.commit().await.unwrap();

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Noop);
}

#[sqlx_test]
async fn host_init_returns_retry(pool: PgPool) {
    let TestContext { env: _env, mh, .. } = init(pool).await;
    mh.advance_state(ManagedHostState::HostInit {
        machine_state: MachineState::Init,
    })
    .await;

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Retry(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Retry);
}

#[sqlx_test]
async fn waiting_for_discovery_without_cleanup_returns_retry(pool: PgPool) {
    let TestContext {
        env,
        mut mh,
        admin_segment,
    } = init(pool).await;
    mh.host.discover_primary_iface(admin_segment).await;
    mh.advance_state(ManagedHostState::HostInit {
        machine_state: MachineState::WaitingForDiscovery,
    })
    .await;

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    host.clear_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Retry(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Retry);
}

#[sqlx_test]
async fn waiting_for_discovery_after_cleanup_returns_discovery(pool: PgPool) {
    let TestContext {
        env,
        mut mh,
        admin_segment,
    } = init(pool).await;
    mh.host.discover_primary_iface(admin_segment).await;
    mh.advance_state(ManagedHostState::HostInit {
        machine_state: MachineState::WaitingForDiscovery,
    })
    .await;

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    host.update_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Discovery);
}

#[sqlx_test]
async fn waiting_for_cleanup_without_completed_cleanup_returns_reset(pool: PgPool) {
    let TestContext { env, mh, .. } = init(pool).await;
    mh.advance_state(ManagedHostState::WaitingForCleanup {
        cleanup_state: CleanupState::HostCleanup {
            boss_controller_id: None,
        },
        cleanup_context: CleanupContext::InitialDiscovery,
    })
    .await;

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    host.clear_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Reset(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Reset);
}

#[sqlx_test]
async fn waiting_for_cleanup_after_cleanup_returns_noop(pool: PgPool) {
    let TestContext { env, mh, .. } = init(pool).await;
    mh.advance_state(ManagedHostState::WaitingForCleanup {
        cleanup_state: CleanupState::HostCleanup {
            boss_controller_id: None,
        },
        cleanup_context: CleanupContext::InitialDiscovery,
    })
    .await;

    let mut txn = env.db_txn().await;
    let host = mh.host.db_machine(&mut txn).await;
    host.update_cleanup_time(&mut txn).await;
    txn.commit().await.unwrap();

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Noop);
}

#[sqlx_test]
async fn failed_discovery_returns_discovery(pool: PgPool) {
    let TestContext { env: _env, mh, .. } = init(pool).await;
    mh.advance_state(ManagedHostState::Failed {
        details: FailureDetails {
            cause: FailureCause::Discovery {
                err: "host discovery failed".to_string(),
            },
            failed_at: chrono::Utc::now(),
            source: FailureSource::Scout,
        },
        machine_id: mh.host.id.into(),
        retry_count: 0,
    })
    .await;

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Discovery(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Discovery);
}

#[sqlx_test]
async fn waiting_for_measurements_returns_measure(pool: PgPool) {
    let TestContext { env: _env, mh, .. } = init(pool).await;
    mh.advance_state(ManagedHostState::Measuring {
        measuring_state: MeasuringState::WaitingForMeasurements,
    })
    .await;

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Measure(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Measure);
}

#[sqlx_test]
async fn ready_returns_noop(pool: PgPool) {
    let TestContext { env: _env, mh, .. } = init(pool).await;
    mh.advance_to_converged_ready().await;

    let response = mh.host.forge_agent_control().await;
    assert!(matches!(response.action, Some(Action::Noop(_))));
    assert_eq!(response.legacy_action(), LegacyAction::Noop);
}
