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

use carbide_uuid::rack::RackId;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthReport, HealthReportApplyMode,
};
use model::expected_machine::ExpectedMachineData;
use model::machine::LoadSnapshotOptions;
use model::rack::RackConfig;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{self as rpc_forge};
use tonic::Request;

use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::test_support::health::{HealthCrud, HealthStatusView, check_health_aggregation};
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_managed_host_with_config,
    create_test_env_with_overrides, get_config, send_health_report_entry,
};

fn leak_alert_report(source: &str) -> HealthReport {
    HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: "BmsLeakDetectRack".parse().unwrap(),
            target: None,
            in_alert_since: Some(chrono::Utc::now()),
            message: "Leak detected".to_string(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::sensor_critical(),
                HealthAlertClassification::hardware(),
            ],
        }],
    }
}

fn empty_healthy_report(source: &str) -> HealthReport {
    HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![],
    }
}

async fn test_env(pool: sqlx::PgPool) -> TestEnv {
    create_test_env_with_overrides(pool, TestEnvOverrides::with_config(get_config())).await
}

/// Persists a rack and returns its ID.
async fn new_rack(pool: &sqlx::PgPool) -> Result<RackId, Box<dyn std::error::Error>> {
    let mut txn = pool.acquire().await?;
    let rack_id = TestRackDbBuilder::new().persist(&mut txn).await?;
    Ok(rack_id)
}

/// Builds the rack health-override CRUD surface over `env` for `id`. The shared
/// checks in [`crate::test_support::health`] drive these closures.
// The four `impl AsyncFn` members are intentionally distinct unnameable closure
// types; there is nothing to factor into a `type` alias.
#[allow(clippy::type_complexity)]
fn rack_crud(
    env: &TestEnv,
    id: RackId,
) -> HealthCrud<
    RackId,
    impl AsyncFn(RackId, HealthReport, rpc_forge::HealthReportApplyMode) -> Result<(), tonic::Status>,
    impl AsyncFn(RackId) -> Result<Vec<rpc_forge::HealthReportEntry>, tonic::Status>,
    impl AsyncFn(RackId, String) -> Result<(), tonic::Status>,
    impl AsyncFn(RackId) -> Result<HealthStatusView, tonic::Status>,
> {
    HealthCrud {
        real_id: id,
        nonexistent_id: RackId::new(uuid::Uuid::new_v4().to_string()),
        alert: leak_alert_report("dsx-exchange-consumer"),
        alert_source: "dsx-exchange-consumer",
        insert: async move |id, report: HealthReport, mode| {
            let report: rpc::health::HealthReport = report.into();
            env.api
                .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
                    rack_id: Some(id),
                    health_report_entry: Some(rpc_forge::HealthReportEntry {
                        report: Some(report),
                        mode: mode as i32,
                    }),
                }))
                .await
                .map(|_| ())
        },
        list: async move |id| {
            Ok(env
                .api
                .list_rack_health_reports(Request::new(rpc_forge::ListRackHealthReportsRequest {
                    rack_id: Some(id),
                }))
                .await?
                .into_inner()
                .health_report_entries)
        },
        remove: async move |id, source| {
            env.api
                .remove_rack_health_report(Request::new(rpc_forge::RemoveRackHealthReportRequest {
                    rack_id: Some(id),
                    source,
                }))
                .await
                .map(|_| ())
        },
        find: async move |id| {
            let resp = env
                .api
                .find_racks_by_ids(Request::new(rpc_forge::RacksByIdsRequest {
                    rack_ids: vec![id],
                }))
                .await?
                .into_inner();
            assert_eq!(resp.racks.len(), 1);
            let status = resp.racks[0].status.clone().unwrap();
            Ok(HealthStatusView {
                health: status.health,
                health_sources: status.health_sources,
            })
        },
    }
}

#[crate::sqlx_test]
async fn test_insert_list_remove_rack_override(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    rack_crud(&env, rack_id).check_insert_list_remove().await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_idempotent_insert(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    rack_crud(&env, rack_id).check_idempotent_insert().await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_retains_in_alert_since(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    rack_crud(&env, rack_id)
        .check_retains_in_alert_since()
        .await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_remove_nonexistent_source(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    rack_crud(&env, rack_id)
        .check_remove_nonexistent_source()
        .await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_missing_rack_id(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    rack_crud(&env, rack_id).check_missing_entity().await;
    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_health_visible_in_find_racks_by_ids(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    rack_crud(&env, rack_id).check_visible_in_find().await;
    Ok(())
}

async fn insert_rack_health(env: &TestEnv, id: RackId, report: HealthReport) {
    let report: rpc::health::HealthReport = report.into();
    env.api
        .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
            rack_id: Some(id),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(report),
                mode: rpc_forge::HealthReportApplyMode::Replace as i32,
            }),
        }))
        .await
        .unwrap();
}

async fn rack_health_history(
    env: &TestEnv,
    id: RackId,
    start_time: Option<chrono::DateTime<chrono::Utc>>,
    end_time: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<rpc_forge::HealthHistoryRecord> {
    env.api
        .find_rack_health_histories(Request::new(rpc_forge::RackHealthHistoriesRequest {
            rack_ids: vec![id.clone()],
            start_time: start_time.map(Into::into),
            end_time: end_time.map(Into::into),
        }))
        .await
        .unwrap()
        .into_inner()
        .histories
        .remove(&id.to_string())
        .unwrap_or_default()
        .records
}

/// The rack controller records a health-history snapshot each iteration; the
/// DB layer deduplicates unchanged observations and only appends when the
/// aggregate health content changes. This exercises that path end-to-end
/// through the `FindRackHealthHistories` RPC.
#[crate::sqlx_test]
async fn test_rack_health_history_records_and_deduplicates(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let id = new_rack(&pool).await?;

    insert_rack_health(&env, id.clone(), leak_alert_report("dsx-exchange-consumer")).await;
    env.run_rack_controller_iteration().await;
    let after_first = rack_health_history(&env, id.clone(), None, None)
        .await
        .len();
    assert!(
        after_first >= 1,
        "expected at least one health-history record after the first iteration"
    );

    env.run_rack_controller_iteration().await;
    let after_second = rack_health_history(&env, id.clone(), None, None)
        .await
        .len();
    assert_eq!(
        after_first, after_second,
        "unchanged aggregate health must not add a new record"
    );

    insert_rack_health(
        &env,
        id.clone(),
        empty_healthy_report("dsx-exchange-consumer"),
    )
    .await;
    env.run_rack_controller_iteration().await;
    let after_change = rack_health_history(&env, id.clone(), None, None)
        .await
        .len();
    assert_eq!(
        after_change,
        after_second + 1,
        "changed aggregate health must append exactly one record"
    );

    Ok(())
}

/// The `FindRackHealthHistories` handler must forward both time bounds from the
/// request into the shared query. This proves the rack-specific wiring; the
/// shared range-filtering query in `find_by_object_ids` is also exercised by
/// the machine time-range test.
#[crate::sqlx_test]
async fn test_rack_health_history_respects_time_range(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let id = new_rack(&pool).await?;

    let before = chrono::Utc::now();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    insert_rack_health(&env, id.clone(), leak_alert_report("dsx-exchange-consumer")).await;
    env.run_rack_controller_iteration().await;

    let in_range =
        rack_health_history(&env, id.clone(), Some(before), Some(chrono::Utc::now())).await;
    assert!(
        !in_range.is_empty(),
        "a range bracketing the write must return the record"
    );

    let ended_before = rack_health_history(&env, id.clone(), None, Some(before)).await;
    assert!(
        ended_before.is_empty(),
        "records written after end_time must be excluded"
    );

    let starts_future = rack_health_history(
        &env,
        id.clone(),
        Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        None,
    )
    .await;
    assert!(
        starts_future.is_empty(),
        "records written before start_time must be excluded"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_rack_health_histories_rejects_empty_ids(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool).await;
    let status = env
        .api
        .find_rack_health_histories(Request::new(rpc_forge::RackHealthHistoriesRequest {
            rack_ids: vec![],
            start_time: None,
            end_time: None,
        }))
        .await
        .expect_err("empty rack_ids must be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    Ok(())
}

#[crate::sqlx_test]
async fn test_rack_health_aggregation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = test_env(pool.clone()).await;
    let rack_id = new_rack(&pool).await?;
    check_health_aggregation(
        "racks",
        rack_id,
        leak_alert_report("dsx-exchange-consumer"),
        empty_healthy_report("admin-override"),
        &env.test_meter,
        async |id, report: HealthReport, mode| {
            let report: rpc::health::HealthReport = report.into();
            env.api
                .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
                    rack_id: Some(id),
                    health_report_entry: Some(rpc_forge::HealthReportEntry {
                        report: Some(report),
                        mode: mode as i32,
                    }),
                }))
                .await
                .map(|_| ())
        },
        async || env.run_rack_controller_iteration().await,
    )
    .await;
    Ok(())
}

// ---- Rack-specific behaviors: propagation to host aggregate health and the
// host-vs-rack override precedence rules. These have no power-shelf or switch
// counterpart, so they stay here rather than in the shared CRUD helper.

#[crate::sqlx_test]
async fn test_propagation_to_host_aggregate_health(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(get_config()))
            .await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.acquire().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    drop(txn);

    let host_config =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mh = create_managed_host_with_config(&env, host_config).await;
    let host_machine_id = mh.id;

    let report = leak_alert_report("dsx-exchange-consumer");
    env.api
        .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(report.into()),
                mode: rpc_forge::HealthReportApplyMode::Merge as i32,
            }),
        }))
        .await?;

    let snapshot = db::managed_host::load_snapshot(
        &mut env.db_reader(),
        &host_machine_id,
        LoadSnapshotOptions::default(),
    )
    .await?
    .unwrap();

    assert!(
        snapshot.rack_health_overrides.is_some(),
        "rack_health_overrides should be populated"
    );

    let has_leak_alert = snapshot.aggregate_health.alerts.iter().any(|a| {
        a.id.as_str() == "BmsLeakDetectRack"
            && a.classifications
                .contains(&HealthAlertClassification::prevent_allocations())
    });
    assert!(
        has_leak_alert,
        "Host aggregate health should contain rack leak alert with PreventAllocations"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_host_allocatability_blocked_by_rack_override(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(get_config()))
            .await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.acquire().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    drop(txn);

    let host_config =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: Some(rack_id.clone()),
            ..Default::default()
        });
    let mh = create_managed_host_with_config(&env, host_config).await;
    let host_machine_id = mh.id;

    let report = leak_alert_report("dsx-exchange-consumer");
    env.api
        .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(report.into()),
                mode: rpc_forge::HealthReportApplyMode::Merge as i32,
            }),
        }))
        .await?;

    let snapshot = db::managed_host::load_snapshot(
        &mut env.db_reader(),
        &host_machine_id,
        LoadSnapshotOptions::default(),
    )
    .await?
    .unwrap();

    let result = snapshot.is_usable_as_instance(false);
    assert!(
        result.is_err(),
        "Host should NOT be allocatable when rack has PreventAllocations override"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_host_replace_overrides_rack_alerts(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(get_config()))
            .await;

    let mh = create_managed_host(&env).await;
    let host_machine_id = mh.id;

    let host_replace = empty_healthy_report("sre-override");
    send_health_report_entry(
        &env,
        &host_machine_id,
        (host_replace, HealthReportApplyMode::Replace),
    )
    .await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.acquire().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let config = RackConfig::default();
    db::rack::update(&mut txn, &rack_id, &config).await?;
    drop(txn);

    let rack_report = leak_alert_report("dsx-exchange-consumer");
    env.api
        .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(rack_report.into()),
                mode: rpc_forge::HealthReportApplyMode::Merge as i32,
            }),
        }))
        .await?;

    let snapshot = db::managed_host::load_snapshot(
        &mut env.db_reader(),
        &host_machine_id,
        LoadSnapshotOptions::default(),
    )
    .await?
    .unwrap();

    let has_leak_alert = snapshot.aggregate_health.alerts.iter().any(|a| {
        a.id.as_str() == "BmsLeakDetectRack"
            && a.classifications
                .contains(&HealthAlertClassification::prevent_allocations())
    });
    assert!(
        !has_leak_alert,
        "Rack alerts should not appear when host has Replace override"
    );

    let result = snapshot.is_usable_as_instance(false);
    assert!(
        result.is_ok(),
        "Host with Replace override should not be blocked by rack alerts"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_host_replace_takes_full_precedence_over_rack_replace(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(get_config()))
            .await;

    let mh = create_managed_host(&env).await;
    let host_machine_id = mh.id;

    let host_replace = empty_healthy_report("sre-override");
    send_health_report_entry(
        &env,
        &host_machine_id,
        (host_replace, HealthReportApplyMode::Replace),
    )
    .await;

    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let mut txn = pool.acquire().await?;
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await?;
    let config = RackConfig::default();
    db::rack::update(&mut txn, &rack_id, &config).await?;
    drop(txn);

    let rack_report = leak_alert_report("rack-level-replace");
    env.api
        .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(rack_report.into()),
                mode: rpc_forge::HealthReportApplyMode::Replace as i32,
            }),
        }))
        .await?;

    let snapshot = db::managed_host::load_snapshot(
        &mut env.db_reader(),
        &host_machine_id,
        LoadSnapshotOptions::default(),
    )
    .await?
    .unwrap();

    assert!(
        snapshot.host_snapshot.health_reports.replace.is_some(),
        "Host-level Replace override should still be present"
    );

    let has_leak_alert = snapshot
        .aggregate_health
        .alerts
        .iter()
        .any(|a| a.id.as_str() == "BmsLeakDetectRack");
    assert!(
        !has_leak_alert,
        "Rack overrides should be skipped when host has Replace override"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_dsx_consumer_contract(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env =
        create_test_env_with_overrides(pool.clone(), TestEnvOverrides::with_config(get_config()))
            .await;

    let mut txn = pool.acquire().await?;
    let rack_id = TestRackDbBuilder::new().persist(&mut txn).await?;
    drop(txn);

    let report = HealthReport {
        source: "dsx-exchange-consumer".to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: "BmsLeakDetectRack".parse().unwrap(),
            target: Some(rack_id.to_string()),
            in_alert_since: Some(chrono::Utc::now()),
            message: format!("Leak detected on rack {}", rack_id),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::sensor_critical(),
                HealthAlertClassification::hardware(),
            ],
        }],
    };

    env.api
        .insert_rack_health_report(Request::new(rpc_forge::InsertRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(report.into()),
                mode: rpc_forge::HealthReportApplyMode::Merge as i32,
            }),
        }))
        .await?;

    env.api
        .remove_rack_health_report(Request::new(rpc_forge::RemoveRackHealthReportRequest {
            rack_id: Some(rack_id.clone()),
            source: "dsx-exchange-consumer".to_string(),
        }))
        .await?;

    let list_resp = env
        .api
        .list_rack_health_reports(Request::new(rpc_forge::ListRackHealthReportsRequest {
            rack_id: Some(rack_id.clone()),
        }))
        .await?
        .into_inner();
    assert_eq!(
        list_resp.health_report_entries.len(),
        0,
        "All overrides should be removed after DSX consumer clear"
    );

    Ok(())
}
