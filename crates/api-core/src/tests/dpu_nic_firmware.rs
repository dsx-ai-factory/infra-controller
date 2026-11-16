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
use std::collections::HashSet;
use std::string::ToString;
use std::time::Duration;

use carbide_instrument::testing::MetricsCapture;
use carbide_machine_controller::health_report::create_host_update_health_report_dpufw;
use common::api_fixtures::{
    TestEnv, create_managed_host, create_managed_host_multi_dpu, create_test_env,
};
use figment::Figment;
use figment::providers::{Format, Toml};
use model::machine::LoadSnapshotOptions;
use model::machine_update_module::{
    AutomaticFirmwareUpdateReference, HOST_UPDATE_HEALTH_REPORT_SOURCE,
};

use crate::CarbideResult;
use crate::machine_update_manager::dpu_nic_firmware::DpuNicFirmwareUpdate;
use crate::machine_update_manager::machine_update_module::MachineUpdateModule;
use crate::tests::common;
use crate::tests::common::api_fixtures::TestManagedHost;
use crate::tests::common::api_fixtures::test_managed_host::TestManagedHostSnapshots;
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::dpu_machine_update::{get_all_snapshots, update_nic_firmware_version};

#[crate::sqlx_test]
async fn test_start_updates(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock("test-dpu-firmware-admission".into())
        .await?;
    let managed_host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    managed_host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };

    let snapshots = get_all_snapshots(&env).await;

    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Failed to create transaction");

    let metrics = MetricsCapture::for_registry(env.test_meter.registry());

    let started_count = dpu_nic_firmware_update
        .start_updates(&env.pool, &work_lock, 10, &HashSet::default(), &snapshots)
        .await?;

    assert_eq!(started_count.len(), 1);
    assert!(started_count.contains(&managed_host.id));

    // Starting the host's DPU updates moves the progress counter. Other tests
    // in this binary drive the same emit concurrently (MetricsCapture
    // serializes only metric-asserting tests), so assert at least one rather
    // than exactly one.
    assert!(
        metrics.counter_delta(
            "carbide_firmware_updates_total",
            &[("target", "dpu_nic"), ("phase", "started")],
        ) >= 1.0
    );

    // Check if health override is placed
    let managed_host = managed_host.snapshot(&mut txn).await;

    for dpu in managed_host.dpu_snapshots.iter() {
        let initiator = &dpu.reprovision_requested.as_ref().unwrap().initiator;
        assert!(initiator.starts_with(AutomaticFirmwareUpdateReference::REF_NAME));
    }

    Ok(())
}

/// A slow classic admission must not consume the DPF compatibility budget and
/// prevent another classic host from using the remaining update capacity.
#[crate::sqlx_test]
async fn test_classic_updates_outlive_dpf_admission_deadline(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock("test-dpu-firmware-admission".into())
        .await?;
    let hosts = [
        create_managed_host(&env).await,
        create_managed_host(&env).await,
    ];
    let mut txn = env.pool.begin().await?;
    for host in &hosts {
        host.update_nic_firmware_version(&mut txn).await?;
    }
    txn.commit().await?;
    let updater = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };
    let snapshots = get_all_snapshots(&env).await;

    // Lock both candidates so the first admission waits regardless of map order.
    let mut writer = env.pool.begin().await?;
    for host in &hosts {
        db::machine::lock_by_id_for_update(writer.as_mut(), &host.dpu().id.into()).await?;
    }
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(writer.as_mut())
        .await?;
    let updating_hosts = HashSet::new();
    let admission = updater.start_updates(&env.pool, &work_lock, 2, &updating_hosts, &snapshots);
    tokio::pin!(admission);
    tokio::select! {
        result = &mut admission => panic!("classic admission did not wait for the DPU lock: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, blocker_pid, "SELECT reprovisioning_requested IS NOT NULL") => {}
    }

    // Resume before releasing the locks or awaiting further database I/O.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    writer.commit().await?;
    let admitted = tokio::time::timeout(Duration::from_secs(30), admission)
        .await
        .expect("classic admission must finish after the DPU locks are released")?;
    assert_eq!(
        admitted,
        HashSet::from([hosts[0].id.into(), hosts[1].id.into()])
    );

    let mut txn = env.pool.begin().await?;
    for host in &hosts {
        let dpu = host.dpu().db_machine(&mut txn).await;
        let request = dpu
            .reprovision_requested
            .expect("both classic requests must be persisted");
        assert!(
            request
                .initiator
                .starts_with(AutomaticFirmwareUpdateReference::REF_NAME)
        );
        assert!(request.update_firmware);
        assert!(request.started_at.is_none());
    }
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_start_updates_with_multidpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock("test-dpu-firmware-admission".into())
        .await?;

    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let host = mh.host().rpc_machine().await;
    let dpu_ids = host.status.unwrap().associated_dpu_machine_ids;
    let dpu_machine_id = dpu_ids[0];
    let dpu_machine_id2 = dpu_ids[1];
    let mut txn = env.pool.begin().await?;
    update_nic_firmware_version(&mut txn, &dpu_machine_id, "11.10.1000").await?;
    update_nic_firmware_version(&mut txn, &dpu_machine_id2, "11.10.1000").await?;
    txn.commit().await?;

    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };

    let snapshots = get_all_snapshots(&env).await;

    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Failed to create transaction");

    let dpus_started = dpu_nic_firmware_update
        .start_updates(&env.pool, &work_lock, 10, &HashSet::default(), &snapshots)
        .await?;

    assert_eq!(dpus_started.len(), 1);
    assert!(dpus_started.contains(&mh.host().id));

    // Check if health override is placed
    let managed_host = mh.snapshot(&mut txn).await;

    for dpu in managed_host.dpu_snapshots.iter() {
        let initiator = &dpu.reprovision_requested.as_ref().unwrap().initiator;
        assert!(initiator.starts_with(AutomaticFirmwareUpdateReference::REF_NAME));
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_get_updates_in_progress(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock("test-dpu-firmware-admission".into())
        .await?;
    let managed_host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    managed_host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };

    let snapshots = get_all_snapshots(&env).await;

    let mut txn = env
        .pool
        .begin()
        .await
        .expect("Failed to create transaction");

    let updating_count = dpu_nic_firmware_update
        .get_updates_in_progress(&mut txn)
        .await?;

    assert!(updating_count.is_empty());

    let started_count = dpu_nic_firmware_update
        .start_updates(&env.pool, &work_lock, 10, &HashSet::default(), &snapshots)
        .await?;

    let updating_count = dpu_nic_firmware_update
        .get_updates_in_progress(&mut txn)
        .await?;

    assert!(started_count.contains(&managed_host.id));
    assert_eq!(updating_count.len(), 1);
    assert!(updating_count.contains(&managed_host.id));

    Ok(())
}

#[crate::sqlx_test]
async fn test_check_for_updates(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let machines = vec![
        create_managed_host(&env).await,
        create_managed_host(&env).await,
    ];
    let mut txn = env.pool.begin().await?;
    for m in &machines {
        m.update_nic_firmware_version(&mut txn).await?;
    }
    txn.commit().await?;

    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };

    let snapshots = machines
        .snapshots(
            &mut env.db_reader(),
            LoadSnapshotOptions {
                include_history: false,
                include_instance_data: false,
                host_health_config: env.config.host_health,
            },
        )
        .await;

    let machine_updates = dpu_nic_firmware_update.check_for_updates(&snapshots).await;
    assert_eq!(machine_updates.len(), 2);

    Ok(())
}

/// A DPU that lands on the wrong version keeps its update marker, so the
/// manager re-sees it every pass -- but the outcome must count once, not once
/// per poll; a successful update clears the memory so a later wrong landing
/// is a new outcome.
#[crate::sqlx_test]
async fn test_wrong_version_failures_count_once_per_outcome(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_instrument::testing::MetricsCapture;

    let env = create_test_env(pool).await;
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock("test-dpu-firmware-admission".into())
        .await?;
    let mh = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    mh.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;

    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };
    let snapshots = get_all_snapshots(&env).await;
    dpu_nic_firmware_update
        .start_updates(&env.pool, &work_lock, 10, &HashSet::default(), &snapshots)
        .await?;

    let land_on_version = |version: &'static str, env: &TestEnv| {
        let pool = env.pool.clone();
        let dpu_id = mh.dpu().id.to_string();
        let host_id = mh.id;
        async move {
            let mut txn = pool.begin().await.unwrap();
            let query = r#"UPDATE machine_topologies SET topology=jsonb_set(topology, '{discovery_data,Info,dpu_info,firmware_version}', $1, false)
     WHERE machine_id=$2"#;
            sqlx::query::<_>(query)
                .bind(sqlx::types::Json(version))
                .bind(&dpu_id)
                .execute(&mut *txn)
                .await
                .unwrap();
            let query = r#"UPDATE machines set reprovisioning_requested = NULL where id = $1"#;
            sqlx::query(query)
                .bind(&dpu_id)
                .execute(&mut *txn)
                .await
                .unwrap();
            // The in-update marker the manager looks for when deciding a
            // reprovision "finished" (mirrors the pretend-completed steps in
            // test_clear_completed_updates above).
            let health_override = create_host_update_health_report_dpufw();
            db::machine::insert_health_report(
                &mut txn,
                &host_id,
                health_report::HealthReportApplyMode::Merge,
                &health_override,
                false,
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();
        }
    };

    let metrics = MetricsCapture::for_registry(env.test_meter.registry());
    let wrong_version_delta = || {
        metrics.counter_delta(
            "carbide_firmware_update_failures_total",
            &[
                ("target", "dpu_nic"),
                ("cause", "wrong_version_after_update"),
            ],
        )
    };

    // The update "finishes" on a version outside the configured set.
    land_on_version("0.0.0-wrong", &env).await;
    let mut txn = env.pool.begin().await?;
    dpu_nic_firmware_update
        .clear_completed_updates(&mut txn)
        .await?;
    dpu_nic_firmware_update
        .clear_completed_updates(&mut txn)
        .await?;
    txn.commit().await?;
    assert_eq!(wrong_version_delta(), 1.0, "one outcome, not one per pass");

    // A later pass sees the desired version: markers and the reported memory
    // both clear.
    land_on_version("24.42.1000", &env).await;
    let mut txn = env.pool.begin().await?;
    dpu_nic_firmware_update
        .clear_completed_updates(&mut txn)
        .await?;
    txn.commit().await?;
    assert_eq!(wrong_version_delta(), 1.0, "a success is not a failure");

    // A fresh update cycle lands wrong again: that is a new outcome.
    dpu_nic_firmware_update
        .start_updates(
            &env.pool,
            &work_lock,
            10,
            &HashSet::default(),
            &get_all_snapshots(&env).await,
        )
        .await?;
    land_on_version("0.0.1-wrong", &env).await;
    let mut txn = env.pool.begin().await?;
    dpu_nic_firmware_update
        .clear_completed_updates(&mut txn)
        .await?;
    txn.commit().await?;
    assert_eq!(wrong_version_delta(), 2.0, "a new wrong landing re-reports");

    Ok(())
}

/// DPF picks update targets by the DPUDeployment's expected BFB, so a
/// DPF-managed DPU's reported NIC firmware has no reason to appear in
/// `dpu_nic_firmware_update_versions`. The completion path must not hold it
/// against that list, or the HostUpdateInProgress alert stays pinned after the
/// reprovision finishes and the host never becomes allocatable again.
#[crate::sqlx_test]
async fn test_clear_completed_updates_dpf_host_off_list_version(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    // Mark the host as DPF-ingested and pretend a DPF-driven reprovision just
    // finished: the DPU is back with a firmware version outside the configured
    // set, its reprovisioning flag is cleared, and the in-update marker is still
    // on the host.
    let mut txn = env.pool.begin().await?;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &mh.id).await?;
    update_nic_firmware_version(&mut txn, &mh.dpu().id, "0.0.0-from-bfb").await?;
    let query = r#"UPDATE machines set reprovisioning_requested = NULL where id = $1"#;
    sqlx::query(query)
        .bind(mh.dpu().id.to_string())
        .execute(&mut *txn)
        .await?;
    db::machine::insert_health_report(
        &mut txn,
        &mh.id,
        health_report::HealthReportApplyMode::Merge,
        &create_host_update_health_report_dpufw(),
        false,
    )
    .await?;
    txn.commit().await?;

    assert!(
        !env.config
            .dpu_config
            .dpu_nic_firmware_update_versions
            .contains(&"0.0.0-from-bfb".to_string()),
        "the version the BFB landed must be off the NIC firmware allowlist",
    );

    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };

    let metrics = MetricsCapture::for_registry(env.test_meter.registry());
    let mut txn = env.pool.begin().await?;
    dpu_nic_firmware_update
        .clear_completed_updates(&mut txn)
        .await?;

    let managed_host = mh.snapshot(&mut txn).await;
    assert!(
        !managed_host
            .host_snapshot
            .health_reports
            .merges
            .contains_key(HOST_UPDATE_HEALTH_REPORT_SOURCE),
        "DPF host must have its update marker removed once the reprovision completes",
    );
    assert!(managed_host.aggregate_health.alerts.is_empty());
    assert_eq!(
        metrics.counter_delta(
            "carbide_firmware_update_failures_total",
            &[
                ("target", "dpu_nic"),
                ("cause", "wrong_version_after_update"),
            ],
        ),
        0.0,
        "a DPF update landing off the NIC allowlist is not a wrong-version failure",
    );

    Ok(())
}

impl TestManagedHost {
    pub(in crate::tests) async fn update_nic_firmware_version(
        &self,
        txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> CarbideResult<()> {
        update_nic_firmware_version(txn, &self.dpu().id, "11.10.1000").await
    }
}

// Replacing the manager owner must reject the actual DPU write, even when the
// earlier snapshot still describes a valid automatic-update candidate.
#[crate::sqlx_test]
async fn stale_work_lock_cannot_admit_dpu_firmware(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let updater = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };
    let snapshots = get_all_snapshots(&env).await;
    assert_eq!(updater.check_for_updates(&snapshots).await.len(), 1);
    let work_key = "stale-dpu-firmware-admission";
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock(work_key.into())
        .await?;
    let changed =
        sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
            .bind(work_key)
            .execute(&env.pool)
            .await?;
    assert_eq!(changed.rows_affected(), 1);

    let error = updater
        .start_updates(&env.pool, &work_lock, 1, &HashSet::new(), &snapshots)
        .await
        .expect_err("the previous worker must not admit a DPU update");
    assert!(matches!(error, crate::CarbideError::FailedPrecondition(_)));
    let mut txn = env.pool.begin().await?;
    let retained = host.snapshot(&mut txn).await;
    assert!(
        retained
            .dpu_snapshots
            .iter()
            .all(|dpu| dpu.reprovision_requested.is_none())
    );
    txn.commit().await?;
    Ok(())
}

// A failed accounting read cannot make every occupied DPU slot appear free.
#[crate::sqlx_test]
async fn outstanding_dpu_read_failure_is_returned(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = Figment::new()
        .merge(Toml::file(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/cfg/test_data/full_config.toml"
        )))
        .extract()?;
    let test_meter = carbide_utils::test_support::test_meter::TestMeter::default();
    let updater = DpuNicFirmwareUpdate {
        instrumentation: carbide_instrument::Instrumentation::new(test_meter.meter()),
        reported_wrong_versions: std::sync::Mutex::new(HashSet::new()),
        metrics: None,
        config: std::sync::Arc::new(config),
        dpf: None,
    };
    let mut txn = pool.begin().await?;
    sqlx::query("SELECT 1 / 0")
        .execute(txn.as_mut())
        .await
        .expect_err("abort only the accounting transaction");
    let error = updater
        .get_updates_in_progress(txn.as_mut())
        .await
        .expect_err("an aborted read must not advertise zero occupied slots");
    assert!(matches!(error, crate::CarbideError::DBError(_)));
    txn.rollback().await?;
    Ok(())
}

// The admission transaction must retain its fence while waiting for a DPU row.
// A new worker can take ownership only after the original request commits.
#[crate::sqlx_test]
async fn dpu_admission_keeps_work_lock_ownership_until_commit(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let updater = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };
    let work_key = "fenced-dpu-admission";
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock(work_key.into())
        .await?;
    let snapshots = get_all_snapshots(&env).await;
    let updating_hosts = HashSet::new();

    // Pause the real writer after its fence, before it can save the request.
    let mut blocker = env.pool.begin().await?;
    db::machine::lock_by_id_for_update(blocker.as_mut(), &host.dpu().id.into()).await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    let admission = updater.start_updates(&env.pool, &work_lock, 1, &updating_hosts, &snapshots);
    tokio::pin!(admission);
    let admission_pid = tokio::select! {
        result = &mut admission => panic!("admission did not wait for the DPU row: {result:?}"),
        pid = wait_for_blocked_query(&env.pool, blocker_pid, "SELECT reprovisioning_requested IS NOT NULL") => pid,
    };

    let takeover =
        sqlx::query("UPDATE work_locks SET worker_id = gen_random_uuid() WHERE work_key = $1")
            .bind(work_key)
            .execute(&env.pool);
    tokio::pin!(takeover);
    tokio::select! {
        result = &mut admission => panic!("admission escaped the held DPU row: {result:?}"),
        result = &mut takeover => panic!("ownership changed before admission committed: {result:?}"),
        _ = wait_for_blocked_query(&env.pool, admission_pid, "UPDATE work_locks SET worker_id") => {},
    }

    // The database has observed both waits, so release the DPU row without
    // relying on a guessed delay or expiring the real work lease.
    blocker.commit().await?;
    let (admitted, taken_over) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(admission, takeover)
    })
    .await
    .expect("admission and takeover must finish after the DPU row is released");
    assert_eq!(admitted?, HashSet::from([host.id.into()]));
    assert_eq!(taken_over?.rows_affected(), 1);
    let mut txn = env.pool.begin().await?;
    let dpu = host.dpu().db_machine(&mut txn).await;
    assert!(
        dpu.reprovision_requested
            .expect("the request must commit before ownership changes")
            .update_firmware
    );
    txn.commit().await?;
    Ok(())
}

// A contended admission with no operator timeout must release its fence under
// the statement cap, so another replica can process unrelated work-lock commands.
#[crate::sqlx_test]
async fn contended_dpu_admission_releases_another_work_manager(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let updater = DpuNicFirmwareUpdate {
        instrumentation: env.api.instrumentation.clone(),
        reported_wrong_versions: std::sync::Mutex::new(HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };
    let writer_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .after_connect(|connection, _| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = '0'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(env.pool.connect_options().as_ref().clone())
        .await?;
    let mut tasks = tokio::task::JoinSet::new();
    let contender = db::work_lock_manager::start(
        &mut tasks,
        env.pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await?;
    let work_key = "contended-dpu-admission";
    let work_lock = env
        .api
        .work_lock_manager_handle
        .try_acquire_lock(work_key.into())
        .await?;
    let snapshots = get_all_snapshots(&env).await;
    let updating = HashSet::new();
    let mut blocker = env.pool.begin().await?;
    db::machine::lock_by_id_for_update(blocker.as_mut(), &host.dpu().id.into()).await?;
    let blocker_pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    let (admission, acquire_same, unrelated) = {
        let admission = updater.start_updates(&writer_pool, &work_lock, 1, &updating, &snapshots);
        tokio::pin!(admission);
        let admission_pid = tokio::select! {
            result = &mut admission => panic!("admission did not reach the held DPU row: {result:?}"),
            pid = wait_for_blocked_query(&env.pool, blocker_pid, "SELECT reprovisioning_requested IS NOT NULL") => pid,
        };

        // Real acquisition waits in the second manager's FIFO, even though the
        // lease has not expired and this attempt will eventually be rejected.
        let acquire_same = contender.try_acquire_lock(work_key.into());
        tokio::pin!(acquire_same);
        tokio::select! {
            result = &mut admission => panic!("admission expired before the competing acquire was observed: {result:?}"),
            result = &mut acquire_same => panic!("competing acquire escaped the admission fence: {}", result.is_ok()),
            _ = wait_for_blocked_query(&env.pool, admission_pid, "INSERT INTO work_locks") => {},
        }
        let unrelated = contender.try_acquire_lock("unrelated-dpu-admission-work".into());
        tokio::time::timeout(Duration::from_secs(8), async {
            tokio::join!(admission, acquire_same, unrelated)
        })
        .await
        .expect(
            "the statement cap must unblock both work-lock commands while the DPU row stays locked",
        )
    };
    assert!(matches!(admission, Err(crate::CarbideError::DBError(_))));
    assert!(matches!(
        acquire_same,
        Err(db::work_lock_manager::AcquireLockError::WorkAlreadyLocked(
            _
        ))
    ));
    unrelated?.release().await?;
    let limit: String = sqlx::query_scalar("SHOW statement_timeout")
        .fetch_one(&writer_pool)
        .await?;
    assert_eq!(limit, "0");
    blocker.commit().await?;
    let mut txn = env.pool.begin().await?;
    assert!(
        host.dpu()
            .db_machine(&mut txn)
            .await
            .reprovision_requested
            .is_none()
    );
    txn.commit().await?;
    drop(work_lock);
    drop(contender);
    writer_pool.close().await;
    tasks.shutdown().await;
    Ok(())
}
