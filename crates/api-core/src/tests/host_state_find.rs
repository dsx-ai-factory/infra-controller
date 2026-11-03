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
use std::net::IpAddr;
use std::time::Duration;

use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::rack::RackId;
use chrono::Utc;
use db::ConditionalWrite;
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use itertools::Itertools;
use model::expected_machine::ExpectedMachineData;
use model::firmware::{DriveModel, FirmwareComponent, FirmwareComponentType};
use model::hardware_info::HardwareInfo;
use model::machine::{
    LoadSnapshotOptions, ManagedHostState, ManagedHostStateSnapshot, NotAllocatableReason,
};
use model::machine_boot_interface::{BootInterfaceSelectionSource, MachineBootInterfaceTarget};
use model::site_explorer::{
    EndpointExplorationError, EndpointExplorationReport, ExploredEndpoint, Inventory,
};
use model::test_support::ManagedHostConfig;
use regex::Regex;
use rpc::forge::forge_server::Forge;
use tonic::Request;

use crate::cfg::file::CarbideConfig;
use crate::handlers::host_state::SnapshotReadHook;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host, create_managed_host_multi_dpu,
    create_managed_host_with_config, create_test_env, create_test_env_with_overrides, get_config,
};
use crate::tests::host_firmware_config::{host_reprovision_requested, update_manager};

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

async fn host_state(env: &TestEnv, host_id: MachineId) -> rpc::forge::HostState {
    find_host_states(env, vec![host_id])
        .await
        .unwrap()
        .hosts
        .remove(0)
}

async fn stored_endpoint(env: &TestEnv, address: IpAddr) -> ExploredEndpoint {
    db::explored_endpoints::find_all_by_ip(address, &mut env.pool.acquire().await.unwrap())
        .await
        .unwrap()
        .remove(0)
}

/// One request reads one database snapshot. A commit from another connection
/// between its snapshot read and its gate read changes neither, and the next
/// request sees the commit.
#[crate::sqlx_test]
async fn read_snapshot_ignores_concurrent_commit(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_id = MachineId::from(create_managed_host(&env).await.id);
    let hook = SnapshotReadHook::default();
    let mut request = Request::new(rpc::forge::HostStatesByIdsRequest {
        host_ids: vec![host_id],
    });
    request.extensions_mut().insert(hook.clone());
    let api = env.api.clone();
    let reply = tokio::spawn(async move { api.find_host_states_by_ids(request).await });

    let reached = hook.after_snapshot.notified();
    tokio::time::timeout(Duration::from_secs(60), reached)
        .await
        .expect("snapshot read");
    sqlx::query("UPDATE machines SET firmware_autoupdate = false WHERE id = $1")
        .bind(host_id)
        .execute(&env.pool)
        .await
        .unwrap();
    hook.resume.notify_one();

    let answers = |state: rpc::forge::HostState| {
        let update = state.update.unwrap();
        let reason = update.blocking_reason;
        let policy = update.firmware_policy_eligible;
        (state.firmware_autoupdate, update.eligible, reason, policy)
    };
    let first = reply.await.unwrap().unwrap().into_inner().hosts.remove(0);
    assert_eq!(answers(first), (None, true, None, Some(true)));
    let off = Some("autoupdate_off_host".to_string());
    let next = host_state(&env, host_id).await;
    assert_eq!(answers(next), (Some(false), false, off, Some(false)));
}

/// Alerts with their merged message lines sorted, since merge order follows
/// DPU order.
fn sorted_alerts(
    mut alerts: Vec<rpc::health::HealthProbeAlert>,
) -> Vec<rpc::health::HealthProbeAlert> {
    for alert in &mut alerts {
        alert.message = alert.message.lines().sorted().join("\n");
    }
    alerts
}

#[crate::sqlx_test]
async fn compact_read_matches_full_read_and_explored_report(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let hosts = fixture_hosts(&env).await;
    let machine_ids: Vec<MachineId> = hosts.iter().map(|(_, id)| *id.as_machine_id()).collect();
    let mut compact: HashMap<_, _> = find_host_states(&env, machine_ids.clone())
        .await
        .unwrap()
        .hosts
        .into_iter()
        .map(|state| (state.id, state))
        .collect();
    let mut full: HashMap<_, _> = env
        .api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids,
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .into_iter()
        .map(|machine| (machine.id, machine))
        .collect();

    for (name, host_id) in &hosts {
        let id = Some(*host_id.as_machine_id());
        let (state, machine) = (compact.remove(&id).unwrap(), full.remove(&id).unwrap());
        let (status, config) = (machine.status.unwrap(), machine.config.unwrap());
        let bmc = machine.bmc_info.unwrap_or_default();
        // Allocation, update and the explored report exist only in the compact read.
        let expected = rpc::forge::HostState {
            id: machine.id,
            state: machine.state,
            state_version: machine.state_version,
            state_reason: machine.state_reason,
            rack_id: machine.rack_id,
            nvlink_domain_id: status.nvlink_info.and_then(|info| info.domain_uuid),
            instance_type_id: config.instance_type_id,
            bmc_ip: bmc.ip,
            bmc_mac: bmc.mac,
            dpu_ids: status.associated_dpu_machine_ids,
            update_complete: status.update_complete,
            firmware_autoupdate: config.firmware_autoupdate,
            health_alerts: sorted_alerts(status.health.unwrap().alerts),
            quarantine: status.quarantine,
            ..state.clone()
        };
        let health_alerts = sorted_alerts(state.health_alerts.clone());
        let state = rpc::forge::HostState {
            health_alerts,
            ..state
        };
        assert_eq!(state, expected, "{name}");

        let explored = state.explored.expect(name);
        let request = rpc::site_explorer::ExploredEndpointsByIdsRequest {
            endpoint_ids: vec![explored.address.clone()],
        };
        let endpoint = env
            .api
            .find_explored_endpoints_by_ids(Request::new(request))
            .await
            .unwrap()
            .into_inner()
            .endpoints
            .remove(0);
        let report = endpoint.report.unwrap();
        let versions = &report.firmware_versions;
        assert_eq!(explored.vendor, report.vendor, "{name}");
        assert_eq!(explored.bmc_version.as_ref(), versions.get("bmc"), "{name}");
        assert_eq!(
            explored.uefi_version.as_ref(),
            versions.get("uefi"),
            "{name}"
        );
        assert_eq!(explored.report_version, endpoint.report_version, "{name}");
        let error = report.last_exploration_error;
        assert_eq!(explored.last_exploration_error, error, "{name}");
        // The full explored read has no model, so it comes from the stored report.
        let stored = stored_endpoint(&env, explored.address.parse().unwrap()).await;
        assert_eq!(explored.model, stored.report.model, "{name}");
    }
}

/// How `allocation_attempt_agrees_per_reason` stops a host from being allocated.
#[derive(Clone, Copy)]
enum Blocker {
    None,
    NotReady,
    PendingInstance,
    PendingDecommission,
    PendingBootConfiguration,
    DpuReprovisionStarted,
    Maintenance,
    HealthAlert,
}

fn allocation_request(
    host: &TestManagedHost,
    segment_id: NetworkSegmentId,
) -> rpc::forge::InstanceAllocationRequest {
    rpc::forge::InstanceAllocationRequest {
        machine_id: Some(host.id),
        config: Some(rpc::forge::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(single_interface_network_config(segment_id)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn block_allocation(
    env: &TestEnv,
    host: &TestManagedHost,
    segment_id: NetworkSegmentId,
    blocker: Blocker,
) {
    let host_id = HostMachineId::from(&host.id);
    let mut txn = env.pool.begin().await.unwrap();
    match blocker {
        Blocker::None => {}
        Blocker::NotReady => {
            db::machine::update_state(&mut txn, &host_id, &ManagedHostState::ForceDeletion)
                .await
                .unwrap()
        }
        // Allocation stores the instance before the state machine sees it.
        Blocker::PendingInstance => {
            env.api
                .allocate_instance(Request::new(allocation_request(host, segment_id)))
                .await
                .unwrap();
        }
        Blocker::PendingDecommission => db::machine::set_decommission_requested(&mut txn, host_id)
            .await
            .unwrap(),
        Blocker::PendingBootConfiguration => {
            db::machine_desired_boot_interface::force_set(
                &mut txn,
                &host_id,
                &MachineBootInterfaceTarget::MacOnly([0x02, 0, 0, 0, 0x81, 0x50].into()),
                BootInterfaceSelectionSource::Operator,
            )
            .await
            .unwrap();
        }
        Blocker::DpuReprovisionStarted => {
            let dpu_id = &host.dpu_ids[0];
            db::machine::trigger_dpu_reprovisioning_request(dpu_id, &mut txn, "test", false)
                .await
                .unwrap();
            db::machine::update_dpu_reprovision_start_time(dpu_id, &mut txn)
                .await
                .unwrap();
        }
        Blocker::Maintenance => {
            env.api
                .set_maintenance(Request::new(rpc::forge::MaintenanceRequest {
                    operation: rpc::forge::MaintenanceOperation::Enable.into(),
                    host_id: Some(host.id.into()),
                    reference: Some("compact read test".into()),
                }))
                .await
                .unwrap();
        }
        Blocker::HealthAlert => insert_alert(env, host.id.into(), "host alert").await,
    }
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn allocation_attempt_agrees_per_reason(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Every reason but no_dpu_snapshots, which needs a host whose DPU rows are missing.
    for (blocker, expect) in [
        (Blocker::None, None),
        (Blocker::NotReady, Some("invalid_state")),
        (Blocker::PendingInstance, Some("pending_instance_creation")),
        (Blocker::PendingDecommission, Some("pending_decommission")),
        (
            Blocker::PendingBootConfiguration,
            Some("pending_boot_configuration"),
        ),
        (
            Blocker::DpuReprovisionStarted,
            Some("dpu_reprovisioning_started"),
        ),
        (Blocker::Maintenance, Some("maintenance_mode")),
        (Blocker::HealthAlert, Some("health_alert")),
    ] {
        let host = create_managed_host(&env).await;
        block_allocation(&env, &host, segment_id, blocker).await;

        let allocation = host_state(&env, host.id.into()).await.allocation.unwrap();
        assert_eq!(allocation.blocking_reason.as_deref(), expect);
        assert_eq!(allocation.allocatable, expect.is_none(), "{expect:?}");

        // Allocation fails with the error for the same reason, or succeeds.
        let reason = db::managed_host::load_snapshot(
            &mut *env.pool.acquire().await.unwrap(),
            &host.id.into(),
            LoadSnapshotOptions::default().with_host_health(env.config.host_health),
        )
        .await
        .unwrap()
        .unwrap()
        .is_usable_as_instance(false);
        let attempt = env
            .api
            .allocate_instance(Request::new(allocation_request(&host, segment_id)))
            .await;
        match (reason, attempt) {
            (Ok(()), Ok(_)) => assert_eq!(expect, None),
            (Err(reason), Err(error)) => {
                assert_eq!(Some(reason.code()), expect);
                // Written out here, not taken from production, so a changed mapping fails.
                let (code, wording, names_machine) = match &reason {
                    NotAllocatableReason::InvalidState(_) => {
                        (tonic::Code::InvalidArgument, "given machine state", true)
                    }
                    NotAllocatableReason::PendingInstanceCreation => (
                        tonic::Code::InvalidArgument,
                        "already used by another instance creation request",
                        true,
                    ),
                    NotAllocatableReason::PendingDecommission => (
                        tonic::Code::FailedPrecondition,
                        "pending decommission request",
                        true,
                    ),
                    NotAllocatableReason::PendingBootConfiguration => (
                        tonic::Code::FailedPrecondition,
                        "pending boot configuration",
                        true,
                    ),
                    NotAllocatableReason::DpuReprovisioningStarted => (
                        tonic::Code::FailedPrecondition,
                        "started DPU reprovisioning",
                        true,
                    ),
                    NotAllocatableReason::NoDpuSnapshots => {
                        (tonic::Code::Internal, "has no DPU", true)
                    }
                    // These two messages do not name the machine.
                    NotAllocatableReason::MaintenanceMode => {
                        (tonic::Code::FailedPrecondition, "MaintenanceMode", false)
                    }
                    NotAllocatableReason::HealthAlert(_) => {
                        (tonic::Code::FailedPrecondition, "health probe alert", false)
                    }
                };
                assert_eq!(error.code(), code, "{expect:?}: {}", error.message());
                assert!(
                    error.message().contains(wording),
                    "{expect:?}: {:?} lacks {wording:?}",
                    error.message()
                );
                assert_eq!(
                    error.message().contains(&host.id.to_string()),
                    names_machine,
                    "{expect:?}: {:?}",
                    error.message()
                );
            }
            (reason, attempt) => panic!("{expect:?}: {reason:?} but allocation gave {attempt:?}"),
        }
    }
}

#[crate::sqlx_test]
async fn explored_report_freshness(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let read = async || host_state(&env, host.id.into()).await.explored.unwrap();

    // A healthy report stores LastExplorationError as JSON null.
    let initial = read().await;
    let address: IpAddr = initial.address.parse().unwrap();
    assert_eq!(initial.last_exploration_error, None);
    let try_update = async |report: &EndpointExplorationReport| {
        let version = stored_endpoint(&env, address).await.report_version;
        let mut conn = env.pool.acquire().await.unwrap();
        db::explored_endpoints::try_update(address, version, report, false, &mut conn)
            .await
            .unwrap()
    };

    // A new BMC version shows at once, while the machine record keeps the old one.
    let mut report = stored_endpoint(&env, address).await.report;
    report
        .versions
        .insert(FirmwareComponentType::Bmc, "9.9.9".to_string());
    assert_eq!(try_update(&report).await, ConditionalWrite::Applied(()));
    let updated = read().await;
    assert_eq!(updated.bmc_version.as_deref(), Some("9.9.9"));
    assert_ne!(updated.report_at, initial.report_at);
    assert_eq!(updated.waiting_for_explorer_refresh, Some(false));
    let request = rpc::forge::MachinesByIdsRequest {
        machine_ids: vec![host.id.into()],
        include_history: false,
    };
    let machine = env
        .api
        .find_machines_by_ids(Request::new(request))
        .await
        .unwrap();
    let full_bmc = machine.into_inner().machines.remove(0).bmc_info.unwrap();
    assert_ne!(full_bmc.firmware_version.as_deref(), Some("9.9.9"));

    // A failed exploration keeps the old versions and waits for a refresh.
    let error = EndpointExplorationError::ConnectionRefused {
        details: "port 443".to_string(),
    };
    let version = stored_endpoint(&env, address).await.report_version;
    let write = db::explored_endpoints::try_update_last_exploration_error(
        address,
        version,
        &error,
        Duration::from_secs(1),
        &mut env.pool.acquire().await.unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(write, ConditionalWrite::Applied(()));
    let failed = read().await;
    assert_eq!(
        failed.last_exploration_error,
        Some(serde_json::to_string(&error).unwrap())
    );
    assert_eq!(failed.bmc_version.as_deref(), Some("9.9.9"));
    assert_eq!(failed.waiting_for_explorer_refresh, Some(true));

    // Clearing the error is not a fresh exploration either.
    db::explored_endpoints::clear_last_known_error(address, &mut env.pool.acquire().await.unwrap())
        .await
        .unwrap();
    let cleared = read().await;
    assert_eq!(cleared.last_exploration_error, None);
    assert_eq!(cleared.bmc_version.as_deref(), Some("9.9.9"));
    assert_eq!(cleared.waiting_for_explorer_refresh, Some(true));

    // A real report write ends the wait.
    assert_eq!(try_update(&report).await, ConditionalWrite::Applied(()));
    assert_eq!(read().await.waiting_for_explorer_refresh, Some(false));

    // A report without the LastExplorationError key has no error.
    sqlx::query(
        "UPDATE explored_endpoints SET exploration_report = exploration_report - 'LastExplorationError'
            WHERE address = $1",
    )
    .bind(address)
    .execute(&env.pool)
    .await
    .unwrap();
    assert_eq!(read().await.last_exploration_error, None);
}

/// One host in an env with `configure` applied to its config. The compact
/// read's update answer must match whether a manager pass with spare capacity
/// requests the update.
async fn eligibility_matches_update_manager(
    pool: sqlx::PgPool,
    configure: impl FnOnce(&mut CarbideConfig, HostMachineId),
    assign_instance: bool,
    expect: Result<(), &str>,
) {
    // The host ID is known before the env exists, so the config can list it.
    let host_config = ManagedHostConfig::default();
    let host_id = HostMachineId::try_from(
        model::machine::machine_id::from_hardware_info(&HardwareInfo::from(&host_config)).unwrap(),
    )
    .unwrap();
    let mut config = get_config();
    configure(&mut config, host_id);
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let host = create_managed_host_with_config(&env, host_config).await;
    assert_eq!(HostMachineId::from(&host.id), host_id);
    if assign_instance {
        let segment_id = env.create_vpc_and_tenant_segment().await;
        host.instance_builer(&env)
            .single_interface_network_config(segment_id)
            .build()
            .await;
    }

    let update = host_state(&env, host.id.into()).await.update.unwrap();
    assert_eq!(update.blocking_reason.as_deref(), expect.err());
    assert_eq!(update.eligible, expect.is_ok());

    update_manager(&env, env.config.clone())
        .run_single_iteration()
        .await
        .unwrap();
    assert_eq!(
        host_reprovision_requested(&env, &host).await,
        update.eligible
    );
}

#[crate::sqlx_test]
async fn eligibility_matches_update_manager_global_on(pool: sqlx::PgPool) {
    eligibility_matches_update_manager(pool, |_, _| {}, false, Ok(())).await;
}

#[crate::sqlx_test]
async fn eligibility_matches_update_manager_global_off(pool: sqlx::PgPool) {
    let configure = |config: &mut CarbideConfig, _| config.firmware_global.autoupdate = false;
    eligibility_matches_update_manager(pool, configure, false, Err("autoupdate_off_global")).await;
}

#[crate::sqlx_test]
async fn eligibility_matches_update_manager_disable_list(pool: sqlx::PgPool) {
    let configure = |config: &mut CarbideConfig, host_id: HostMachineId| {
        config.firmware_global.host_disable_autoupdate = vec![host_id.to_string()]
    };
    eligibility_matches_update_manager(pool, configure, false, Err("disable_list")).await;
}

#[crate::sqlx_test]
async fn eligibility_matches_update_manager_manual_tagging(pool: sqlx::PgPool) {
    let configure = |config: &mut CarbideConfig, _| {
        config.firmware_global.instance_updates_manual_tagging = true
    };
    eligibility_matches_update_manager(pool, configure, true, Err("not_ready")).await;
}

#[crate::sqlx_test]
async fn compact_read_error_returns_no_partial_batch(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let broken = MachineId::from(create_managed_host(&env).await.id);
    let healthy = MachineId::from(create_managed_host(&env).await.id);
    sqlx::query(
        "UPDATE explored_endpoints
            SET exploration_report = jsonb_set(exploration_report, '{Versions}', '\"broken\"')
            WHERE address = $1::inet",
    )
    .bind(host_state(&env, broken).await.bmc_ip.unwrap())
    .execute(&env.pool)
    .await
    .unwrap();

    let error = find_host_states(&env, vec![broken, healthy])
        .await
        .expect_err("a host that fails to decode fails the batch");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        find_host_states(&env, vec![healthy])
            .await
            .unwrap()
            .hosts
            .len(),
        1
    );
}

#[crate::sqlx_test]
async fn compact_inventory_through_handler(pool: sqlx::PgPool) {
    // The R750 catalog entry also covers CX7 cards, which report one entry per card,
    // and two drive models, which share a slug.
    let mut config = get_config();
    let cx7 = Regex::new("^CX7_").unwrap();
    let drive_models = ["KCD8DPUG15T3", "Dell Ent NVMe CM6 RI 1.92TB"];
    for firmware in config.host_models.values_mut() {
        if firmware.model == "PowerEdge R750" {
            let component = FirmwareComponent {
                current_version_reported_as: Some(cx7.clone()),
                ..Default::default()
            };
            firmware
                .components
                .insert(FirmwareComponentType::Cx7, component);
            for model in drive_models {
                let component = FirmwareComponent {
                    current_version_reported_as: Some(
                        Regex::new(&format!("^Drive_{}_", regex::escape(model))).unwrap(),
                    ),
                    ..Default::default()
                };
                let model = DriveModel::new(model).unwrap();
                firmware
                    .components
                    .insert(FirmwareComponentType::Drive { model }, component);
            }
        }
    }
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let host = create_managed_host(&env).await;
    let read = async || host_state(&env, host.id.into()).await.explored.unwrap();
    let address: IpAddr = read().await.address.parse().unwrap();
    let entry = |id: &str, version: &str| (id.to_string(), Some(version.to_string()));

    let reported = [
        ("CX7_0", "28.2"),
        ("CX7_1", "28.1"),
        ("Drive_KCD8DPUG15T3_drive_bay_0", "1XET7106"),
        ("Drive_Dell Ent NVMe CM6 RI 1.92TB_Disk.Bay.1", "2.2.1"),
        ("Drive_KCD8DPUG15T3_drive_bay_1", "1XET7107"),
    ];
    let mut reversed = reported;
    reversed.reverse();
    for report in [reported, reversed] {
        let mut endpoint = stored_endpoint(&env, address).await;
        let inventories = &mut endpoint.report.service[0].inventories;
        inventories.retain(|inventory| {
            !inventory.id.starts_with("CX7_") && !inventory.id.starts_with("Drive_")
        });
        inventories.extend(report.map(|(id, version)| Inventory {
            id: id.to_string(),
            version: Some(version.to_string()),
            ..Default::default()
        }));
        let mut conn = env.pool.acquire().await.unwrap();
        let version = endpoint.report_version;
        let write = db::explored_endpoints::try_update(
            address,
            version,
            &endpoint.report,
            false,
            &mut conn,
        );
        assert_eq!(write.await.unwrap(), ConditionalWrite::Applied(()));

        let inventory: Vec<_> = read()
            .await
            .firmware_inventory
            .unwrap()
            .components
            .into_iter()
            .map(|group| {
                let entries = group
                    .entries
                    .into_iter()
                    .map(|entry| (entry.inventory_id, entry.version));
                (group.component, entries.collect::<Vec<_>>())
            })
            .collect();
        let expected = [
            ("bmc", vec![entry("Installed-__iDRACz", "5.10.20")]),
            ("cx7", vec![entry("CX7_0", "28.2"), entry("CX7_1", "28.1")]),
            (
                "drive-Dell Ent NVMe CM6 RI 1.92TB",
                vec![entry(
                    "Drive_Dell Ent NVMe CM6 RI 1.92TB_Disk.Bay.1",
                    "2.2.1",
                )],
            ),
            (
                "drive-KCD8DPUG15T3",
                vec![
                    entry("Drive_KCD8DPUG15T3_drive_bay_0", "1XET7106"),
                    entry("Drive_KCD8DPUG15T3_drive_bay_1", "1XET7107"),
                ],
            ),
            (
                "uefi",
                vec![entry("Current-159-1.13.2__BIOS.Setup.1-1", "1.12.0")],
            ),
        ]
        .map(|(component, entries)| (component.to_string(), entries));
        assert_eq!(inventory, expected, "{report:?}");
    }
}
