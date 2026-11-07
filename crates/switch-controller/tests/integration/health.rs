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
use carbide_test_harness::test_support::health::check_health_aggregation;
use carbide_uuid::switch::SwitchId;
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use rpc::forge::{self as rpc_forge};
use tonic::Request;

use crate::common::{ControllerEnv, new_switch};

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

async fn insert_switch_health(env: &ControllerEnv, id: SwitchId, report: HealthReport) {
    let report: rpc::health::HealthReport = report.into();
    env.api
        .insert_switch_health_report(Request::new(rpc_forge::InsertSwitchHealthReportRequest {
            switch_id: Some(id),
            health_report_entry: Some(rpc_forge::HealthReportEntry {
                report: Some(report),
                mode: rpc_forge::HealthReportApplyMode::Replace as i32,
            }),
        }))
        .await
        .unwrap();
}

async fn switch_health_history(
    env: &ControllerEnv,
    id: SwitchId,
) -> Vec<rpc_forge::HealthHistoryRecord> {
    env.api
        .find_switch_health_histories(Request::new(rpc_forge::SwitchHealthHistoriesRequest {
            switch_ids: vec![id],
            start_time: None,
            end_time: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .histories
        .remove(&id.to_string())
        .unwrap_or_default()
        .records
}

#[sqlx_test]
async fn switch_health_history_records_and_deduplicates(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool).await;
    let id = new_switch(&env, None, None).await?;

    insert_switch_health(&env, id, alert_report("external-monitor")).await;
    env.run_switch_controller_iteration().await;
    let after_first = switch_health_history(&env, id).await.len();
    assert!(
        after_first >= 1,
        "expected at least one health-history record after the first iteration"
    );

    env.run_switch_controller_iteration().await;
    let after_second = switch_health_history(&env, id).await.len();
    assert_eq!(
        after_first, after_second,
        "unchanged aggregate health must not add a new record"
    );

    insert_switch_health(&env, id, empty_healthy_report("external-monitor")).await;
    env.run_switch_controller_iteration().await;
    let after_change = switch_health_history(&env, id).await.len();
    assert_eq!(
        after_change,
        after_second + 1,
        "changed aggregate health must append exactly one record"
    );

    Ok(())
}

#[sqlx_test]
async fn switch_health_aggregation(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = ControllerEnv::new(pool).await;
    let id = new_switch(&env, None, None).await?;
    check_health_aggregation(
        "switches",
        id,
        alert_report("external-monitor"),
        empty_healthy_report("admin-override"),
        &env.harness.test_meter,
        async |id, report: HealthReport, mode| {
            let report: rpc::health::HealthReport = report.into();
            env.api
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
        async || env.run_switch_controller_iteration().await,
    )
    .await;
    Ok(())
}
