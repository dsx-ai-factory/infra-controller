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

use carbide_instrument::testing::MetricsCapture;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::firmware::FirmwareComponentType;
use model::machine::{HostReprovisionState, InstanceState, ManagedHostState};
use model::test_support::ManagedHostConfig;
use tonic::Request;

struct TestContext {
    env: TestHarness,
    mh: TestManagedHost,
}

async fn init(pool: PgPool) -> TestContext {
    let env = TestHarness::builder(pool).build().await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let mh = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;
    TestContext { env, mh }
}

fn waiting_reprovision_state(upgrade_task_id: &str) -> HostReprovisionState {
    HostReprovisionState::WaitingForScoutUpgrade {
        upgrade_task_id: upgrade_task_id.to_string(),
        firmware_type: FirmwareComponentType::Bmc,
        final_version: "1.2.3".to_string(),
        power_drains_needed: None,
        started_at: chrono::Utc::now(),
        deadline: chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        delivered_at: None,
        task_json: String::new(),
        result: None,
    }
}

fn waiting_state(upgrade_task_id: &str) -> ManagedHostState {
    ManagedHostState::HostReprovision {
        reprovision_state: waiting_reprovision_state(upgrade_task_id),
        retry_count: 0,
    }
}

fn assigned_waiting_state(upgrade_task_id: &str) -> ManagedHostState {
    ManagedHostState::Assigned {
        instance_state: InstanceState::HostReprovision {
            reprovision_state: waiting_reprovision_state(upgrade_task_id),
        },
    }
}

fn status_request(
    mh: &TestManagedHost,
    upgrade_task_id: &str,
) -> rpc::forge::ScoutFirmwareUpgradeStatusRequest {
    rpc::forge::ScoutFirmwareUpgradeStatusRequest {
        machine_id: Some(mh.host.id),
        success: true,
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
        error: String::new(),
        upgrade_task_id: upgrade_task_id.to_string(),
    }
}

#[sqlx_test]
async fn stores_successful_result(pool: PgPool) {
    const UPGRADE_TASK_ID: &str = "scout-upgrade-task-id";

    let TestContext { env, mh } = init(pool).await;
    mh.advance_state(waiting_state(UPGRADE_TASK_ID)).await;

    env.api()
        .report_scout_firmware_upgrade_status(Request::new(
            rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                stdout: "upgrade complete".to_string(),
                ..status_request(&mh, UPGRADE_TASK_ID)
            },
        ))
        .await
        .unwrap();

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScoutUpgrade {
        result,
        delivered_at,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForScoutUpgrade");
    };
    assert!(delivered_at.is_none());
    let result = result.as_ref().expect("result should be set");
    assert!(result.success);
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "upgrade complete");
}

#[sqlx_test]
async fn stores_successful_result_for_assigned_host(pool: PgPool) {
    const UPGRADE_TASK_ID: &str = "assigned-scout-upgrade-task-id";

    let TestContext { env, mh } = init(pool).await;
    mh.advance_state(assigned_waiting_state(UPGRADE_TASK_ID))
        .await;

    env.api()
        .report_scout_firmware_upgrade_status(Request::new(
            rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                stdout: "assigned upgrade complete".to_string(),
                ..status_request(&mh, UPGRADE_TASK_ID)
            },
        ))
        .await
        .unwrap();

    let machine = mh.host.machine().await;
    let ManagedHostState::Assigned {
        instance_state: InstanceState::HostReprovision { reprovision_state },
    } = machine.current_state()
    else {
        panic!("assigned host reprovision wrapper was not preserved");
    };
    let HostReprovisionState::WaitingForScoutUpgrade {
        result,
        delivered_at,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForScoutUpgrade");
    };
    assert!(delivered_at.is_none());
    let result = result.as_ref().expect("result should be set");
    assert!(result.success);
    assert_eq!(result.stdout, "assigned upgrade complete");
}

#[sqlx_test]
async fn stores_failed_result(pool: PgPool) {
    const UPGRADE_TASK_ID: &str = "scout-upgrade-task-id";

    let TestContext { env, mh } = init(pool).await;
    mh.advance_state(waiting_state(UPGRADE_TASK_ID)).await;

    env.api()
        .report_scout_firmware_upgrade_status(Request::new(
            rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                success: false,
                exit_code: 1,
                stdout: "starting upgrade".to_string(),
                stderr: "permission denied".to_string(),
                error: "script failed".to_string(),
                ..status_request(&mh, UPGRADE_TASK_ID)
            },
        ))
        .await
        .unwrap();

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScoutUpgrade {
        result,
        delivered_at,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForScoutUpgrade");
    };
    assert!(delivered_at.is_none());
    let result = result.as_ref().expect("result should be set");
    assert!(!result.success);
    assert_eq!(result.exit_code, 1);
    assert_eq!(result.stderr, "permission denied");
    assert_eq!(result.error, "script failed");
}

#[sqlx_test]
async fn rejects_result_in_wrong_state(pool: PgPool) {
    let TestContext { env, mh } = init(pool).await;

    let err = env
        .api()
        .report_scout_firmware_upgrade_status(Request::new(status_request(
            &mh,
            "scout-upgrade-task-id",
        )))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
}

#[sqlx_test]
async fn rejects_stale_task_id(pool: PgPool) {
    const CURRENT_TASK_ID: &str = "current-scout-upgrade-task-id";
    const STALE_TASK_ID: &str = "stale-scout-upgrade-task-id";

    let TestContext { env, mh } = init(pool).await;
    mh.advance_state(waiting_state(CURRENT_TASK_ID)).await;

    let err = env
        .api()
        .report_scout_firmware_upgrade_status(Request::new(status_request(&mh, STALE_TASK_ID)))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::FailedPrecondition);

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScoutUpgrade {
        upgrade_task_id,
        result,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForScoutUpgrade");
    };
    assert_eq!(upgrade_task_id, CURRENT_TASK_ID);
    assert!(result.is_none());
}

#[sqlx_test]
async fn counts_failed_state_handler_wakeup(pool: PgPool) {
    const UPGRADE_TASK_ID: &str = "scout-upgrade-task-id";

    let TestContext { env, mh } = init(pool.clone()).await;
    mh.advance_state(waiting_state(UPGRADE_TASK_ID)).await;

    // Remove the state-handler queue table so the post-commit wakeup enqueue
    // fails while the report path itself stays healthy.
    sqlx::query("DROP TABLE machine_state_controller_queued_objects")
        .execute(&pool)
        .await
        .unwrap();

    let metrics = MetricsCapture::start();
    env.api()
        .report_scout_firmware_upgrade_status(Request::new(status_request(&mh, UPGRADE_TASK_ID)))
        .await
        .expect("a failed state-handler wakeup must not fail the status report");

    assert_eq!(
        metrics.counter_delta(
            "carbide_state_handler_wakeup_failures_total",
            &[("trigger", "scout_firmware_upgrade_status")],
        ),
        1.0
    );
}

#[sqlx_test]
async fn truncates_result_output(pool: PgPool) {
    const UPGRADE_TASK_ID: &str = "scout-upgrade-task-id";

    let TestContext { env, mh } = init(pool).await;
    mh.advance_state(waiting_state(UPGRADE_TASK_ID)).await;

    let large_output = "x".repeat(10_000);
    env.api()
        .report_scout_firmware_upgrade_status(Request::new(
            rpc::forge::ScoutFirmwareUpgradeStatusRequest {
                stdout: large_output.clone(),
                stderr: large_output.clone(),
                error: large_output,
                ..status_request(&mh, UPGRADE_TASK_ID)
            },
        ))
        .await
        .unwrap();

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::WaitingForScoutUpgrade {
        result,
        delivered_at,
        ..
    } = reprovision_state
    else {
        panic!("Not in WaitingForScoutUpgrade");
    };
    assert!(delivered_at.is_none());
    let result = result.as_ref().expect("result should be set");
    assert!(result.stdout.len() <= 1500);
    assert!(result.stderr.len() <= 1500);
    assert!(result.error.len() <= 1500);
}

#[sqlx_test]
async fn concurrent_completion_preserves_result_against_stale_poll(pool: PgPool) {
    let TestContext { env, mh } = init(pool.clone()).await;
    let task = rpc::scout_firmware_upgrade::ScoutFirmwareUpgradeTask {
        upgrade_task_id: "concurrent-task".into(),
        ..Default::default()
    };
    let mut waiting = waiting_reprovision_state(&task.upgrade_task_id);
    if let HostReprovisionState::WaitingForScoutUpgrade { task_json, .. } = &mut waiting {
        *task_json = serde_json::to_string(&task).unwrap();
    }
    mh.advance_state(ManagedHostState::HostReprovision {
        retry_count: 0,
        reprovision_state: waiting.clone(),
    })
    .await;
    let before = mh.host.machine().await;
    if let HostReprovisionState::WaitingForScoutUpgrade { result, .. } = &mut waiting {
        *result = Some(model::machine::ScoutUpgradeResult {
            success: true,
            exit_code: 0,
            stdout: "completed".into(),
            stderr: String::new(),
            error: String::new(),
        });
    }
    let completed = ManagedHostState::HostReprovision {
        retry_count: 0,
        reprovision_state: waiting,
    };
    let mut txn = pool.begin().await.unwrap();
    let host_id = mh.host.id.try_into().unwrap();
    let write = db::machine::try_update_controller_state(
        &mut txn,
        &host_id,
        before.state.version,
        before.state.version.increment(),
        &completed,
    )
    .await
    .unwrap();
    assert_eq!(write, db::ConditionalWrite::Applied(()));

    // The poll reads the committed pending state, then waits behind the completion
    // writer's history lock. Committing that writer makes the poll's version stale.
    let api = env.api_arc();
    let machine_id = mh.host.id;
    let poll = tokio::spawn(async move {
        api.forge_agent_control(Request::new(rpc::forge::ForgeAgentControlRequest {
            machine_id: Some(machine_id),
        }))
        .await
    });
    let waiting_for_lock = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                 AND query LIKE 'INSERT INTO machine_state_history%')",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    txn.commit().await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), poll)
        .await
        .expect("poll should finish after completion commits")
        .unwrap();
    waiting_for_lock.expect("poll should wait behind the completion writer");
    assert_eq!(result.unwrap_err().code(), tonic::Code::Aborted);
    let after = mh.host.machine().await;
    assert_eq!(after.current_state(), &completed);
    assert_eq!(
        after.status.last_scout_contact_time,
        before.status.last_scout_contact_time
    );
}
