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

//! Checks compatibility before automatic DPF updates queue or restart DPUs.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbide_dpf::{DpfError, DpuDeploymentType, DpuPhase};
use carbide_instrument::testing::capture_logs_async;
use carbide_machine_controller::dpf::{
    DpfOperations, MockDpfOperations, validate_reprovision_compatibility,
};
use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::{AsMachineId, StableHostMachineId};
use libredfish::{PowerState, SystemPowerControl};
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::dpu_machine_update::{DpuMachineUpdate, OutdatedDpfDpu};
use model::expected_machine::ExpectedInterface;
use model::machine::{
    DecommissioningState, FailureCause, InstallDpuOsState, Machine, MachineValidationContext,
    MachineValidationFilter, ManagedHostState, ReprovisionState,
};
use model::machine_update_module::{
    AutomaticFirmwareUpdateReference, DpuReprovisionInitiator, HOST_UPDATE_HEALTH_REPORT_SOURCE,
};
use model::test_support::dpu::DPU_BF4_INFO_JSON;
use rpc::forge::forge_server::Forge;
use state_controller::controller::StateController;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use super::{dpf_config, expect_dpf_service_inventory, phase_observation};
use crate::cfg::file::CarbideConfig;
use crate::machine_update_manager::dpu_nic_firmware::DpuNicFirmwareUpdate;
use crate::machine_update_manager::machine_update_module::MachineUpdateModule;
use crate::tests::common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, TestManagedHost, create_managed_host_with_dpf_bf4,
    create_managed_host_with_dpf_multi, create_managed_host_with_dpf_multi_hw,
    create_test_env_with_overrides, get_config,
};
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::dpu_machine_update::get_all_snapshots;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
enum LabelObservation {
    Current,
    Stale,
    Unavailable,
    AstraOnly,
}

struct Fixture {
    env: TestEnv,
    host: TestManagedHost,
    updater: DpuNicFirmwareUpdate,
    labels: Arc<Mutex<LabelObservation>>,
    outdated: Arc<Mutex<Vec<OutdatedDpfDpu>>>,
    label_reads: Arc<AtomicUsize>,
    verified_deployments: Arc<Mutex<Vec<DpuDeploymentType>>>,
    reject_next_reads: Arc<AtomicUsize>,
}

async fn fixture(pool: sqlx::PgPool) -> Fixture {
    fixture_with_deployment(pool, DpuDeploymentType::Bf4Generic, 1).await
}

async fn fixture_with_deployment(
    pool: sqlx::PgPool,
    deployment: DpuDeploymentType,
    dpu_count: usize,
) -> Fixture {
    fixture_with_config(pool, deployment, dpu_count, get_config()).await
}

async fn fixture_with_config(
    pool: sqlx::PgPool,
    deployment: DpuDeploymentType,
    dpu_count: usize,
    mut config: CarbideConfig,
) -> Fixture {
    let labels = Arc::new(Mutex::new(LabelObservation::Current));
    let outdated = Arc::new(Mutex::new(Vec::<OutdatedDpfDpu>::new()));
    let label_reads = Arc::new(AtomicUsize::new(0));
    let verified_deployments = Arc::new(Mutex::new(Vec::new()));
    let reject_next_reads = Arc::new(AtomicUsize::new(0));
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    // The fixture must observe `WaitingForReady` before completing DPU
    // discovery. Park each device there once so a controller iteration
    // with multiple hosts cannot skip that checkpoint.
    let mut observed_devices = HashSet::new();
    mock.expect_get_dpu_phase_observation()
        .returning(move |device, _| {
            Ok(phase_observation(
                if observed_devices.insert(device.to_string()) {
                    DpuPhase::NodeEffect
                } else {
                    DpuPhase::Ready
                },
            ))
        });
    mock.expect_deployment_type_for_dpu()
        .returning(move |_, astra_nics| {
            Ok(
                if astra_nics && deployment == DpuDeploymentType::Bf4Generic {
                    DpuDeploymentType::Bf4Astra
                } else {
                    deployment
                },
            )
        });
    let observed_labels = labels.clone();
    let reads = label_reads.clone();
    let deployments = verified_deployments.clone();
    let reject_reads = reject_next_reads.clone();
    mock.expect_verify_node_labels()
        .returning(move |_, deployment| {
            reads.fetch_add(1, Ordering::SeqCst);
            deployments.lock().unwrap().push(deployment);
            if reject_reads
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Ok(false);
            }
            match *observed_labels.lock().unwrap() {
                LabelObservation::Current => Ok(true),
                LabelObservation::Stale => Ok(false),
                LabelObservation::Unavailable => {
                    Err(DpfError::timeout("read DPUNode", "injected read failure"))
                }
                LabelObservation::AstraOnly => Ok(deployment == DpuDeploymentType::Bf4Astra),
            }
        });
    let observed_outdated = outdated.clone();
    mock.expect_find_outdated_dpus_dpf()
        .returning(move || Ok(observed_outdated.lock().unwrap().clone()));
    // Compatibility checks must not delete DPUs or change selector labels.
    mock.expect_reprovision_dpu().never();
    mock.expect_transfer_dpu_node_deployment_labels().never();
    expect_dpf_service_inventory(&mut mock);
    let dpf: Arc<dyn DpfOperations> = Arc::new(mock);
    config.dpf = dpf_config();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf.clone()),
    )
    .await;
    let host = timeout(TEST_TIMEOUT, async {
        match deployment {
            DpuDeploymentType::Bf3 => create_managed_host_with_dpf_multi(&env, dpu_count).await,
            DpuDeploymentType::Bf4Generic => {
                create_managed_host_with_dpf_multi_hw(&env, dpu_count, DPU_BF4_INFO_JSON).await
            }
            _ => panic!("unsupported fixture deployment: {deployment:?}"),
        }
    })
    .await
    .expect("timed out during initial provisioning");
    outdated.lock().unwrap().push(OutdatedDpfDpu {
        dpu_machine_id: host.dpu_ids[0],
        target_source: "updated-bfb".to_string(),
    });
    let updater = DpuNicFirmwareUpdate {
        reported_wrong_versions: Mutex::new(HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: Some(dpf),
    };
    label_reads.store(0, Ordering::SeqCst);
    Fixture {
        env,
        host,
        updater,
        labels,
        outdated,
        label_reads,
        verified_deployments,
        reject_next_reads,
    }
}

/// Incompatible labels and a failed Kubernetes read defer automatic requests.
/// A later successful check queues the update.
#[crate::sqlx_test]
async fn automatic_update_defers_incompatible_or_unavailable_dpf(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    for observation in [LabelObservation::Stale, LabelObservation::Unavailable] {
        *f.labels.lock().unwrap() = observation;
        let snapshots = get_all_snapshots(&f.env).await;
        let started = f
            .updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap();
        assert!(started.is_empty(), "{observation:?}");
        let mut txn = f.env.db_txn().await;
        assert!(
            f.host
                .dpu()
                .db_machine(&mut txn)
                .await
                .reprovision_requested
                .is_none(),
            "{observation:?}"
        );
        assert!(
            matches!(
                f.host.host().db_machine(&mut txn).await.state.value,
                ManagedHostState::Ready
            ),
            "{observation:?}"
        );
        txn.commit().await.unwrap();
    }
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 2);
    *f.labels.lock().unwrap() = LabelObservation::Current;
    let snapshots = get_all_snapshots(&f.env).await;
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    let mut txn = f.env.db_txn().await;
    let request = f
        .host
        .dpu()
        .db_machine(&mut txn)
        .await
        .reprovision_requested
        .unwrap();
    assert!(request.started_at.is_none());
    txn.commit().await.unwrap();
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 3);
}

/// The compatibility budget covers database reads and releases the reader
/// connection even while the database lock that blocked the read remains held.
#[crate::sqlx_test]
async fn automatic_update_times_out_blocked_inventory_and_recovers(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let other_host = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_bf4(&f.env))
        .await
        .expect("timed out provisioning the second deadline candidate");
    f.outdated.lock().unwrap().push(OutdatedDpfDpu {
        dpu_machine_id: other_host.dpu_ids[0],
        target_source: "updated-bfb".to_string(),
    });
    f.label_reads.store(0, Ordering::SeqCst);
    let snapshots = get_all_snapshots(&f.env).await;
    let reader_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(60))
        .after_connect(|connection, _| {
            Box::pin(async move {
                // Production must install its own server bound.
                sqlx::query(
                    "SELECT set_config('statement_timeout', '0', false), \
                     set_config('lock_timeout', '0', false)",
                )
                .execute(connection)
                .await?;
                Ok(())
            })
        })
        .connect_with(f.env.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let mut blocker = f.env.db_txn().await;
    sqlx::query("LOCK TABLE expected_machines IN ACCESS EXCLUSIVE MODE")
        .execute(blocker.as_mut())
        .await
        .unwrap();
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();
    let updating_hosts = HashSet::new();
    let admission = f
        .updater
        .start_updates(&reader_pool, 1, &updating_hosts, &snapshots);
    tokio::pin!(admission);
    let started_at = tokio::time::Instant::now();
    let blocked_pid = tokio::select! {
        result = &mut admission => panic!("inventory read did not wait for the table lock: {result:?}"),
        pid = wait_for_blocked_query(&f.env.pool, blocker_pid, "expected_machines") => pid,
    };
    assert!(
        timeout(Duration::from_secs(35), &mut admission)
            .await
            .expect("compatibility must time out before the pool's 60-second acquire timeout")
            .unwrap()
            .is_empty()
    );
    assert!(
        started_at.elapsed() >= Duration::from_secs(29),
        "blocked inventory must consume the compatibility deadline rather than fail early"
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 0);
    let connection = timeout(Duration::from_secs(5), reader_pool.acquire())
        .await
        .expect("cancelled inventory read must release the pool slot while its lock is held")
        .unwrap();
    drop(connection);
    timeout(Duration::from_secs(5), async {
        loop {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pid = $1)")
                    .bind(blocked_pid)
                    .fetch_one(&f.env.pool)
                    .await
                    .unwrap();
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the cancelled backend must exit while its table blocker remains held");
    let mut txn = f.env.db_txn().await;
    assert!(
        f.host
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_none()
    );
    txn.commit().await.unwrap();
    blocker.commit().await.unwrap();

    // The server cap must preserve an operator's tighter statement deadline.
    sqlx::query("SELECT set_config('statement_timeout', '100ms', false)")
        .execute(&reader_pool)
        .await
        .unwrap();
    let mut blocker = f.env.db_txn().await;
    sqlx::query("LOCK TABLE expected_machines IN ACCESS EXCLUSIVE MODE")
        .execute(blocker.as_mut())
        .await
        .unwrap();
    let error = timeout(
        Duration::from_secs(2),
        validate_reprovision_compatibility(
            &snapshots[&f.host.id],
            &f.host.dpu_ids,
            &reader_pool,
            &f.env.config.rack_profiles,
            f.updater.dpf.as_deref().unwrap(),
        ),
    )
    .await
    .expect("a shorter operator timeout must remain effective")
    .unwrap_err();
    assert!(error.to_string().contains("statement timeout"), "{error}");
    blocker.commit().await.unwrap();

    let admitted = timeout(
        TEST_TIMEOUT,
        f.updater
            .start_updates(&reader_pool, 1, &HashSet::new(), &snapshots),
    )
    .await
    .expect("admission must recover after the inventory lock is released")
    .unwrap();
    assert_eq!(admitted.len(), 1);
    let mut txn = f.env.db_txn().await;
    for host in [&f.host, &other_host] {
        assert_eq!(
            host.dpu()
                .db_machine(&mut txn)
                .await
                .reprovision_requested
                .is_some(),
            admitted.contains(&host.id),
            "{}",
            host.id
        );
    }
    txn.commit().await.unwrap();
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 1);
    let restored_timeout: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&reader_pool)
        .await
        .unwrap();
    assert_eq!(restored_timeout, "0");
    reader_pool.close().await;
}

/// A DPF-ingested BF3 with an old BMC uses network installation, so DPF labels
/// must not block its automatic update.
#[crate::sqlx_test]
async fn automatic_update_skips_dpf_preflight_for_legacy_reprovision(pool: sqlx::PgPool) {
    let f = fixture_with_deployment(pool, DpuDeploymentType::Bf3, 1).await;
    let mut txn = f.env.db_txn().await;
    db::machine_topology::update_firmware_version_by_machine_id(
        txn.as_mut(),
        &f.host.dpu_ids[0].to_machine_id(),
        "BF-24.04",
        "test-bios",
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    *f.labels.lock().unwrap() = LabelObservation::Stale;
    let snapshots = get_all_snapshots(&f.env).await;
    let snapshot = &snapshots[&f.host.id];
    assert!(snapshot.host_snapshot.config.dpf.used_for_ingestion);
    assert_eq!(
        snapshot.dpu_snapshots[0]
            .status
            .bmc_info
            .firmware_version
            .as_deref(),
        Some("BF-24.04")
    );
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 0);
    let mut txn = f.env.db_txn().await;
    assert!(
        f.host
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_some_and(|request| request.started_at.is_none())
    );
    txn.commit().await.unwrap();
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    let ManagedHostState::DPUReprovision { dpu_states } = host.state.value else {
        panic!("expected legacy reprovision, got {:?}", host.state.value);
    };
    assert_eq!(
        dpu_states.states.get(&f.host.dpu_ids[0]),
        Some(&ReprovisionState::WaitingForNetworkInstall)
    );
    assert!(dpu.reprovision_requested.unwrap().started_at.is_some());
    assert_ne!(
        dpu.status.last_reboot_requested,
        snapshot.dpu_snapshots[0].status.last_reboot_requested
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 0);
}

/// Disabling DPF at runtime preserves Ready's legacy reprovision route for
/// a DPF-ingested host, even when the installed node's labels are incompatible.
#[crate::sqlx_test]
async fn runtime_dpf_disable_preserves_legacy_ready_reprovision(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let snapshots = get_all_snapshots(&f.env).await;
    assert!(
        snapshots[&f.host.id]
            .host_snapshot
            .config
            .dpf
            .used_for_ingestion
    );
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    let mut site_config = f.env.config.machine_state_handler_site_config();
    site_config.dpf_enabled = false;
    let mut services = f.env.machine_state_handler_services();
    services.site_config = Arc::new(site_config);
    let mut controller = StateController::<MachineStateControllerIO>::builder()
        .database(
            f.env.pool.clone(),
            f.env.api.work_lock_manager_handle.clone(),
        )
        .processor_id(uuid::Uuid::new_v4().to_string())
        .services(Arc::new(services))
        .state_handler(Arc::new(f.env.machine_state_handler.clone()))
        .io(Arc::new(MachineStateControllerIO {
            host_health: f.env.config.host_health,
            sla_config: model::machine::slas::MachineSlaConfig::new(
                f.env.config.machine_state_controller.failure_retry_time,
            ),
        }))
        .build_for_manual_iterations(CancellationToken::new())
        .unwrap();
    *f.labels.lock().unwrap() = LabelObservation::Stale;
    let timepoint = f.env.redfish_sim.timepoint();
    timeout(TEST_TIMEOUT, controller.run_single_iteration())
        .await
        .expect("legacy reprovision did not start after runtime DPF disable");

    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    let ManagedHostState::DPUReprovision { dpu_states } = host.state.value else {
        panic!("expected legacy reprovision, got {:?}", host.state.value);
    };
    assert_eq!(
        dpu_states.states.get(&f.host.dpu_ids[0]),
        Some(&ReprovisionState::InstallDpuOs {
            substate: InstallDpuOsState::InstallingBFB,
        })
    );
    assert!(dpu.reprovision_requested.unwrap().started_at.is_some());
    assert_ne!(
        dpu.status.last_reboot_requested,
        snapshots[&f.host.id].dpu_snapshots[0]
            .status
            .last_reboot_requested
    );
    assert!(
        host.health_reports
            .merges
            .contains_key(HOST_UPDATE_HEALTH_REPORT_SOURCE)
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 1);
    assert!(
        f.env
            .redfish_sim
            .actions_since(&timepoint)
            .for_host(&dpu.status.bmc_info.ip.unwrap().to_string())
            .iter()
            .any(|action| matches!(action, RedfishSimAction::Power(_)))
    );
}

/// Automatic admission preserves a manual request committed after its snapshot
/// and does not report that update as started.
#[crate::sqlx_test]
async fn automatic_update_preserves_concurrent_manual_request(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let snapshots = get_all_snapshots(&f.env).await;
    let mut manual = f.env.db_txn().await;
    db::machine::trigger_dpu_reprovisioning_request(
        &f.host.dpu_ids[0],
        manual.as_mut(),
        rpc::forge::UpdateInitiator::AdminCli.as_str_name(),
        false,
    )
    .await
    .unwrap();
    let expected_request = f
        .host
        .dpu()
        .db_machine(&mut manual)
        .await
        .reprovision_requested;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(manual.as_mut())
        .await
        .unwrap();
    let updating_hosts = HashSet::new();
    let admission =
        capture_logs_async(
            f.updater
                .start_updates(&f.env.pool, 1, &updating_hosts, &snapshots),
        );
    tokio::pin!(admission);
    tokio::select! {
        result = &mut admission => panic!("automatic request did not wait for the manual writer: {result:?}"),
        _ = wait_for_blocked_query(&f.env.pool, blocker_pid, "SELECT reprovisioning_requested IS NOT NULL") => {}
    }
    manual.commit().await.unwrap();
    let (admitted, logs) = timeout(TEST_TIMEOUT, admission)
        .await
        .expect("automatic admission did not finish after the manual request committed");
    assert!(admitted.unwrap().is_empty());
    assert!(logs.iter().all(|log| !matches!(
        log.field("event_name"),
        Some("firmware_update_failed" | "firmware_update_progress")
    )));
    let mut txn = f.env.db_txn().await;
    assert_eq!(
        f.host
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested,
        expected_request
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 1);
}

/// A blocked candidate leaves the update slot available for another host.
/// Reject the first node read so map iteration order cannot affect the case.
#[crate::sqlx_test]
async fn incompatible_dpf_update_does_not_consume_capacity(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let mut other_hosts = Vec::new();
    for _ in 0..2 {
        let host = timeout(TEST_TIMEOUT, create_managed_host_with_dpf_bf4(&f.env))
            .await
            .expect("timed out during initial provisioning");
        f.outdated.lock().unwrap().push(OutdatedDpfDpu {
            dpu_machine_id: host.dpu_ids[0],
            target_source: "updated-bfb".to_string(),
        });
        other_hosts.push(host);
    }
    f.reject_next_reads.store(1, Ordering::SeqCst);
    f.label_reads.store(0, Ordering::SeqCst);
    let snapshots = get_all_snapshots(&f.env).await;
    let started = f
        .updater
        .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
        .await
        .unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 2);
    let mut txn = f.env.db_txn().await;
    for host in std::iter::once(&f.host).chain(&other_hosts) {
        assert_eq!(
            host.dpu()
                .db_machine(&mut txn)
                .await
                .reprovision_requested
                .is_some(),
            started.contains(&host.id)
        );
    }
}

/// A request admitted earlier must be checked again if labels or Kubernetes
/// availability change before the `Ready` controller starts it.
#[crate::sqlx_test]
async fn queued_dpf_update_rechecks_before_restart(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let timepoint = f.env.redfish_sim.timepoint();
    let mut txn = f.env.db_txn().await;
    let reboot_before = f
        .host
        .dpu()
        .db_machine(&mut txn)
        .await
        .status
        .last_reboot_requested;
    txn.commit().await.unwrap();

    for (observation, expected_reason) in [
        (
            LabelObservation::Stale,
            "reconcile expected hardware inventory and deployment selector labels before reprovisioning",
        ),
        (LabelObservation::Unavailable, "injected read failure"),
    ] {
        *f.labels.lock().unwrap() = LabelObservation::Current;
        let snapshots = get_all_snapshots(&f.env).await;
        assert_eq!(
            f.updater
                .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
                .await
                .unwrap(),
            HashSet::from([f.host.id.into()])
        );
        let mut txn = f.env.db_txn().await;
        assert_eq!(
            f.updater
                .get_updates_in_progress(txn.as_mut())
                .await
                .unwrap(),
            HashSet::from([f.host.id.into()])
        );
        txn.commit().await.unwrap();
        *f.labels.lock().unwrap() = observation;
        f.env.run_machine_state_controller_iteration().await;
        let mut txn = f.env.db_txn().await;
        let host = f.host.host().db_machine(&mut txn).await;
        let dpu = f.host.dpu().db_machine(&mut txn).await;
        assert!(
            matches!(host.state.value, ManagedHostState::Ready),
            "{observation:?}"
        );
        assert!(
            matches!(host.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains(expected_reason)),
            "{observation:?}"
        );
        assert!(dpu.reprovision_requested.is_none(), "{observation:?}");
        assert_eq!(
            dpu.status.last_reboot_requested, reboot_before,
            "{observation:?}"
        );
        assert!(
            f.updater
                .get_updates_in_progress(txn.as_mut())
                .await
                .unwrap()
                .is_empty(),
            "{observation:?}"
        );
        txn.commit().await.unwrap();
    }
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 4);
    assert!(
        f.env
            .redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::Power(_)))
    );

    *f.labels.lock().unwrap() = LabelObservation::Current;
    let snapshots = get_all_snapshots(&f.env).await;
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    assert!(matches!(
        host.state.value,
        ManagedHostState::DPUReprovision { .. }
    ));
    assert!(dpu.reprovision_requested.unwrap().started_at.is_some());
    assert_ne!(dpu.status.last_reboot_requested, reboot_before);
    let dpu_bmc_ip = dpu.status.bmc_info.ip.unwrap().to_string();
    assert!(
        f.env
            .redfish_sim
            .actions_since(&timepoint)
            .for_host(&dpu_bmc_ip)
            .iter()
            .any(|action| matches!(action, RedfishSimAction::Power(_)))
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 6);
    txn.commit().await.unwrap();

    // Admission cannot prevent selector drift after the request starts.
    // Execution must still reject the incompatible node before touching DPF.
    *f.labels.lock().unwrap() = LabelObservation::Stale;
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    assert!(matches!(
        host.state.value,
        ManagedHostState::Failed { details, .. }
            if matches!(&details.cause, FailureCause::DpfProvisioning { err } if err.contains("stale labels"))
    ));
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 7);
}

/// A concurrent request change wins over both a successful compatibility
/// check and withdrawal after an incompatible result.
#[crate::sqlx_test]
async fn queued_dpf_update_preserves_concurrent_request_changes(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    for (case, observation, replacement, expected_reason) in [
        (
            "clear before claim",
            LabelObservation::Current,
            None,
            "request or host changed",
        ),
        (
            "replace before claim",
            LabelObservation::Current,
            Some(
                rpc::forge::UpdateInitiator::AdminCli
                    .as_str_name()
                    .to_string(),
            ),
            "request or host changed",
        ),
        (
            "replace before withdrawal",
            LabelObservation::Stale,
            Some(
                DpuReprovisionInitiator::Automatic(AutomaticFirmwareUpdateReference {
                    from: "replacement".to_string(),
                    to: "updated-bfb".to_string(),
                })
                .to_string(),
            ),
            "selected Bf4Generic deployment",
        ),
    ] {
        *f.labels.lock().unwrap() = LabelObservation::Current;
        let snapshots = get_all_snapshots(&f.env).await;
        let host_before = &snapshots[&f.host.id].host_snapshot;
        let reboot_before = snapshots[&f.host.id].dpu_snapshots[0]
            .status
            .last_reboot_requested;
        f.label_reads.store(0, Ordering::SeqCst);
        assert_eq!(
            f.updater
                .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
                .await
                .unwrap(),
            HashSet::from([f.host.id.into()])
        );
        let mut writer = f.env.db_txn().await;
        db::machine::lock_by_id_for_update(writer.as_mut(), &f.host.id.to_machine_id())
            .await
            .unwrap();
        let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(writer.as_mut())
            .await
            .unwrap();
        let timepoint = f.env.redfish_sim.timepoint();
        *f.labels.lock().unwrap() = observation;
        let iteration = f.env.run_machine_state_controller_iteration();
        tokio::pin!(iteration);
        tokio::select! {
            () = &mut iteration => panic!("{case}: controller did not wait to claim the host"),
            _ = wait_for_blocked_query(&f.env.pool, blocker_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => {}
        }
        assert_eq!(f.label_reads.load(Ordering::SeqCst), 2, "{case}");
        if let Some(initiator) = replacement {
            let previous = f
                .host
                .dpu()
                .db_machine(&mut writer)
                .await
                .reprovision_requested
                .unwrap();
            db::machine::trigger_dpu_reprovisioning_request(
                &f.host.dpu_ids[0],
                writer.as_mut(),
                &initiator,
                false,
            )
            .await
            .unwrap();
            let replacement = f
                .host
                .dpu()
                .db_machine(&mut writer)
                .await
                .reprovision_requested
                .unwrap();
            assert_ne!(previous.requested_at, replacement.requested_at, "{case}");
        } else {
            db::machine::clear_dpu_reprovisioning_request(
                writer.as_mut(),
                &f.host.dpu_ids[0],
                true,
            )
            .await
            .unwrap();
        }
        let expected_request = f
            .host
            .dpu()
            .db_machine(&mut writer)
            .await
            .reprovision_requested;
        writer.commit().await.unwrap();
        timeout(TEST_TIMEOUT, iteration)
            .await
            .expect("controller did not finish after the request changed");

        let mut txn = f.env.db_txn().await;
        let host = f.host.host().db_machine(&mut txn).await;
        let dpu = f.host.dpu().db_machine(&mut txn).await;
        assert_eq!(host.state.value, ManagedHostState::Ready, "{case}");
        assert_eq!(host.state.version, host_before.state.version, "{case}");
        assert_eq!(host.health_reports, host_before.health_reports, "{case}");
        assert!(
            matches!(host.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains(expected_reason)),
            "{case}"
        );
        assert_eq!(dpu.reprovision_requested, expected_request, "{case}");
        assert_eq!(dpu.status.last_reboot_requested, reboot_before, "{case}");
        assert!(
            f.env
                .redfish_sim
                .actions_since(&timepoint)
                .all_hosts()
                .iter()
                .all(|action| !matches!(action, RedfishSimAction::Power(_))),
            "{case}"
        );
        db::machine::clear_dpu_reprovisioning_request(txn.as_mut(), &f.host.dpu_ids[0], false)
            .await
            .unwrap();
        sqlx::query("UPDATE machines SET controller_state_outcome = NULL WHERE id = $1")
            .bind(f.host.id)
            .execute(txn.as_mut())
            .await
            .unwrap();
        txn.commit().await.unwrap();
    }
}

/// Allocation can commit without changing the host's Ready state or version.
/// The reprovision claim must recheck the instance after acquiring that row.
#[crate::sqlx_test]
async fn queued_dpf_update_preserves_concurrent_allocation(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let segment_id = f.env.create_vpc_and_tenant_segment().await;
    let snapshots = get_all_snapshots(&f.env).await;
    let host_before = &snapshots[&f.host.id].host_snapshot;
    let reboot_before = snapshots[&f.host.id].dpu_snapshots[0]
        .status
        .last_reboot_requested;
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    let mut txn = f.env.db_txn().await;
    let expected_request = f
        .host
        .dpu()
        .db_machine(&mut txn)
        .await
        .reprovision_requested;
    txn.commit().await.unwrap();

    // Allocation owns the host row before inserting the instance. Hold only
    // that insert so the controller can observe Ready and finish preflight.
    let mut blocker = f.env.db_txn().await;
    sqlx::query("LOCK TABLE instances IN SHARE MODE")
        .execute(blocker.as_mut())
        .await
        .unwrap();
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();
    let allocation =
        f.env
            .api
            .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
                machine_id: Some(f.host.host().id),
                config: Some(rpc::forge::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(segment_id)),
                    ..Default::default()
                }),
                ..Default::default()
            }));
    tokio::pin!(allocation);
    let allocation_pid = tokio::select! {
        result = &mut allocation => panic!("allocation did not wait to insert the instance: {result:?}"),
        pid = wait_for_blocked_query(&f.env.pool, blocker_pid, "INSERT INTO instances") => pid,
    };
    let timepoint = f.env.redfish_sim.timepoint();
    let iteration = f.env.run_machine_state_controller_iteration();
    tokio::pin!(iteration);
    tokio::select! {
        result = &mut allocation => panic!("allocation passed the blocked instance insert: {result:?}"),
        () = &mut iteration => panic!("controller did not wait for allocation's host lock"),
        _ = wait_for_blocked_query(&f.env.pool, allocation_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => {}
    }
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 2);
    blocker.commit().await.unwrap();
    let (allocation, ()) = timeout(TEST_TIMEOUT, async { tokio::join!(allocation, iteration) })
        .await
        .expect("allocation and controller did not finish after releasing the instance insert");
    let instance_id = allocation.unwrap().into_inner().id.unwrap();

    let mut txn = f.env.db_txn().await;
    let snapshot = f.host.snapshot(&mut txn).await;
    assert_eq!(snapshot.instance.as_ref().unwrap().id, instance_id);
    assert_eq!(snapshot.host_snapshot.state.value, ManagedHostState::Ready);
    assert_eq!(
        snapshot.host_snapshot.state.version,
        host_before.state.version
    );
    assert_eq!(
        snapshot.host_snapshot.health_reports,
        host_before.health_reports
    );
    assert!(
        matches!(&snapshot.host_snapshot.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("request or host changed"))
    );
    assert_eq!(
        snapshot.dpu_snapshots[0].reprovision_requested,
        expected_request
    );
    assert_eq!(
        snapshot.dpu_snapshots[0].status.last_reboot_requested,
        reboot_before
    );
    assert!(
        f.env
            .redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::Power(_)))
    );
}

/// A DPU-first writer must not deadlock either claiming or withdrawing an
/// automatic request. Both paths release the host and preserve pending work.
#[crate::sqlx_test]
async fn queued_dpf_update_defers_contended_dpu_locks(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let snapshots = get_all_snapshots(&f.env).await;
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    for observation in [LabelObservation::Current, LabelObservation::Stale] {
        let mut writer = f.env.db_txn().await;
        db::machine::lock_by_id_for_update(writer.as_mut(), &f.host.dpu_ids[0].into())
            .await
            .unwrap();
        let expected = f
            .host
            .dpu()
            .db_machine(&mut writer)
            .await
            .reprovision_requested;
        *f.labels.lock().unwrap() = observation;
        let timepoint = f.env.redfish_sim.timepoint();
        timeout(
            Duration::from_secs(5),
            f.env.run_machine_state_controller_iteration(),
        )
        .await
        .expect("DPU contention must defer without waiting on its writer");
        // A health writer can now acquire the host while still owning its DPU.
        sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
            .bind(f.host.id)
            .execute(writer.as_mut())
            .await
            .unwrap();
        let host = f.host.host().db_machine(&mut writer).await;
        let dpu = f.host.dpu().db_machine(&mut writer).await;
        assert_eq!(host.state.value, ManagedHostState::Ready, "{observation:?}");
        assert!(
            matches!(
                host.controller_state_outcome,
                Some(PersistentStateHandlerOutcome::Wait { .. })
            ),
            "{observation:?}"
        );
        assert_eq!(dpu.reprovision_requested, expected, "{observation:?}");
        assert_eq!(
            dpu.status.last_reboot_requested,
            snapshots[&f.host.id].dpu_snapshots[0]
                .status
                .last_reboot_requested,
            "{observation:?}"
        );
        assert_eq!(
            host.health_reports, snapshots[&f.host.id].host_snapshot.health_reports,
            "{observation:?}"
        );
        assert!(
            f.env
                .redfish_sim
                .actions_since(&timepoint)
                .all_hosts()
                .iter()
                .all(|action| !matches!(action, RedfishSimAction::Power(_))),
            "{observation:?}"
        );
        sqlx::query("UPDATE machines SET controller_state_outcome = NULL WHERE id = $1")
            .bind(f.host.id)
            .execute(writer.as_mut())
            .await
            .unwrap();
        writer.commit().await.unwrap();
    }
    *f.labels.lock().unwrap() = LabelObservation::Current;
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    assert!(matches!(
        f.host.host().db_machine(&mut txn).await.state.value,
        ManagedHostState::DPUReprovision { .. }
    ));
    assert!(
        f.host
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .unwrap()
            .started_at
            .is_some()
    );
}

/// Ready requests committed during compatibility keep DPU reprovisioning unstarted.
/// Their writers do not change the controller-state version checked by the claim.
#[crate::sqlx_test]
async fn queued_dpf_update_rechecks_pending_work_before_claim(pool: sqlx::PgPool) {
    #[derive(Clone, Copy, Debug)]
    enum PendingWork {
        HostReprovision,
        MachineValidation,
        SkuVerification,
        Decommission,
    }

    type RequestCheck = fn(&Machine<StableHostMachineId>) -> bool;
    let cases: [(PendingWork, RequestCheck); 4] = [
        (PendingWork::HostReprovision, |host| {
            host.host_reprovision_requested
                .as_ref()
                .is_some_and(|request| request.started_at.is_none())
        }),
        (PendingWork::MachineValidation, |host| {
            host.on_demand_machine_validation_request == Some(true)
                && host.on_demand_machine_validation_id.is_some()
        }),
        (PendingWork::SkuVerification, |host| {
            host.config.hw_sku.is_some()
                && host.status.hw_sku.as_ref().is_some_and(|status| {
                    status
                        .verify_request_time
                        .is_some_and(|time| time > host.state.version.timestamp())
                })
        }),
        (PendingWork::Decommission, |host| {
            host.decommission_requested
        }),
    ];
    let mut config = get_config();
    config.bom_validation.enabled = true;
    let f = fixture_with_config(pool, DpuDeploymentType::Bf4Generic, 1, config).await;

    for (work, check_request) in cases {
        let snapshots = get_all_snapshots(&f.env).await;
        let host_before = &snapshots[&f.host.id].host_snapshot;
        assert!(host_before.config.hw_sku.is_some(), "{work:?}");
        f.label_reads.store(0, Ordering::SeqCst);
        assert_eq!(
            f.updater
                .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
                .await
                .unwrap(),
            HashSet::from([f.host.id.into()]),
            "{work:?}"
        );
        let mut writer = f.env.db_txn().await;
        db::machine::lock_by_id_for_update(writer.as_mut(), &f.host.id.into())
            .await
            .unwrap();
        let expected_request = f
            .host
            .dpu()
            .db_machine(&mut writer)
            .await
            .reprovision_requested;
        let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(writer.as_mut())
            .await
            .unwrap();
        let timepoint = f.env.redfish_sim.timepoint();
        let iteration = f.env.run_machine_state_controller_iteration();
        tokio::pin!(iteration);
        tokio::select! {
            () = &mut iteration => panic!("{work:?}: controller did not wait for the host writer"),
            _ = wait_for_blocked_query(&f.env.pool, blocker_pid, "SELECT id FROM machines WHERE id = $1 FOR UPDATE") => {}
        }
        assert_eq!(f.label_reads.load(Ordering::SeqCst), 2, "{work:?}");
        match work {
            PendingWork::HostReprovision => {
                db::host_machine_update::trigger_host_reprovisioning_request(
                    writer.as_mut(),
                    "test",
                    &f.host.id.into(),
                )
                .await
                .unwrap();
            }
            PendingWork::MachineValidation => {
                db::machine_validation::create_new_run(
                    writer.as_mut(),
                    &f.host.id.into(),
                    MachineValidationContext::OnDemand,
                    MachineValidationFilter::default(),
                )
                .await
                .unwrap();
                db::machine::set_machine_validation_request(
                    writer.as_mut(),
                    &f.host.id.into(),
                    true,
                )
                .await
                .unwrap();
            }
            PendingWork::SkuVerification => {
                db::machine::update_sku_status_verify_request_time(
                    writer.as_mut(),
                    &f.host.id.into(),
                )
                .await
                .unwrap();
            }
            PendingWork::Decommission => {
                db::machine::set_decommission_requested(writer.as_mut(), f.host.id)
                    .await
                    .unwrap();
            }
        }
        let requested_host = f.host.host().db_machine(&mut writer).await;
        writer.commit().await.unwrap();
        timeout(TEST_TIMEOUT, iteration)
            .await
            .expect("controller did not observe the committed request");
        let mut txn = f.env.db_txn().await;
        let host = f.host.host().db_machine(&mut txn).await;
        let dpu = f.host.dpu().db_machine(&mut txn).await;
        assert_eq!(host.state.value, ManagedHostState::Ready, "{work:?}");
        assert_eq!(host.state.version, host_before.state.version, "{work:?}");
        assert!(check_request(&host), "{work:?}");
        assert!(
            matches!(host.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("request or host changed")),
            "{work:?}"
        );
        assert_eq!(dpu.reprovision_requested, expected_request, "{work:?}");
        assert!(
            dpu.reprovision_requested.unwrap().started_at.is_none(),
            "{work:?}"
        );
        assert_eq!(
            dpu.status.last_reboot_requested,
            snapshots[&f.host.id].dpu_snapshots[0]
                .status
                .last_reboot_requested,
            "{work:?}"
        );
        assert_eq!(
            host.health_reports, requested_host.health_reports,
            "{work:?}"
        );
        assert!(
            f.env
                .redfish_sim
                .actions_since(&timepoint)
                .all_hosts()
                .iter()
                .all(|action| !matches!(action, RedfishSimAction::Power(_))),
            "{work:?}"
        );
        match work {
            PendingWork::HostReprovision => {
                db::host_machine_update::clear_host_reprovisioning_request(
                    txn.as_mut(),
                    &f.host.id.into(),
                )
                .await
                .unwrap();
            }
            PendingWork::MachineValidation => {
                db::machine::set_machine_validation_request(txn.as_mut(), &f.host.id.into(), false)
                    .await
                    .unwrap();
            }
            PendingWork::SkuVerification => {
                sqlx::query("UPDATE machines SET hw_sku_status = $2 WHERE id = $1")
                    .bind(f.host.id)
                    .bind(host_before.status.hw_sku.as_ref().map(sqlx::types::Json))
                    .execute(txn.as_mut())
                    .await
                    .unwrap();
            }
            PendingWork::Decommission => {}
        }
        if !matches!(work, PendingWork::Decommission) {
            db::machine::clear_dpu_reprovisioning_request(txn.as_mut(), &f.host.dpu_ids[0], false)
                .await
                .unwrap();
            sqlx::query("UPDATE machines SET controller_state_outcome = NULL WHERE id = $1")
                .bind(f.host.id)
                .execute(txn.as_mut())
                .await
                .unwrap();
        }
        txn.commit().await.unwrap();
    }

    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    assert_eq!(
        host.state.value,
        ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::SuppressingSiteExplorer,
        }
    );
    assert!(!host.decommission_requested);
    assert!(
        f.host
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .unwrap()
            .started_at
            .is_none()
    );
}

/// Ready preserves a manual request while labels are stale, then starts the
/// same request after recovery without requiring the operator to queue it again.
#[crate::sqlx_test]
async fn manual_dpf_update_defers_and_recovers(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let mut txn = f.env.db_txn().await;
    db::machine::trigger_dpu_reprovisioning_request(
        &f.host.dpu_ids[0],
        txn.as_mut(),
        rpc::forge::UpdateInitiator::AdminCli.as_str_name(),
        true,
    )
    .await
    .unwrap();
    let before = f.host.dpu().db_machine(&mut txn).await;
    txn.commit().await.unwrap();
    *f.labels.lock().unwrap() = LabelObservation::Stale;
    let timepoint = f.env.redfish_sim.timepoint();
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    assert_eq!(host.state.value, ManagedHostState::Ready);
    assert!(
        matches!(host.controller_state_outcome, Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason.contains("reconcile expected hardware inventory and deployment selector labels before reprovisioning"))
    );
    assert_eq!(dpu.reprovision_requested, before.reprovision_requested);
    assert_eq!(
        dpu.status.last_reboot_requested,
        before.status.last_reboot_requested
    );
    assert!(
        f.env
            .redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .iter()
            .all(|action| !matches!(action, RedfishSimAction::Power(_)))
    );
    txn.commit().await.unwrap();
    *f.labels.lock().unwrap() = LabelObservation::Current;
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    assert!(matches!(
        host.state.value,
        ManagedHostState::DPUReprovision { .. }
    ));
    let request = dpu.reprovision_requested.unwrap();
    assert_eq!(
        request.requested_at,
        before.reprovision_requested.unwrap().requested_at
    );
    assert!(request.started_at.is_some());
    assert_ne!(
        dpu.status.last_reboot_requested,
        before.status.last_reboot_requested
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 2);
}

/// A later DPU's existing request rolls back earlier writes in the same host
/// batch, even when the caller commits its surrounding transaction.
#[crate::sqlx_test]
async fn automatic_dpf_update_rolls_back_rejected_batch(pool: sqlx::PgPool) {
    let f = fixture_with_deployment(pool, DpuDeploymentType::Bf4Generic, 2).await;
    let mut txn = f.env.db_txn().await;
    db::machine::trigger_dpu_reprovisioning_request(
        &f.host.dpu_ids[1],
        txn.as_mut(),
        rpc::forge::UpdateInitiator::AdminCli.as_str_name(),
        false,
    )
    .await
    .unwrap();
    let expected = f
        .host
        .dpu_n(1)
        .db_machine(&mut txn)
        .await
        .reprovision_requested;
    let updates = f
        .host
        .dpu_ids
        .iter()
        .map(|dpu_id| DpuMachineUpdate {
            host_machine_id: f.host.id.into(),
            dpu_machine_id: *dpu_id,
            firmware_version: "updated-bfb".to_string(),
            dpf_managed: true,
        })
        .collect::<Vec<_>>();
    assert!(
        !db::dpu_machine_update::trigger_reprovisioning_for_managed_host(txn.as_mut(), &updates)
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();
    let mut txn = f.env.db_txn().await;
    assert!(
        f.host
            .dpu_n(0)
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_none()
    );
    assert_eq!(
        f.host
            .dpu_n(1)
            .db_machine(&mut txn)
            .await
            .reprovision_requested,
        expected
    );
}

/// A failed restart retains its committed start and can retry after labels
/// change, without running admission again or replacing the start time.
#[crate::sqlx_test]
async fn started_dpf_update_retry_preserves_start_time(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    let snapshots = get_all_snapshots(&f.env).await;
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    let dpu_before = &snapshots[&f.host.id].dpu_snapshots[0];
    let dpu_bmc_ip = dpu_before.status.bmc_info.ip.unwrap().to_string();
    f.env
        .redfish_sim
        .set_power_state(&dpu_bmc_ip, PowerState::On);
    f.env.redfish_sim.fail_next_power_action(
        SystemPowerControl::ForceRestart,
        "transient DPU restart failure",
    );
    let failed_attempt = f.env.redfish_sim.timepoint();
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    assert_eq!(host.state.value, ManagedHostState::Ready);
    assert!(matches!(
        host.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Error { err, .. })
            if err.contains("transient DPU restart failure")
    ));
    assert!(
        host.health_reports
            .merges
            .contains_key(HOST_UPDATE_HEALTH_REPORT_SOURCE)
    );
    let started_at = dpu.reprovision_requested.unwrap().started_at.unwrap();
    assert_eq!(
        dpu.status.last_reboot_requested,
        dpu_before.status.last_reboot_requested
    );
    txn.commit().await.unwrap();
    assert_eq!(
        f.env
            .redfish_sim
            .actions_since(&failed_attempt)
            .for_host(&dpu_bmc_ip),
        vec![RedfishSimAction::PowerFailed(
            SystemPowerControl::ForceRestart
        )]
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 2);

    *f.labels.lock().unwrap() = LabelObservation::Stale;
    let timepoint = f.env.redfish_sim.timepoint();
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    let dpu = f.host.dpu().db_machine(&mut txn).await;
    assert!(matches!(
        host.state.value,
        ManagedHostState::DPUReprovision { .. }
    ));
    assert_eq!(
        dpu.reprovision_requested.unwrap().started_at,
        Some(started_at)
    );
    assert_eq!(f.label_reads.load(Ordering::SeqCst), 2);
    assert_ne!(
        dpu.status.last_reboot_requested,
        dpu_before.status.last_reboot_requested
    );
    assert_eq!(
        f.env
            .redfish_sim
            .actions_since(&timepoint)
            .for_host(&dpu_bmc_ip),
        vec![RedfishSimAction::Power(SystemPowerControl::ForceRestart)]
    );
}

/// Mixed per-DPU deployment selections fail before any shared-node label check.
#[crate::sqlx_test]
async fn dpf_update_compatibility_rejects_mixed_deployments(pool: sqlx::PgPool) {
    let f = fixture_with_deployment(pool, DpuDeploymentType::Bf4Generic, 2).await;
    let first_dpu = f.host.dpu_ids[0];
    let mut mock = MockDpfOperations::new();
    mock.expect_deployment_type_for_dpu()
        .times(2)
        .returning(move |dpu, _| {
            Ok(if dpu.id == first_dpu {
                DpuDeploymentType::Bf3
            } else {
                DpuDeploymentType::Bf4Generic
            })
        });
    mock.expect_verify_node_labels().never();
    let snapshots = get_all_snapshots(&f.env).await;
    let error = validate_reprovision_compatibility(
        &snapshots[&f.host.id],
        &f.host.dpu_ids,
        &f.env.pool,
        &f.env.config.rack_profiles,
        &mock,
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("mixed DPF deployment types"));
}

/// For BF4, expected `CX9` inventory must select Astra before DPA discovery
/// populates interfaces; otherwise a correctly labeled Astra node looks
/// incompatible.
#[crate::sqlx_test]
async fn dpf_update_compatibility_uses_expected_astra_inventory(pool: sqlx::PgPool) {
    let f = fixture(pool).await;
    *f.labels.lock().unwrap() = LabelObservation::AstraOnly;
    let snapshots = get_all_snapshots(&f.env).await;
    let snapshot = &snapshots[&f.host.id];
    let check = async || {
        validate_reprovision_compatibility(
            snapshot,
            &f.host.dpu_ids,
            &f.env.pool,
            &f.env.config.rack_profiles,
            f.updater.dpf.as_deref().unwrap(),
        )
        .await
    };
    let error = check().await.unwrap_err();
    assert!(error.to_string().contains("selected Bf4Generic deployment"));

    let mut txn = f.env.db_txn().await;
    let mut expected = db::expected_machine::find_by_bmc_mac_address(
        txn.as_mut(),
        snapshot.host_snapshot.status.bmc_info.mac.unwrap(),
    )
    .await
    .unwrap()
    .expect("the fixture has expected inventory");
    expected.data.interfaces.push(ExpectedInterface {
        physical_ifname: Some("C1-1-L1".to_string()),
        nic_type: Some("CX9".to_string()),
        ..Default::default()
    });
    db::expected_machine::update(txn.as_mut(), &expected)
        .await
        .unwrap();
    let discovered = db::dpa_interface::find_by_machine_id(
        txn.as_mut(),
        *f.host.id,
        model::dpa_interface::DpaSearchConfig {
            only_svpc: false,
            only_astra: true,
        },
    )
    .await
    .unwrap();
    assert!(discovered.is_empty());
    txn.commit().await.unwrap();

    check().await.unwrap();

    let snapshots = get_all_snapshots(&f.env).await;
    assert_eq!(
        f.updater
            .start_updates(&f.env.pool, 1, &HashSet::new(), &snapshots)
            .await
            .unwrap(),
        HashSet::from([f.host.id.into()])
    );
    f.env.run_machine_state_controller_iteration().await;
    let mut txn = f.env.db_txn().await;
    assert!(matches!(
        f.host.host().db_machine(&mut txn).await.state.value,
        ManagedHostState::DPUReprovision { .. }
    ));
    txn.commit().await.unwrap();

    // Record only execution's selector choice. Rejecting it stops before DPF
    // resource mutations while proving execution also uses expected CX9 data.
    f.verified_deployments.lock().unwrap().clear();
    *f.labels.lock().unwrap() = LabelObservation::Stale;
    f.env.run_machine_state_controller_iteration().await;
    assert_eq!(
        *f.verified_deployments.lock().unwrap(),
        vec![DpuDeploymentType::Bf4Astra]
    );
    let mut txn = f.env.db_txn().await;
    let host = f.host.host().db_machine(&mut txn).await;
    assert!(matches!(
        host.state.value,
        ManagedHostState::Failed { details, .. }
            if matches!(&details.cause, FailureCause::DpfProvisioning { err } if err.contains("stale labels"))
    ));
}
