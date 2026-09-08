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

//! Releasing the DPF maintenance hold so a changed DPUService rolls out.
//!
//! DPF parks a DPU in `NodeEffect` when its services change, and carbide opens
//! that gate only for a DPU already running the software its DPUDeployment
//! declares. These cover the four outcomes: released, deferred because the DPU
//! is outdated, deferred because the release itself failed, and not attempted
//! at all because nothing is pending.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use carbide_dpf::{DpfError, DpuDeploymentType, DpuPhase};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_uuid::machine::{DpuMachineId, HostMachineId};
use model::machine::{DpfState, DpuInitState, DpuInitStates, ManagedHostState};
use model::machine_pending_action::MachinePendingActionKind::DpuServiceSync;
use tokio::time::timeout;

use super::dpf_config;
use crate::tests::common::api_fixtures::test_managed_host::TestManagedHost;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host_with_dpf, create_test_env_with_overrides,
    get_config,
};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Counters for the two calls this handler is responsible for, so a test can
/// assert what happened *after* provisioning rather than in total.
#[derive(Default)]
pub(super) struct Calls {
    pub(super) outdated_checks: AtomicUsize,
    pub(super) hold_releases: AtomicUsize,
}

/// `outdated` drives what `is_dpu_outdated` reports; `release_fails` makes the
/// hold release return an error so the marker's survival can be checked.
pub(super) fn mock(
    calls: Arc<Calls>,
    outdated: Arc<AtomicBool>,
    release_fails: Arc<AtomicBool>,
) -> MockDpfOperations {
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device().returning(|_, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase()
        .returning(|_, _| Ok(DpuPhase::Ready));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));

    let outdated_calls = calls.clone();
    mock.expect_is_dpu_outdated().returning(move |_| {
        outdated_calls
            .outdated_checks
            .fetch_add(1, Ordering::SeqCst);
        Ok(outdated.load(Ordering::SeqCst))
    });

    mock.expect_release_maintenance_hold().returning(move |_| {
        calls.hold_releases.fetch_add(1, Ordering::SeqCst);
        if release_fails.load(Ordering::SeqCst) {
            Err(DpfError::InvalidState("release refused".to_string()))
        } else {
            Ok(())
        }
    });
    mock
}

pub(super) struct Fixture {
    pub(super) env: TestEnv,
    pub(super) mh: TestManagedHost,
    pub(super) calls: Arc<Calls>,
    pub(super) pool: sqlx::PgPool,
}

/// Provisions a DPF host and returns it sitting in `Ready`, with the call
/// counters zeroed so only post-provisioning activity is measured.
pub(super) async fn provisioned(
    pool: sqlx::PgPool,
    outdated: Arc<AtomicBool>,
    release_fails: Arc<AtomicBool>,
) -> Fixture {
    provisioned_with_sync(pool, outdated, release_fails, true).await
}

pub(super) async fn provisioned_with_sync(
    pool: sqlx::PgPool,
    outdated: Arc<AtomicBool>,
    release_fails: Arc<AtomicBool>,
    sync_enabled: bool,
) -> Fixture {
    let calls = Arc::new(Calls::default());
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(mock(calls.clone(), outdated, release_fails));

    let mut config = get_config();
    config.dpf = dpf_config();
    config.dpf.dpu_service_sync_enabled = sync_enabled;

    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await;

    let mh = timeout(TEST_TIMEOUT, create_managed_host_with_dpf(&env))
        .await
        .expect("timed out during initial provisioning");

    calls.outdated_checks.store(0, Ordering::SeqCst);
    calls.hold_releases.store(0, Ordering::SeqCst);

    Fixture {
        env,
        mh,
        calls,
        pool,
    }
}

pub(super) async fn request_sync(pool: &sqlx::PgPool, host_id: &HostMachineId) {
    let mut conn = pool.acquire().await.unwrap();
    db::machine_pending_action::request(&mut conn, host_id, DpuServiceSync)
        .await
        .expect("recorded pending action");
}

pub(super) async fn is_outstanding(pool: &sqlx::PgPool, host_id: &HostMachineId) -> bool {
    db::machine_pending_action::is_outstanding(pool, host_id, DpuServiceSync)
        .await
        .expect("read pending action")
}

/// Puts one DPU back into the DPF `WaitingForReady` substate so the provisioning
/// handler runs again without replaying the whole operator workflow.
pub(super) async fn reset_host_to_waiting_for_ready(
    pool: &sqlx::PgPool,
    host_id: &HostMachineId,
    dpu_id: &DpuMachineId,
) {
    let state = ManagedHostState::DPUInit {
        dpu_states: DpuInitStates {
            states: HashMap::from([(
                *dpu_id,
                DpuInitState::DpfStates {
                    state: DpfState::WaitingForReady { phase_detail: None },
                },
            )]),
        },
    };
    let version = format!("V999-T{}", chrono::Utc::now().timestamp_micros());

    sqlx::query(
        "UPDATE machines SET \
            controller_state = $1, \
            controller_state_version = $2, \
            controller_state_outcome = NULL, \
            health_reports = '{\"merges\": {}, \"replace\": null}'::jsonb \
         WHERE id = $3",
    )
    .bind(sqlx::types::Json(serde_json::to_value(&state).unwrap()))
    .bind(&version)
    .bind(host_id)
    .execute(pool)
    .await
    .unwrap();
}

/// A host whose DPUs all match their deployment gets its hold released, and the
/// pending action is recorded as completed.
#[crate::sqlx_test]
async fn hold_is_released_when_every_dpu_is_current(pool: sqlx::PgPool) {
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    timeout(
        TEST_TIMEOUT,
        fixture.env.run_machine_state_controller_iteration(),
    )
    .await
    .expect("timed out during state controller iteration");

    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        1,
        "an up-to-date host should have its maintenance hold released"
    );
    assert!(
        !is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "a successful release must complete the pending action"
    );
}

/// A DPU still awaiting reprovisioning keeps its hold, and the pending action
/// stays outstanding so a later sweep can retry once the DPU is current.
#[crate::sqlx_test]
async fn hold_is_kept_while_a_dpu_is_outdated(pool: sqlx::PgPool) {
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(true)),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    timeout(
        TEST_TIMEOUT,
        fixture.env.run_machine_state_controller_iteration(),
    )
    .await
    .expect("timed out during state controller iteration");

    assert!(
        fixture.calls.outdated_checks.load(Ordering::SeqCst) > 0,
        "the handler should have evaluated the host's DPUs"
    );
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        0,
        "an outdated DPU must keep its hold; reprovisioning owns it"
    );
    assert!(
        is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "deferring must not consume the pending action"
    );
}

/// A release that fails leaves the pending action outstanding. Completing it
/// here would strand the host: a DPU already in `NodeEffect` does not re-enter
/// it, so the watcher never re-fires and the signal would be lost.
#[crate::sqlx_test]
async fn a_failed_release_leaves_the_action_outstanding(pool: sqlx::PgPool) {
    // Provisioning releases its own hold on the way to Ready, so the failure is
    // armed only once the host has arrived there.
    let release_fails = Arc::new(AtomicBool::new(false));
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(false)),
        release_fails.clone(),
    )
    .await;
    release_fails.store(true, Ordering::SeqCst);
    request_sync(&fixture.pool, &fixture.mh.id).await;

    timeout(
        TEST_TIMEOUT,
        fixture.env.run_machine_state_controller_iteration(),
    )
    .await
    .expect("timed out during state controller iteration");

    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        1,
        "the release should have been attempted"
    );
    assert!(
        is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "a failed release must not complete the pending action"
    );
}

/// Provisioning releases the node's hold itself, which satisfies whatever the
/// pending action was asking for. It retires the action there so the host does
/// not arrive in `Ready` carrying a marker for work already done.
///
/// The watcher cannot make this distinction on its own: it fires on the
/// `NodeEffect` phase, which says a DPU is parked but not why, so a provisioning
/// pass records a marker exactly as a DPUService change does.
#[crate::sqlx_test]
async fn provisioning_retires_the_pending_action_when_it_releases_the_hold(pool: sqlx::PgPool) {
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
    .await;
    request_sync(&fixture.pool, &fixture.mh.id).await;
    reset_host_to_waiting_for_ready(&fixture.pool, &fixture.mh.id, &fixture.mh.dpu_ids[0]).await;

    timeout(
        TEST_TIMEOUT,
        fixture.env.run_machine_state_controller_iteration(),
    )
    .await
    .expect("timed out during state controller iteration");

    assert!(
        !is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "the provisioning release must retire the pending action"
    );
    assert_eq!(
        fixture.calls.outdated_checks.load(Ordering::SeqCst),
        0,
        "provisioning retires the action without consulting Kubernetes about DPU currency"
    );
}

/// With the site switch off, carbide never opens the gate itself. The hold and
/// the pending action both survive, because the rollout is then an operator's to
/// drive rather than something to abandon.
#[crate::sqlx_test]
async fn the_site_switch_stops_carbide_releasing_the_hold(pool: sqlx::PgPool) {
    let fixture = provisioned_with_sync(
        pool,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
        false,
    )
    .await;
    request_sync(&fixture.pool, &fixture.mh.id).await;

    timeout(
        TEST_TIMEOUT,
        fixture.env.run_machine_state_controller_iteration(),
    )
    .await
    .expect("timed out during state controller iteration");

    assert_eq!(
        fixture.calls.outdated_checks.load(Ordering::SeqCst),
        0,
        "a disabled site should not even inspect its DPUs"
    );
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        0,
        "a disabled site must not release the hold"
    );
    assert!(
        is_outstanding(&fixture.pool, &fixture.mh.id).await,
        "the pending action must survive so an operator can still act on it"
    );
}

/// Without a pending action the handler makes no Kubernetes calls at all. This
/// is what keeps ordinary Ready sweeps off the API server, so it is the load
/// property worth pinning down rather than an incidental detail.
#[crate::sqlx_test]
async fn nothing_is_checked_without_a_pending_action(pool: sqlx::PgPool) {
    let fixture = provisioned(
        pool,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    )
    .await;

    timeout(
        TEST_TIMEOUT,
        fixture.env.run_machine_state_controller_iteration(),
    )
    .await
    .expect("timed out during state controller iteration");

    assert_eq!(
        fixture.calls.outdated_checks.load(Ordering::SeqCst),
        0,
        "no marker means no DPU should be inspected"
    );
    assert_eq!(
        fixture.calls.hold_releases.load(Ordering::SeqCst),
        0,
        "no marker means no hold should be released"
    );
}
