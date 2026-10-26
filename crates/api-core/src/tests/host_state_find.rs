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

//! The compact host read: its snapshot, and its agreement with the full read,
//! allocation and the update manager.

use std::collections::HashMap;
use std::fmt::Debug;

use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use carbide_uuid::rack::RackId;
use chrono::Utc;
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use model::expected_machine::ExpectedMachineData;
use model::machine::{LoadSnapshotOptions, ManagedHostStateSnapshot};
use rpc::forge::forge_server::Forge;
use tonic::Request;

use crate::test_support::fixture_config::ManagedHostConfigExt as _;
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{
    TestEnv, create_managed_host, create_managed_host_multi_dpu, create_managed_host_with_config,
    create_test_env,
};

/// A health report from `source` with one allocation-blocking alert.
fn alert_report(source: &str, message: &str) -> rpc::forge::HealthReportEntry {
    let mut report = HealthReport::empty(source.to_string());
    report.alerts.push(HealthProbeAlert {
        id: "CompactReadFixture".parse().unwrap(),
        target: None,
        in_alert_since: Some(Utc::now()),
        message: message.to_string(),
        tenant_message: None,
        classifications: vec![HealthAlertClassification::prevent_allocations()],
    });
    rpc::forge::HealthReportEntry {
        report: Some(report.into()),
        mode: rpc::forge::HealthReportApplyMode::Merge as i32,
    }
}

async fn insert_alert(env: &TestEnv, machine_id: MachineId, message: &str) {
    env.api
        .insert_machine_health_report(Request::new(rpc::forge::InsertMachineHealthReportRequest {
            machine_id: Some(machine_id),
            health_report_entry: Some(alert_report("compact-read-fixture", message)),
        }))
        .await
        .unwrap();
}

/// One host per state the compact read must reproduce, built with the
/// existing fixtures and RPCs.
async fn fixture_hosts(env: &TestEnv) -> Vec<(&'static str, HostMachineId)> {
    let ready = create_managed_host(env).await;

    // Both DPUs raise the same alert key, so the merged message follows DPU order.
    let two_dpus = create_managed_host_multi_dpu(env, 2).await;
    for (dpu_id, message) in two_dpus.dpu_ids.iter().zip(["first DPU", "second DPU"]) {
        insert_alert(env, (*dpu_id).into(), message).await;
    }

    let maintenance = create_managed_host(env).await;
    env.api
        .set_maintenance(Request::new(rpc::forge::MaintenanceRequest {
            operation: rpc::forge::MaintenanceOperation::Enable.into(),
            host_id: Some(maintenance.id.into()),
            reference: Some("compact read fixture".into()),
        }))
        .await
        .unwrap();

    let host_alert = create_managed_host(env).await;
    insert_alert(env, host_alert.id.into(), "host alert").await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut env.pool.acquire().await.unwrap())
        .await
        .unwrap();
    let rack_alert = create_managed_host_with_config(
        env,
        env.managed_host_config()
            .with_expected_machine_data(ExpectedMachineData {
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            }),
    )
    .await;
    env.api
        .insert_rack_health_report(Request::new(rpc::forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id),
            health_report_entry: Some(alert_report("compact-read-fixture", "rack alert")),
        }))
        .await
        .unwrap();

    let assigned = create_managed_host(env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    assigned
        .instance_builer(env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    [
        ("ready", &ready),
        ("two DPUs with one alert key", &two_dpus),
        ("maintenance", &maintenance),
        ("host alert", &host_alert),
        ("rack alert", &rack_alert),
        ("instance", &assigned),
    ]
    .into_iter()
    .map(|(name, host)| (name, HostMachineId::from(&host.id)))
    .collect()
}

/// What the allocation and health rules read from a snapshot. Merged alert
/// messages are compared as sorted lines, since their order follows DPU order.
fn rule_inputs(snapshot: &ManagedHostStateSnapshot) -> impl PartialEq + Debug {
    let host = &snapshot.host_snapshot;
    let mut dpus: Vec<_> = snapshot
        .dpu_snapshots
        .iter()
        .map(|dpu| {
            (
                dpu.id,
                dpu.reprovision_requested.clone(),
                dpu.health_reports.clone(),
            )
        })
        .collect();
    dpus.sort_by_key(|dpu| dpu.0);
    let alerts: Vec<_> = snapshot
        .aggregate_health
        .alerts
        .iter()
        .map(|alert| {
            let mut lines: Vec<_> = alert.message.lines().map(str::to_string).collect();
            lines.sort();
            (
                alert.id.clone(),
                alert.target.clone(),
                alert.classifications.clone(),
                lines,
            )
        })
        .collect();
    (
        (
            snapshot.managed_state.clone(),
            snapshot.instance.is_some(),
            host.decommission_requested,
            host.machine_maintenance_requested.clone(),
            host.health_reports.clone(),
            host.pending_boot_interface_config_version(),
        ),
        dpus,
        snapshot.rack_health_overrides.clone(),
        alerts,
        snapshot
            .is_usable_as_instance(false)
            .map_err(|reason| reason.code()),
    )
}

async fn full_and_compact(
    env: &TestEnv,
    host_ids: &[HostMachineId],
) -> (
    HashMap<HostMachineId, ManagedHostStateSnapshot>,
    HashMap<HostMachineId, ManagedHostStateSnapshot>,
) {
    let mut txn = env.pool.begin().await.unwrap();
    let full = db::managed_host::load_by_machine_ids(
        &mut *txn,
        host_ids,
        LoadSnapshotOptions::default().with_host_health(env.config.host_health),
    )
    .await
    .unwrap();
    let compact =
        db::managed_host::load_compact_by_host_ids(&mut txn, host_ids, env.config.host_health)
            .await
            .unwrap();
    txn.rollback().await.unwrap();
    (full, compact)
}

#[crate::sqlx_test]
async fn compact_snapshot_matches_full_snapshot(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let hosts = fixture_hosts(&env).await;
    let host_ids: Vec<_> = hosts.iter().map(|(_, id)| *id).collect();

    // A batch and a single ID take different query forms.
    let (full, compact) = full_and_compact(&env, &host_ids).await;
    let (_, single) = full_and_compact(&env, &host_ids[..1]).await;
    assert_eq!(compact.len(), hosts.len());
    for (name, host_id) in &hosts {
        assert_eq!(
            rule_inputs(&compact[host_id]),
            rule_inputs(&full[host_id]),
            "{name}"
        );
    }
    assert_eq!(
        rule_inputs(&single[&host_ids[0]]),
        rule_inputs(&full[&host_ids[0]])
    );
}

#[crate::sqlx_test]
async fn compact_decode_sorts_topology_free_dpus(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_id = HostMachineId::from(&create_managed_host_multi_dpu(&env, 2).await.id);

    let (full, compact) = full_and_compact(&env, &[host_id]).await;
    let (full, compact) = (&full[&host_id], &compact[&host_id]);
    let primary_dpu = full
        .host_snapshot
        .status
        .interfaces
        .iter()
        .find(|interface| interface.primary_interface)
        .and_then(|interface| interface.attached_dpu_machine_id);
    assert_eq!(compact.dpu_snapshots.len(), 2);
    assert!(
        compact
            .dpu_snapshots
            .iter()
            .all(|dpu| dpu.status.hardware_info.is_none())
    );
    assert_eq!(Some(compact.dpu_snapshots[0].id), primary_dpu);
    assert_eq!(compact.dpu_snapshots[0].id, full.dpu_snapshots[0].id);
}
async fn find_host_states(
    env: &TestEnv,
    host_ids: Vec<MachineId>,
) -> Result<rpc::forge::HostStateList, tonic::Status> {
    env.api
        .find_host_states_by_ids(Request::new(rpc::forge::HostStatesByIdsRequest {
            host_ids,
        }))
        .await
        .map(tonic::Response::into_inner)
}

#[crate::sqlx_test]
async fn find_host_states_rejects_bad_requests(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let host_id = MachineId::from(host.id);
    let over_max = vec![host_id; env.config.max_find_by_ids as usize + 1];

    for (scenario, host_ids) in [
        ("no IDs", vec![]),
        ("more than max_find_by_ids", over_max),
        ("a DPU ID", vec![host_id, host.dpu_ids[0].into()]),
    ] {
        let error = find_host_states(&env, host_ids).await.expect_err(scenario);
        assert_eq!(error.code(), tonic::Code::InvalidArgument, "{scenario}");
    }
}

#[crate::sqlx_test]
async fn find_host_states_dedups_sorts_and_omits_missing(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mut host_ids: Vec<MachineId> = vec![
        create_managed_host(&env).await.id.into(),
        create_managed_host(&env).await.id.into(),
    ];
    host_ids.sort();
    let missing = MachineId::new(MachineIdSource::Tpm, [0x77; 32], MachineType::Host);

    let reply = find_host_states(&env, vec![host_ids[1], missing, host_ids[0], host_ids[1]])
        .await
        .unwrap();

    let returned: Vec<_> = reply.hosts.iter().filter_map(|host| host.id).collect();
    assert_eq!(returned, host_ids);
    assert!(reply.read_at.is_some());
}
