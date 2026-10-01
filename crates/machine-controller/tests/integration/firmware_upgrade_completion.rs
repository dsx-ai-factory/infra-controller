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
use carbide_machine_controller::handler::MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES;
use carbide_test_harness::CarbideConfig;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::firmware::FirmwareComponentType;
use model::machine::{
    HostReprovisionState, MAX_FIRMWARE_UPGRADE_RETRIES, ManagedHostState, ScoutUpgradeResult,
};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

struct TestContext {
    env: Env,
    mh: TestManagedHost,
}

impl TestContext {
    async fn init(pool: PgPool) -> Self {
        Self::init_with_runtime(pool, |_| {}).await
    }

    async fn init_with_runtime(pool: PgPool, configure: impl FnOnce(&mut CarbideConfig)) -> Self {
        let env = Env::builder(pool)
            .configure_runtime(configure)
            .build()
            .await;
        let domain = env.test_harness.test_domain().await;
        let network_controller = env.test_harness.network_controller();
        let underlay_segment = network_controller.create_underlay_segment(&domain).await;
        network_controller.create_admin_segment(&domain).await;
        let site_explorer = env.test_harness.default_test_site_explorer();
        let mh = env
            .test_harness
            .managed_host_builder(&site_explorer, underlay_segment)
            .with_config(ManagedHostConfig::default())
            .build()
            .await
            .0;
        Self { env, mh }
    }
}

trait TestManagedHostFirmwareExt {
    async fn put_in_waiting_for_scout_upgrade(
        &self,
        deadline: chrono::DateTime<chrono::Utc>,
        power_drains_needed: Option<u32>,
        result: Option<ScoutUpgradeResult>,
    );

    async fn put_in_new_firmware_reported_wait(
        &self,
        previous_reset_time: i64,
        reset_retry_count: u32,
    );

    async fn put_in_failed_firmware_upgrade(
        &self,
        report_time: chrono::DateTime<chrono::Utc>,
        retry_count: u32,
    );
}

impl TestManagedHostFirmwareExt for TestManagedHost {
    async fn put_in_waiting_for_scout_upgrade(
        &self,
        deadline: chrono::DateTime<chrono::Utc>,
        power_drains_needed: Option<u32>,
        result: Option<ScoutUpgradeResult>,
    ) {
        self.advance_state(ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::WaitingForScoutUpgrade {
                upgrade_task_id: "scout-upgrade-task-id".to_string(),
                firmware_type: FirmwareComponentType::Bmc,
                final_version: "1.2.3".to_string(),
                power_drains_needed,
                started_at: chrono::Utc::now(),
                deadline,
                task_json: String::new(),
                result,
            },
            retry_count: 0,
        })
        .await;
    }

    async fn put_in_new_firmware_reported_wait(
        &self,
        previous_reset_time: i64,
        reset_retry_count: u32,
    ) {
        self.advance_state(ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::NewFirmwareReportedWait {
                firmware_type: FirmwareComponentType::Uefi,
                firmware_number: Some(0),
                final_version: "1.13.2".to_string(),
                previous_reset_time: Some(previous_reset_time),
                reset_retry_count,
            },
            retry_count: 0,
        })
        .await;
    }

    async fn put_in_failed_firmware_upgrade(
        &self,
        report_time: chrono::DateTime<chrono::Utc>,
        retry_count: u32,
    ) {
        self.advance_state(ManagedHostState::HostReprovision {
            reprovision_state: HostReprovisionState::FailedFirmwareUpgrade {
                firmware_type: FirmwareComponentType::Bmc,
                report_time: Some(report_time),
                reason: Some("scout upgrade failed".to_string()),
            },
            retry_count,
        })
        .await;
    }
}

#[sqlx_test]
async fn new_firmware_reported_wait_retries_reset_after_timeout(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_new_firmware_reported_wait(chrono::Utc::now().timestamp() - 31 * 60, 0)
        .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::NewFirmwareReportedWait {
        reset_retry_count, ..
    } = reprovision_state
    else {
        panic!("expected NewFirmwareReportedWait, got {reprovision_state:?}");
    };
    assert_eq!(*reset_retry_count, 1);
}

#[sqlx_test]
async fn new_firmware_reported_wait_fails_after_reset_retry_limit(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_new_firmware_reported_wait(
        chrono::Utc::now().timestamp() - 31 * 60,
        MAX_NEW_FIRMWARE_REPORTED_RESET_RETRIES,
    )
    .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    let reason = reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("firmware version did not converge after completed update"),
        "unexpected reason: {reason}",
    );
    assert!(
        reason.contains("expected 1.13.2"),
        "unexpected reason: {reason}",
    );
    assert!(reason.contains("1.12.0"), "unexpected reason: {reason}");
}

#[sqlx_test]
async fn successful_scout_upgrade_transitions_to_reset_for_new_firmware(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_waiting_for_scout_upgrade(
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        Some(1),
        Some(ScoutUpgradeResult {
            success: true,
            exit_code: 0,
            stdout: "ok".to_string(),
            stderr: String::new(),
            error: String::new(),
        }),
    )
    .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::ResetForNewFirmware {
        power_drains_needed,
        ..
    } = reprovision_state
    else {
        panic!("expected ResetForNewFirmware, got {reprovision_state:?}");
    };
    assert_eq!(*power_drains_needed, Some(1));
}

#[sqlx_test]
async fn failed_scout_upgrade_uses_error_as_reason(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_waiting_for_scout_upgrade(
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        None,
        Some(ScoutUpgradeResult {
            success: false,
            exit_code: 1,
            stdout: String::new(),
            stderr: String::new(),
            error: "boom".to_string(),
        }),
    )
    .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    assert_eq!(reason.as_deref(), Some("boom"));
}

#[sqlx_test]
async fn failed_scout_upgrade_without_error_uses_exit_code(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_waiting_for_scout_upgrade(
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        None,
        Some(ScoutUpgradeResult {
            success: false,
            exit_code: 7,
            stdout: String::new(),
            stderr: String::new(),
            error: String::new(),
        }),
    )
    .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    assert_eq!(
        reason.as_deref(),
        Some("scout upgrade failed with exit code 7"),
    );
}

#[sqlx_test]
async fn scout_upgrade_past_deadline_times_out(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_waiting_for_scout_upgrade(
        chrono::Utc::now() - chrono::TimeDelta::minutes(1),
        None,
        None,
    )
    .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    let HostReprovisionState::FailedFirmwareUpgrade { reason, .. } = reprovision_state else {
        panic!("expected FailedFirmwareUpgrade, got {reprovision_state:?}");
    };
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.starts_with("scout firmware upgrade timed out")),
        "unexpected reason: {reason:?}",
    );
}

#[sqlx_test]
async fn scout_upgrade_before_deadline_waits(pool: PgPool) {
    let TestContext { mut env, mh } = TestContext::init(pool).await;
    mh.put_in_waiting_for_scout_upgrade(
        chrono::Utc::now() + chrono::TimeDelta::minutes(60),
        None,
        None,
    )
    .await;

    env.run_single_iteration().await;

    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state, ..
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    assert!(
        matches!(
            reprovision_state,
            HostReprovisionState::WaitingForScoutUpgrade { result: None, .. }
        ),
        "expected to remain in WaitingForScoutUpgrade, got {reprovision_state:?}",
    );
}

/// Drives the `FailedFirmwareUpgrade` arm through all three of its retry
/// outcomes: inside the retry interval nothing moves, past the interval one
/// retry is consumed and counted, and with the budget exhausted the machine
/// parks in the failure state without another count. The log side of the
/// retry emit is covered by the unit test next to the event declaration.
#[sqlx_test]
async fn failed_firmware_upgrade_retries_until_the_budget_is_exhausted(pool: PgPool) {
    const RETRIES_TOTAL: &str = "carbide_host_reprovision_retries_total";
    let retry_interval = chrono::TimeDelta::hours(1);
    let TestContext { mut env, mh } = TestContext::init_with_runtime(pool, |config| {
        config.firmware_global.host_firmware_upgrade_retry_interval = retry_interval;
    })
    .await;
    let metrics = MetricsCapture::start();

    // Still inside the retry interval: the machine stays put and nothing is
    // counted.
    mh.put_in_failed_firmware_upgrade(chrono::Utc::now(), 0)
        .await;
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state,
        retry_count,
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    assert!(
        matches!(
            reprovision_state,
            HostReprovisionState::FailedFirmwareUpgrade { .. }
        ),
        "expected to remain in FailedFirmwareUpgrade, got {reprovision_state:?}",
    );
    assert_eq!(*retry_count, 0);
    assert_eq!(metrics.counter_delta(RETRIES_TOTAL, &[]), 0.0);

    // Past the retry interval with budget left: one retry is taken and
    // counted, and the consumed attempt lands in the state's retry_count.
    mh.put_in_failed_firmware_upgrade(chrono::Utc::now() - retry_interval * 2, 0)
        .await;
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state,
        retry_count,
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    assert!(
        matches!(
            reprovision_state,
            HostReprovisionState::CheckingFirmwareV2 { .. }
        ),
        "expected CheckingFirmwareV2, got {reprovision_state:?}",
    );
    assert_eq!(*retry_count, 1);
    assert_eq!(metrics.counter_delta(RETRIES_TOTAL, &[]), 1.0);

    // Past the retry interval with the budget exhausted: the machine parks in
    // the failure state and the counter does not move again.
    mh.put_in_failed_firmware_upgrade(
        chrono::Utc::now() - retry_interval * 2,
        MAX_FIRMWARE_UPGRADE_RETRIES,
    )
    .await;
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    let ManagedHostState::HostReprovision {
        reprovision_state,
        retry_count,
    } = machine.current_state()
    else {
        panic!("Not in HostReprovision");
    };
    assert!(
        matches!(
            reprovision_state,
            HostReprovisionState::FailedFirmwareUpgrade { .. }
        ),
        "expected to remain in FailedFirmwareUpgrade, got {reprovision_state:?}",
    );
    assert_eq!(*retry_count, MAX_FIRMWARE_UPGRADE_RETRIES);
    assert_eq!(metrics.counter_delta(RETRIES_TOTAL, &[]), 1.0);
}

/// A cold artifact cache defers the upload. The controller must retain the
/// original request across subsequent checks instead of requiring the update
/// manager to issue another request five minutes later.
#[sqlx_test]
async fn cold_firmware_download_preserves_request_until_check_completes(pool: PgPool) {
    let cache = tempfile::tempdir().unwrap();
    let TestContext { mut env, mh } = TestContext::init_with_runtime(pool, |config| {
        config.firmware_global.firmware_download_cache_directory = cache.path().to_owned();
        // Isolate this BMC check from the default fixture's outdated UEFI
        // catalog. Runtime overrides merge with static components rather than
        // replacing them, so merely specifying ordering=[bmc] is insufficient.
        config.host_models.clear();
        config.firmware_global.firmware_directory = cache.path().to_owned();
    })
    .await;
    // The builder stops at discovery. Establish verified boot intent and
    // Ready state so handling reaches the firmware request gate.
    mh.advance_to_converged_ready().await;
    assert_eq!(
        mh.host.machine().await.current_state(),
        &ManagedHostState::Ready
    );

    // Keep a local listener open without answering TLS. This deterministically
    // holds the background download pending, with no external network service.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://{}/firmware.bin", listener.local_addr().unwrap());
    let mut config: model::firmware::HostFirmwareConfig = serde_json::from_value(
        serde_json::json!({
            "vendor": "Dell",
            "model": "PowerEdge R750",
            "ordering": ["bmc"],
            "components": {"bmc": {
                "current_version_reported_as": "^Installed-__iDRACz$",
                "known_firmware": [{
                    "version": "99.5361.1",
                    "default": true,
                    "files": [{"url": url, "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}]
                }]
            }}
        }),
    ).unwrap();
    let mut txn = env.test_harness.db_txn().await;
    db::host_firmware_config::upsert(&mut txn, &config)
        .await
        .unwrap();
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut txn,
        "Automated",
        &mh.host.id,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let requested_at = mh
        .host
        .machine()
        .await
        .host_reprovision_requested
        .unwrap()
        .requested_at;

    for _ in 0..2 {
        env.run_single_iteration().await;
        let machine = mh.host.machine().await;
        assert_eq!(machine.current_state(), &ManagedHostState::Ready);
        assert_eq!(
            machine
                .host_reprovision_requested
                .as_ref()
                .map(|request| request.requested_at),
            Some(requested_at),
            "deferred download must retain the original request",
        );
    }
    // The download really was attempted, rather than an unrelated Ready guard
    // bypassing the firmware check.
    let (_connection, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();

    // An explicit completed check still clears the request. Use the fixture's
    // installed version so this tests completion without a simulated flash.
    config
        .components
        .get_mut(&FirmwareComponentType::Bmc)
        .unwrap()
        .known_firmware[0]
        .version = "5.10.20".to_string();
    let mut txn = env.test_harness.db_txn().await;
    db::host_firmware_config::upsert(&mut txn, &config)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert!(machine.host_reprovision_requested.is_none());
    assert!(
        matches!(
            machine.current_state(),
            ManagedHostState::HostInit {
                machine_state: model::machine::MachineState::WaitingForLockdown { .. },
            }
        ),
        "completed firmware check must hand the unlocked fixture back to lockdown"
    );

    // A host that is already locked down completes directly into Ready and
    // must also clear its request, without entering the lockdown handoff.
    env.redfish_sim
        .set_lockdown(libredfish::EnabledDisabled::Enabled);
    mh.advance_to_converged_ready().await;
    let mut txn = env.test_harness.db_txn().await;
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut txn,
        "Automated",
        &mh.host.id,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    env.run_single_iteration().await;
    let machine = mh.host.machine().await;
    assert_eq!(machine.current_state(), &ManagedHostState::Ready);
    assert!(machine.host_reprovision_requested.is_none());
}
