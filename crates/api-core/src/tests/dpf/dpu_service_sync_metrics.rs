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

//! The gauges for outstanding DPU service sync holds, read from the database.
//!
//! How holds are summarized and exported is unit-tested next to the monitor.
//! What needs a database is the read itself: which rows count, where a host's
//! state, tenancy and SLA waiver come from, what a failed read leaves behind,
//! and what the cap on waiver checks does to a host beyond it.

use carbide_instrument::testing::capture_logs_async;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType, PredictedHostMachineId};
use db::TransactionVending as _;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthReport, HealthReportApplyMode,
};
use model::machine::slas::MachineSlaConfig;
use model::machine::{InstanceState, MachineState, ManagedHostState, RetryInfo};
use model::machine_pending_action::MachinePendingActionActor;
use model::machine_pending_action::MachinePendingActionKind::DpuServiceSync;

use super::dpu_service_sync::request_sync;
use crate::dpu_service_sync_metrics::DpuServiceSyncHoldMonitor;
use crate::tests::common::api_fixtures::{TestEnv, create_managed_host, create_test_env};

const PENDING_COUNT: &str = "carbide_hosts_dpu_service_sync_pending_count";
const OLDEST_AGE: &str = "carbide_hosts_dpu_service_sync_oldest_pending_age_seconds";

/// How far back the idle host's hold is dated: long enough to tell from a
/// fresh hold.
const TWO_HOURS: chrono::Duration = chrono::Duration::hours(2);

/// How far back the idle host's entry into `HostInit` is dated: past the 30
/// minute `HostInit` SLA, and not the age of its hold, so that an age read
/// from the state entry is told from one read from the hold.
const ONE_HOUR: chrono::Duration = chrono::Duration::hours(1);

/// A gauge's four samples, indexed by `above_sla`, then by `in_use`.
fn samples(env: &TestEnv, gauge: &str) -> [[u64; 2]; 2] {
    let exported = env.test_meter.parsed_metrics(gauge);
    let sample = |above_sla: bool, in_use: bool| {
        let labels = format!(r#"{{above_sla="{above_sla}",in_use="{in_use}"}}"#);
        exported
            .iter()
            .find(|(exported_labels, _)| *exported_labels == labels)
            .unwrap_or_else(|| panic!("{gauge}{labels} should be exported"))
            .1
            .parse()
            .expect("gauge samples are integers")
    };
    [
        [sample(false, false), sample(false, true)],
        [sample(true, false), sample(true, true)],
    ]
}

/// The server process behind a pool's only connection.
async fn backend_pid(pool: &sqlx::PgPool) -> i32 {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn complete_sync(pool: &sqlx::PgPool, host_id: &MachineId) {
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        db::machine_pending_action::complete(
            &mut conn,
            host_id,
            DpuServiceSync,
            MachinePendingActionActor::AdminCli,
        )
        .await
        .expect("completed pending action")
    );
}

/// Moves a host into `state`, dated `entered_ago` in the past.
async fn enter_state(
    pool: &sqlx::PgPool,
    host_id: &MachineId,
    state: ManagedHostState,
    entered_ago: chrono::Duration,
) {
    sqlx::query(
        "UPDATE machines SET controller_state = $1, controller_state_version = $2 WHERE id = $3",
    )
    .bind(sqlx::types::Json(state))
    .bind(format!(
        "V999-T{}",
        (chrono::Utc::now() - entered_ago).timestamp_micros()
    ))
    .bind(host_id)
    .execute(pool)
    .await
    .unwrap();
}

#[crate::sqlx_test]
async fn outstanding_holds_are_exported_by_sla_and_tenancy(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    // The monitor reads through a connection of its own, so that the test can
    // tell whether a read gave it back.
    let reader_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let mut monitor = DpuServiceSyncHoldMonitor::new(
        reader_pool.clone(),
        &env.test_meter.meter(),
        env.config.host_health,
        MachineSlaConfig::new(env.config.machine_state_controller.failure_retry_time),
    );

    // Neither host is DPF-managed: a hold counts wherever it is recorded.
    let idle = create_managed_host(&env).await;
    let assigned = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    assigned
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[0, 0], [0, 0]],
        "a site with no holds exports zero rather than nothing"
    );

    request_sync(&pool, &idle.id).await;
    request_sync(&pool, &assigned.id).await;
    sqlx::query("UPDATE machine_pending_actions SET requested_at = $1 WHERE machine_id = $2")
        .bind(chrono::Utc::now() - TWO_HOURS)
        .bind(idle.id)
        .execute(&pool)
        .await
        .unwrap();
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 1], [0, 0]],
        "a held host is counted under whether an instance is allocated on it"
    );

    // Stranded in `HostInit`, the case the automatic release never reaches.
    enter_state(
        &pool,
        &idle.id,
        ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        },
        ONE_HOUR,
    )
    .await;
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[0, 1], [1, 0]],
        "a held host past the SLA of its state moves to above_sla"
    );
    let ages = samples(&env, OLDEST_AGE);
    let two_hours = TWO_HOURS.num_seconds() as u64;
    assert!(
        (two_hours..two_hours + 60).contains(&ages[1][0]),
        "the age runs from when the hold was first requested, got {}",
        ages[1][0]
    );
    assert!(
        ages[0][1] < 60,
        "each label combination has its own oldest hold, got {}",
        ages[0][1]
    );

    let mut waiver = HealthReport::empty("test-sla-exclusion".to_string());
    waiver.alerts.push(HealthProbeAlert {
        classifications: vec![HealthAlertClassification::exclude_from_state_machine_sla()],
        ..HealthProbeAlert::missing_report()
    });
    let mut txn = pool.txn_begin().await.unwrap();
    db::machine::insert_health_report(
        &mut txn,
        &idle.id,
        HealthReportApplyMode::Merge,
        &waiver,
        false,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 1], [0, 0]],
        "an alert that waives the state machine SLA waives it here too"
    );

    complete_sync(&pool, &idle.id).await;
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[0, 1], [0, 0]],
        "a completed hold is no longer counted, while an outstanding one still is"
    );

    // With the last hold completed a successful read would export zero. A held
    // host's state that cannot be decoded fails the read before that happens.
    complete_sync(&pool, &assigned.id).await;
    request_sync(&pool, &idle.id).await;
    sqlx::query("UPDATE machines SET controller_state = '{}'::jsonb WHERE id = $1")
        .bind(idle.id)
        .execute(&pool)
        .await
        .unwrap();
    let reader_pid = backend_pid(&reader_pool).await;
    let error = monitor
        .run_single_iteration()
        .await
        .expect_err("an undecodable host state fails the read");
    assert!(
        error.to_string().contains(&idle.id.to_string()),
        "a read that one host fails must name that host, got {error}"
    );
    assert_eq!(
        backend_pid(&reader_pool).await,
        reader_pid,
        "a read that fails without being abandoned returns its connection"
    );
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[0, 1], [0, 0]],
        "a failed read keeps the previous read instead of exporting zero"
    );

    sqlx::query("UPDATE machines SET controller_state = $1 WHERE id = $2")
        .bind(sqlx::types::Json(ManagedHostState::Ready))
        .bind(idle.id)
        .execute(&pool)
        .await
        .unwrap();
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 0], [0, 0]],
        "the next read that succeeds replaces what the failed one left"
    );

    // Both hosts past the limit of their state, so one read checks both for a
    // waiver. The idle host still carries its waiver; the tenant's host has
    // none. The test configuration's `failure_retry_time` allows
    // `Assigned/BootingWithDiscoveryImage` about a second, where the default
    // configuration would allow 99 minutes.
    request_sync(&pool, &assigned.id).await;
    enter_state(
        &pool,
        &idle.id,
        ManagedHostState::HostInit {
            machine_state: MachineState::WaitingForDiscovery,
        },
        ONE_HOUR,
    )
    .await;
    enter_state(
        &pool,
        &assigned.id,
        ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: RetryInfo::default(),
            },
        },
        chrono::Duration::minutes(20),
    )
    .await;
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 0], [0, 1]],
        "each host past its limit keeps its own tenancy, alerts and configured SLA"
    );

    // A predicted host is on the operator worklist like any other host, so its
    // hold is counted and its waiver honoured.
    let predicted: PredictedHostMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xAC; 32],
        MachineType::PredictedHost,
    )
    .try_into()
    .unwrap();
    sqlx::query("UPDATE machines SET id = $1 WHERE id = $2")
        .bind(predicted)
        .bind(idle.id)
        .execute(&pool)
        .await
        .unwrap();
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 0], [0, 1]],
        "a held predicted host is read like any other"
    );

    // The cap on waiver checks. Both hosts are past their limit and now both
    // carry the waiver, and the predicted host has waited longer.
    sqlx::query(
        "UPDATE machine_pending_actions SET requested_at = $1
         WHERE machine_id = $2 AND completed_at IS NULL",
    )
    .bind(chrono::Utc::now() - TWO_HOURS)
    .bind(predicted)
    .execute(&pool)
    .await
    .unwrap();
    let mut txn = pool.txn_begin().await.unwrap();
    db::machine::insert_health_report(
        &mut txn,
        &assigned.id,
        HealthReportApplyMode::Merge,
        &waiver,
        false,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    monitor.run_single_iteration().await.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 1], [0, 0]],
        "with room to check both hosts, both waivers apply"
    );

    monitor.set_max_health_lookups(1);
    let (read, logs) = capture_logs_async(monitor.run_single_iteration()).await;
    read.unwrap();
    assert_eq!(
        samples(&env, PENDING_COUNT),
        [[1, 0], [0, 1]],
        "the one check goes to the longest wait; the host beyond the cap counts as above SLA despite its waiver"
    );
    let unchecked_host_counts: Vec<_> = logs
        .iter()
        .filter_map(|log| log.field("unchecked_host_count"))
        .collect();
    assert_eq!(
        unchecked_host_counts,
        ["1"],
        "a capped read warns how many hosts it counted without a check"
    );
    reader_pool.close().await;
}
