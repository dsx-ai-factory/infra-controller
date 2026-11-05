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
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use carbide_machine_controller::health_report::create_host_update_health_report;
use carbide_uuid::machine::HostMachineId;
use common::api_fixtures::create_test_env;
use figment::Figment;
use figment::providers::{Format, Toml};
use model::dpu_machine_update::DpuMachineUpdate;
use model::machine::ManagedHostStateSnapshot;
use model::machine_update_module::{
    AutomaticFirmwareUpdateReference, DpuReprovisionInitiator, HOST_UPDATE_HEALTH_REPORT_SOURCE,
};
use sqlx::PgConnection;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::cfg::file::CarbideConfig;
use crate::machine_update_manager::MachineUpdateManager;
use crate::machine_update_manager::machine_update_module::MachineUpdateModule;
use crate::tests::common;
use crate::tests::common::api_fixtures::create_managed_host;
use crate::{CarbideError, CarbideResult};

const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cfg/test_data");

#[derive(Clone)]
struct TestUpdateModule {
    pub(in crate::tests) updates_in_progress: Vec<HostMachineId>,
    pub(in crate::tests) updates_started: HashSet<HostMachineId>,
    start_update_budgets: Arc<Mutex<Vec<i32>>>,
    clear_completed_updates_called: Arc<Mutex<i32>>,
    cleanup_marker: Option<HostMachineId>,
}

#[async_trait]
impl MachineUpdateModule for TestUpdateModule {
    async fn get_updates_in_progress(
        &self,
        _txn: &mut PgConnection,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        Ok(self.updates_in_progress.clone().into_iter().collect())
    }

    async fn start_updates(
        &self,
        _pool: &sqlx::Pool<sqlx::Postgres>,
        _work_lock: &db::work_lock_manager::WorkLock,
        available_updates: i32,
        _updating_machines: &HashSet<HostMachineId>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        self.start_update_budgets
            .lock()
            .unwrap()
            .push(available_updates);
        Ok(self.updates_started.clone())
    }

    async fn clear_completed_updates(&self, txn: &mut PgConnection) -> CarbideResult<()> {
        if let Some(host_id) = self.cleanup_marker {
            let query = "UPDATE machines SET update_complete = true WHERE id = $1";
            sqlx::query(query)
                .bind(host_id)
                .execute(txn)
                .await
                .map_err(|error| db::DatabaseError::query(query, error))?;
        }
        if let Ok(mut guard) = self.clear_completed_updates_called.lock() {
            (*guard) += 1;
        }

        Ok(())
    }

    async fn update_metrics(
        &self,
        _pool: &sqlx::Pool<sqlx::Postgres>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<()> {
        Ok(())
    }
}

impl TestUpdateModule {
    pub(in crate::tests) fn new(
        updates_in_progress: Vec<HostMachineId>,
        updates_started: HashSet<HostMachineId>,
    ) -> Self {
        TestUpdateModule {
            updates_in_progress,
            updates_started,
            start_update_budgets: Arc::new(Mutex::new(Vec::new())),
            clear_completed_updates_called: Arc::new(Mutex::new(0)),
            cleanup_marker: None,
        }
    }
    pub(in crate::tests) fn get_start_updates_called(&self) -> usize {
        self.start_update_budgets.lock().unwrap().len()
    }

    pub(in crate::tests) fn get_clear_completed_updates_called(&self) -> i32 {
        *self.clear_completed_updates_called.lock().unwrap()
    }
}

impl fmt::Display for TestUpdateModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TestUpdateModule")
    }
}

#[crate::sqlx_test]
async fn test_max_outstanding_updates(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    create_managed_host(&env).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    let config: Arc<CarbideConfig> = Arc::new(
        Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .extract()
            .unwrap(),
    );

    // Zero capacity still runs cleanup. At one slot, the first module fills
    // the budget and the next module cannot admit another host.
    for (scenario, capacity, expected_starts) in [
        ("zero capacity", 0, 0),
        ("first module fills capacity", 1, 1),
    ] {
        let mut config = (*config).clone();
        config
            .machine_updater
            .max_concurrent_machine_updates_absolute = Some(capacity);
        config
            .machine_updater
            .max_concurrent_machine_updates_percent = None;
        let module1 = Box::new(TestUpdateModule::new(
            vec![],
            HashSet::from([host_machine_id.into()]),
        ));
        let module2 = Box::new(TestUpdateModule::new(vec![], HashSet::default()));
        let manager = MachineUpdateManager::new_with_modules(
            env.pool.clone(),
            Arc::new(config),
            vec![module1.clone(), module2.clone()],
            env.api.work_lock_manager_handle.clone(),
        );

        manager.run_single_iteration().await?;

        assert_eq!(
            module1.get_start_updates_called(),
            expected_starts,
            "{scenario}"
        );
        assert_eq!(module2.get_start_updates_called(), 0, "{scenario}");
        assert_eq!(
            module1.get_clear_completed_updates_called(),
            1,
            "{scenario}"
        );
        assert_eq!(
            module2.get_clear_completed_updates_called(),
            1,
            "{scenario}"
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_percentage_update_budget_accounts_for_unhealthy_and_updating_hosts(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (updating_host_id, _) = create_managed_host(&env).await.into();
    let (unhealthy_host_id, _) = create_managed_host(&env).await.into();
    create_managed_host(&env).await;
    create_managed_host(&env).await;

    let health_report = health_report::HealthReport {
        alerts: vec![health_report::HealthProbeAlert {
            id: "FanFailure".parse()?,
            target: None,
            in_alert_since: None,
            message: "Fan failure".to_string(),
            tenant_message: None,
            classifications: vec![health_report::HealthAlertClassification::prevent_allocations()],
        }],
        ..health_report::HealthReport::empty("test-host-failure".to_string())
    };
    let mut txn = env.pool.begin().await?;
    db::machine::insert_health_report(
        &mut txn,
        &unhealthy_host_id,
        health_report::HealthReportApplyMode::Merge,
        &health_report,
        false,
    )
    .await?;
    txn.commit().await?;

    let mut config = (*env.config).clone();
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = None;
    config
        .machine_updater
        .max_concurrent_machine_updates_percent = Some(75);
    let module = Box::new(TestUpdateModule::new(
        vec![updating_host_id.into()],
        HashSet::default(),
    ));
    let manager = MachineUpdateManager::new_with_modules(
        env.pool.clone(),
        Arc::new(config),
        vec![module.clone()],
        env.api.work_lock_manager_handle.clone(),
    );

    manager.run_single_iteration().await?;

    // Three slots (75% of four hosts), minus one unhealthy host and one ongoing update.
    assert_eq!(*module.start_update_budgets.lock().unwrap(), vec![1]);

    Ok(())
}

#[crate::sqlx_test]
async fn test_remove_machine_update_markers(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    create_managed_host(&env).await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    let machine_update = DpuMachineUpdate {
        host_machine_id: host_machine_id.into(),
        dpu_machine_id,
        firmware_version: "1".to_owned(),
        dpf_managed: false,
    };

    let reference = &DpuReprovisionInitiator::Automatic(AutomaticFirmwareUpdateReference {
        from: "x".to_owned(),
        to: "y".to_owned(),
    });

    // Apply health override
    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Failed to create transaction");
    add_host_update_alert(&mut txn, &machine_update, reference).await?;
    txn.commit().await.unwrap();

    // Check that health override gets removed
    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Failed to create transaction");
    MachineUpdateManager::remove_machine_update_markers(&mut txn, &machine_update)
        .await
        .unwrap();

    let managed_host =
        db::managed_host::load_snapshot(txn.as_mut(), &host_machine_id, Default::default())
            .await
            .unwrap()
            .unwrap();
    assert!(
        !managed_host
            .host_snapshot
            .health_reports
            .merges
            .contains_key(HOST_UPDATE_HEALTH_REPORT_SOURCE)
    );
    assert!(managed_host.aggregate_health.alerts.is_empty());
    txn.commit().await.unwrap();

    Ok(())
}

#[crate::sqlx_test()]
fn test_start(pool: sqlx::PgPool) {
    let test_module = Box::new(TestUpdateModule::new(vec![], HashSet::default()));
    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default())
            .await
            .unwrap();

    let mut config: Arc<CarbideConfig> = Arc::new(
        Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .extract()
            .unwrap(),
    );

    Arc::get_mut(&mut config)
        .unwrap()
        .machine_update_run_interval = Some(1);
    let update_manager = MachineUpdateManager::new_with_modules(
        pool,
        config,
        vec![test_module.clone()],
        work_lock_manager_handle,
    );

    update_manager
        .start(&mut join_set, cancel_token.clone())
        .expect("Start failed");

    tokio::time::sleep(Duration::from_secs(4)).await;

    let start_count = test_module.get_start_updates_called();

    tokio::time::sleep(Duration::from_secs(4)).await;

    let end_count = test_module.get_start_updates_called();

    assert_ne!(start_count, end_count);

    cancel_token.cancel();

    join_set.join_all().await;

    let start_count = test_module.get_start_updates_called();

    tokio::time::sleep(Duration::from_secs(4)).await;

    let end_count = test_module.get_start_updates_called();

    assert_eq!(start_count, end_count);
}

/// A lock acquisition that fails for infrastructure reasons (here: the work
/// lock manager is gone, so the command channel is closed) must propagate out
/// of `run_single_iteration` as an error rather than being swallowed as a
/// benign yield -- it is what flips the loop's heartbeat to `outcome=error`.
#[crate::sqlx_test()]
fn test_run_single_iteration_propagates_lock_infrastructure_failure(pool: sqlx::PgPool) {
    let test_module = Box::new(TestUpdateModule::new(vec![], HashSet::default()));
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default())
            .await
            .unwrap();
    join_set.shutdown().await;

    let config: Arc<CarbideConfig> = Arc::new(
        Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .extract()
            .unwrap(),
    );
    let update_manager = MachineUpdateManager::new_with_modules(
        pool,
        config,
        vec![test_module],
        work_lock_manager_handle,
    );

    let err = update_manager
        .run_single_iteration()
        .await
        .expect_err("a dead work lock manager is an infrastructure failure, not a yield");
    assert!(
        matches!(err, CarbideError::Internal { .. }),
        "expected CarbideError::Internal, got {err:?}"
    );
    assert!(
        err.to_string()
            .contains("failed to acquire machine update work lock"),
        "unexpected message: {err}"
    );
}

/// The manager waits the interval that Version reports in its update policy.
#[crate::sqlx_test()]
fn update_manager_uses_policy_interval(pool: sqlx::PgPool) {
    let mut join_set = JoinSet::new();
    let work_lock_manager_handle =
        db::work_lock_manager::start(&mut join_set, pool.clone(), Default::default())
            .await
            .unwrap();

    for configured in [None, Some(0), Some(7)] {
        let mut config = crate::test_support::default_config::get();
        config.machine_update_run_interval = configured;
        let policy = rpc::forge::RuntimeConfig::from(config.clone())
            .machine_update_policy
            .expect("Version reports the update policy");
        let update_manager = MachineUpdateManager::new(
            pool.clone(),
            Arc::new(config),
            opentelemetry::global::meter("test"),
            work_lock_manager_handle.clone(),
            None,
        );
        assert_eq!(
            update_manager.run_interval(),
            Duration::from_secs(policy.effective_run_interval_seconds),
            "{configured:?}"
        );
    }
    join_set.shutdown().await;
}

/// Manually adds the HostUpdateInProgress health alert to a Machine
async fn add_host_update_alert(
    txn: &mut PgConnection,
    machine_update: &DpuMachineUpdate,
    reference: &model::machine_update_module::DpuReprovisionInitiator,
) -> CarbideResult<()> {
    let health_override = create_host_update_health_report(
        Some("DpuFirmware".to_string()),
        reference.to_string(),
        false,
    );

    db::machine::insert_health_report(
        txn,
        &machine_update.host_machine_id,
        health_report::HealthReportApplyMode::Merge,
        &health_override,
        false,
    )
    .await?;

    Ok(())
}

// Cards and update modules share a host slot. A second candidate must fit
// beside a multi-card NIC recovery that also has an outstanding DPU request.
#[crate::sqlx_test]
async fn admitted_nic_recovery_shares_the_host_budget(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_utils::test_support::test_meter::TestMeter;
    use model::machine::ManagedHostState;
    use model::machine::nic_firmware::NicFirmwarePhase;

    let env = create_test_env(pool).await;
    let recovering = create_managed_host(&env).await;
    let candidate = create_managed_host(&env).await;
    let mut operation = model::test_support::nic_firmware::operation();
    let mut second_card = operation.targets[0].clone();
    second_card.pci_name = "0000:02:00.0".into();
    second_card.selector =
        model::nic_firmware::NicFirmwareCardSelector::Mac("02:00:00:00:00:02".parse()?);
    operation.targets.push(second_card);
    operation.phase = NicFirmwarePhase::RestoringLockdown {
        target_index: 0,
        failure: Some("restore after a settled preparation failure".into()),
    };
    operation.validate()?;
    let recovering_state = ManagedHostState::NicFirmwareUpdating { operation };
    let mut txn = env.pool.begin().await?;
    db::machine::update_state(&mut txn, &recovering.id, &recovering_state).await?;
    candidate.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;

    let mut config = (*env.config).clone();
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(1);
    config
        .machine_updater
        .max_concurrent_machine_updates_percent = None;
    config.firmware_global.autoupdate = false;
    config
        .dpu_config
        .dpu_nic_firmware_reprovision_update_enabled = true;
    let meter = TestMeter::default();
    let manager = MachineUpdateManager::new(
        env.pool.clone(),
        Arc::new(config.clone()),
        meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    manager.run_single_iteration().await?;

    // First isolate NIC accounting: no other module occupies this host's slot.
    let mut txn = env.pool.begin().await?;
    assert_eq!(
        recovering.host().db_machine(&mut txn).await.state.value,
        recovering_state
    );
    assert!(
        candidate
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_none()
    );
    txn.commit().await?;
    assert_eq!(
        meter
            .formatted_metric("carbide_machines_in_maintenance_count")
            .as_deref(),
        Some("1")
    );
    assert_eq!(
        meter
            .formatted_metric("carbide_machine_updates_started_count")
            .as_deref(),
        Some("0")
    );
    drop(manager);

    // Add a second module's work on the same host; one remaining slot must
    // still admit the distinct candidate despite two NIC targets and overlap.
    let mut txn = env.pool.begin().await?;
    // Seed the request in Ready, then atomically restore the NIC workflow.
    // update_state mirrors host state to its DPUs, and admission requires Ready.
    db::machine::update_state(&mut txn, &recovering.id, &ManagedHostState::Ready).await?;
    recovering.update_nic_firmware_version(&mut txn).await?;
    assert!(
        db::dpu_machine_update::trigger_reprovisioning_for_managed_host(
            &mut txn,
            &[DpuMachineUpdate {
                host_machine_id: recovering.id.into(),
                dpu_machine_id: recovering.dpu().id,
                firmware_version: "11.10.1000".into(),
                dpf_managed: false,
            }],
        )
        .await?
    );
    db::machine::update_state(&mut txn, &recovering.id, &recovering_state).await?;
    let dpu_work = db::dpu_machine_update::get_reprovisioning_machines(&mut txn).await?;
    assert_eq!(dpu_work.len(), 1);
    assert_eq!(dpu_work[0].host_machine_id, recovering.id.into());
    txn.commit().await?;
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(2);
    let meter = TestMeter::default();
    let manager = MachineUpdateManager::new(
        env.pool.clone(),
        Arc::new(config),
        meter.meter(),
        env.api.work_lock_manager_handle.clone(),
        None,
    );
    manager.run_single_iteration().await?;

    let mut txn = env.pool.begin().await?;
    let recovery = recovering.host().db_machine(&mut txn).await;
    assert_eq!(recovery.state.value, recovering_state);
    assert!(
        recovering
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_some()
    );
    assert!(
        candidate
            .dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_some()
    );
    txn.commit().await?;
    assert_eq!(
        meter
            .formatted_metric("carbide_machines_in_maintenance_count")
            .as_deref(),
        Some("2")
    );
    assert_eq!(
        meter
            .formatted_metric("carbide_machine_updates_started_count")
            .as_deref(),
        Some("1")
    );
    Ok(())
}

// Cleanup must retain the manager fence until its supplied transaction commits
// the marker, even while the callback waits for a machine row.
#[crate::sqlx_test]
async fn manager_cleanup_keeps_ownership_until_marker_commit(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::tests::common::postgres::wait_for_blocked_query;

    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    sqlx::query("UPDATE machines SET update_complete = false WHERE id = $1")
        .bind(host.id)
        .execute(&env.pool)
        .await?;
    let mut module = TestUpdateModule::new(vec![], HashSet::new());
    module.cleanup_marker = Some(host.id.into());
    let mut config = (*env.config).clone();
    config
        .machine_updater
        .max_concurrent_machine_updates_absolute = Some(0);
    config
        .machine_updater
        .max_concurrent_machine_updates_percent = None;
    let manager = MachineUpdateManager::new_with_modules(
        env.pool.clone(),
        Arc::new(config),
        vec![Box::new(module)],
        env.api.work_lock_manager_handle.clone(),
    );
    let mut blocker = env.pool.begin().await?;
    db::machine::lock_by_id_for_update(blocker.as_mut(), &host.id.into()).await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    let iteration = manager.run_single_iteration();
    tokio::pin!(iteration);
    let cleanup_pid = tokio::select! {
        result = &mut iteration => panic!("cleanup escaped the machine row: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "UPDATE machines SET update_complete") => pid,
    };
    let takeover =
        sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
            .bind("MachineUpdateManager::run_single_iteration")
            .execute(&env.pool);
    tokio::pin!(takeover);
    tokio::select! {
        result = &mut iteration => panic!("cleanup escaped the machine row: {result:?}"),
        result = &mut takeover => panic!("ownership changed before cleanup committed: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, cleanup_pid, "UPDATE work_locks SET worker_id") => {},
    }
    blocker.commit().await?;
    let (iteration, takeover) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(iteration, takeover)
    })
    .await
    .expect("cleanup and takeover finish after releasing the machine row");
    iteration?;
    assert_eq!(takeover?.rows_affected(), 1);
    let committed: bool = sqlx::query_scalar("SELECT update_complete FROM machines WHERE id = $1")
        .bind(host.id)
        .fetch_one(&env.pool)
        .await?;
    assert!(committed);
    Ok(())
}
