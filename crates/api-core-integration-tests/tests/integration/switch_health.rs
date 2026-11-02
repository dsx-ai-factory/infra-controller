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

use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::health::{HealthCrud, HealthStatusView};
use carbide_uuid::switch::SwitchId;
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use rpc::forge::forge_server::Forge;
use rpc::forge::{self as rpc_forge};
use tonic::Request;

fn alert_report(source: &str) -> HealthReport {
    HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: "SwitchUnhealthy".parse().unwrap(),
            target: None,
            in_alert_since: Some(chrono::Utc::now()),
            message: "Switch health issue detected".to_string(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
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

/// Builds the switch health-override CRUD surface over `env` for `id`. The shared
/// checks in [`carbide_test_harness::test_support::health`] drive these closures.
// The four `impl AsyncFn` members are intentionally distinct unnameable closure
// types; there is nothing to factor into a `type` alias.
#[allow(clippy::type_complexity)]
fn switch_crud(
    env: &TestHarness,
    id: SwitchId,
) -> HealthCrud<
    SwitchId,
    impl AsyncFn(SwitchId, HealthReport, rpc_forge::HealthReportApplyMode) -> Result<(), tonic::Status>,
    impl AsyncFn(SwitchId) -> Result<Vec<rpc_forge::HealthReportEntry>, tonic::Status>,
    impl AsyncFn(SwitchId, String) -> Result<(), tonic::Status>,
    impl AsyncFn(SwitchId) -> Result<HealthStatusView, tonic::Status>,
> {
    HealthCrud {
        real_id: id,
        nonexistent_id: SwitchId::from(uuid::Uuid::new_v4()),
        alert: alert_report("external-monitor"),
        alert_source: "external-monitor",
        insert: async move |id, report: HealthReport, mode| {
            let report: rpc::health::HealthReport = report.into();
            env.api()
                .insert_switch_health_report(Request::new(
                    rpc_forge::InsertSwitchHealthReportRequest {
                        switch_id: Some(id),
                        health_report_entry: Some(rpc_forge::HealthReportEntry {
                            report: Some(report),
                            mode: mode as i32,
                        }),
                    },
                ))
                .await
                .map(|_| ())
        },
        list: async move |id| {
            Ok(env
                .api()
                .list_switch_health_reports(Request::new(
                    rpc_forge::ListSwitchHealthReportsRequest {
                        switch_id: Some(id),
                    },
                ))
                .await?
                .into_inner()
                .health_report_entries)
        },
        remove: async move |id, source| {
            env.api()
                .remove_switch_health_report(Request::new(
                    rpc_forge::RemoveSwitchHealthReportRequest {
                        switch_id: Some(id),
                        source,
                    },
                ))
                .await
                .map(|_| ())
        },
        find: async move |id| {
            let resp = env
                .api()
                .find_switches(Request::new(rpc_forge::SwitchQuery {
                    switch_id: Some(id),
                    name: None,
                }))
                .await?
                .into_inner();
            assert_eq!(resp.switches.len(), 1);
            let status = resp.switches[0].status.clone().unwrap();
            Ok(HealthStatusView {
                health: status.health,
                health_sources: status.health_sources,
            })
        },
    }
}

#[sqlx_test]
async fn test_insert_list_remove_switch_override(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id).check_insert_list_remove().await;
    Ok(())
}

#[sqlx_test]
async fn test_idempotent_insert(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id).check_idempotent_insert().await;
    Ok(())
}

#[sqlx_test]
async fn test_retains_in_alert_since(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id).check_retains_in_alert_since().await;
    Ok(())
}

#[sqlx_test]
async fn test_remove_nonexistent_source(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id)
        .check_remove_nonexistent_source()
        .await;
    Ok(())
}

#[sqlx_test]
async fn test_missing_switch_id(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id).check_missing_entity().await;
    Ok(())
}

#[sqlx_test]
async fn test_replace_mode_override(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id)
        .check_replace_mode(empty_healthy_report("admin-override"), "admin-override")
        .await;
    Ok(())
}

#[sqlx_test]
async fn test_switch_health_visible_in_find_switches(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;
    switch_crud(&env, id).check_visible_in_find().await;
    Ok(())
}

async fn switch_health_history(
    env: &TestHarness,
    id: SwitchId,
    start_time: Option<chrono::DateTime<chrono::Utc>>,
    end_time: Option<chrono::DateTime<chrono::Utc>>,
) -> Vec<rpc_forge::HealthHistoryRecord> {
    env.api()
        .find_switch_health_histories(Request::new(rpc_forge::SwitchHealthHistoriesRequest {
            switch_ids: vec![id],
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

/// The `FindSwitchHealthHistories` handler must forward both time bounds from the
/// request into the shared query. This proves the switch-specific wiring (a
/// regression that passed `None` or swapped the fields would otherwise go
/// unnoticed); the shared range-filtering query in `find_by_object_ids` is also
/// exercised by the machine time-range test.
#[sqlx_test]
async fn test_switch_health_history_respects_time_range(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let id = env.create_switch(0, 0).await.id;

    let before = chrono::Utc::now();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mut txn = env.db_txn().await;
    db::health_history::persist(
        txn.as_mut(),
        db::health_history::HealthHistoryTableId::Switch,
        &id,
        &alert_report("external-monitor"),
    )
    .await?;
    txn.commit().await?;

    // A range that brackets the write returns the record.
    let in_range = switch_health_history(&env, id, Some(before), Some(chrono::Utc::now())).await;
    assert!(
        !in_range.is_empty(),
        "a range bracketing the write must return the record"
    );

    // An end_time before the write excludes it (proves end_time is forwarded).
    let ended_before = switch_health_history(&env, id, None, Some(before)).await;
    assert!(
        ended_before.is_empty(),
        "records written after end_time must be excluded"
    );

    // A start_time after the write excludes it (proves start_time is forwarded).
    let starts_future = switch_health_history(
        &env,
        id,
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

#[sqlx_test]
async fn test_find_switch_health_histories_rejects_empty_ids(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let status = env
        .api()
        .find_switch_health_histories(Request::new(rpc_forge::SwitchHealthHistoriesRequest {
            switch_ids: vec![],
            start_time: None,
            end_time: None,
        }))
        .await
        .expect_err("empty switch_ids must be rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    Ok(())
}
