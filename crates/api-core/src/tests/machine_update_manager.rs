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
    start_updates_called: Arc<Mutex<i32>>,
    clear_completed_updates_called: Arc<Mutex<i32>>,
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
        _available_updates: i32,
        _updating_machines: &HashSet<HostMachineId>,
        _snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<HostMachineId>> {
        if let Ok(mut guard) = self.start_updates_called.lock() {
            (*guard) += 1;
        }
        Ok(self.updates_started.clone())
    }

    async fn clear_completed_updates(&self, _txn: &mut PgConnection) -> CarbideResult<()> {
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
            start_updates_called: Arc::new(Mutex::new(0)),
            clear_completed_updates_called: Arc::new(Mutex::new(0)),
        }
    }
    pub(in crate::tests) fn get_start_updates_called(&self) -> i32 {
        *self.start_updates_called.lock().unwrap()
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

    let mut machines_started = HashSet::default();
    machines_started.insert(host_machine_id.into());

    let module1 = Box::new(TestUpdateModule::new(vec![], machines_started));
    let module2 = Box::new(TestUpdateModule::new(vec![], HashSet::default()));

    let machine_update_manager = MachineUpdateManager::new_with_modules(
        env.pool.clone(),
        config,
        vec![module1.clone(), module2.clone()],
        env.api.work_lock_manager_handle.clone(),
    );

    machine_update_manager.run_single_iteration().await?;

    assert_eq!(module1.get_start_updates_called(), 1);
    assert_eq!(module2.get_start_updates_called(), 0);

    assert_eq!(module1.get_clear_completed_updates_called(), 1);
    assert_eq!(module2.get_clear_completed_updates_called(), 1);

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
