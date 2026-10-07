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

use carbide_instrument::testing::MetricsCapture;
use carbide_machine_controller::health_report::create_host_update_health_report_dpufw;
use common::api_fixtures::{
    TestEnv, create_managed_host, create_managed_host_multi_dpu, create_test_env,
};
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
use crate::tests::dpu_machine_update::{get_all_snapshots, update_nic_firmware_version};

#[crate::sqlx_test]
async fn test_start_updates(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let managed_host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    managed_host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
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

    let metrics = MetricsCapture::start();

    let started_count = dpu_nic_firmware_update
        .start_updates(&env.pool, 10, &HashSet::default(), &snapshots)
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

#[crate::sqlx_test]
async fn test_start_updates_with_multidpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

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
        .start_updates(&env.pool, 10, &HashSet::default(), &snapshots)
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
    let managed_host = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    managed_host.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;
    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
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
        .start_updates(&env.pool, 10, &HashSet::default(), &snapshots)
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

    let machine_updates = dpu_nic_firmware_update
        .check_for_updates(&snapshots, 10)
        .await;
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
    let mh = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await?;
    mh.update_nic_firmware_version(&mut txn).await?;
    txn.commit().await?;

    let dpu_nic_firmware_update = DpuNicFirmwareUpdate {
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };
    let snapshots = get_all_snapshots(&env).await;
    dpu_nic_firmware_update
        .start_updates(&env.pool, 10, &HashSet::default(), &snapshots)
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

    let metrics = MetricsCapture::start();
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
        reported_wrong_versions: std::sync::Mutex::new(std::collections::HashSet::new()),
        metrics: None,
        config: env.config.clone(),
        dpf: None,
    };

    let metrics = MetricsCapture::start();
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
