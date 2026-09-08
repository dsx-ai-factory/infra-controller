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

use std::str::FromStr;

use db::machine::update_dpu_agent_health_report;
use db::{self};
use health_report::HealthReportApplyMode;
use model::machine::health_override::HARDWARE_HEALTH_OVERRIDE_PREFIX;
use model::machine::{HardwareHealthReportsConfig, HostHealthConfig, LoadSnapshotOptions};
use rpc::forge::HealthSourceOrigin;
use rpc::forge::forge_server::Forge;
use tonic::Request;

use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_test_env_with_overrides, get_config,
    network_configured_with_health, remove_health_report_entry, send_health_report_entry,
    simulate_hardware_health_report,
};

/// Tests whether DPU agent health reports are stored as a standard health source.
#[crate::sqlx_test]
async fn test_update_dpu_agent_health_report(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    // Start with a clean slate
    let mut txn = env.pool.begin().await?;
    db::machine::remove_health_report(
        &mut txn,
        &dpu_machine_id,
        HealthReportApplyMode::Merge,
        health_report::HealthReport::DPU_AGENT_SOURCE,
    )
    .await?;
    txn.commit().await?;

    let mut health = hr("dpu-agent", vec![], vec![("Failure1", None, "Failure1")]);
    health.observed_at = Some("2025-03-19T18:22:01+00:00".parse()?);
    let mut txn = env.pool.begin().await?;
    update_dpu_agent_health_report(&mut txn, &dpu_machine_id, &health).await?;
    txn.commit().await?;

    check_reports_equal(
        "forge-dpu-agent",
        load_snapshot(&env, &host_machine_id).await?.dpu_snapshots[0]
            .dpu_agent_health_report()
            .cloned()
            .unwrap(),
        health,
    );

    let mut newer_health = hr("dpu-agent", vec![], vec![("Failure2", None, "Failure2")]);
    newer_health.observed_at = Some("2025-03-19T18:22:03+00:00".parse()?);
    let mut txn = env.pool.begin().await?;
    update_dpu_agent_health_report(&mut txn, &dpu_machine_id, &newer_health).await?;
    txn.commit().await?;

    let mut older_health = hr("dpu-agent", vec![], vec![("Failure3", None, "Failure3")]);
    older_health.observed_at = Some("2025-03-19T18:22:02+00:00".parse()?);
    let mut txn = env.pool.begin().await?;
    update_dpu_agent_health_report(&mut txn, &dpu_machine_id, &older_health).await?;
    txn.commit().await?;

    check_reports_equal(
        "forge-dpu-agent",
        load_snapshot(&env, &host_machine_id).await?.dpu_snapshots[0]
            .dpu_agent_health_report()
            .cloned()
            .unwrap(),
        older_health,
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_health_reporting(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    // As part of test fixtures creating the managed host, we send an empty hardware health
    // report and an empty dpu agent health report.
    check_reports_equal(
        "forge-dpu-agent",
        load_snapshot(&env, &host_machine_id).await?.dpu_snapshots[0]
            .dpu_agent_health_report()
            .cloned()
            .unwrap(),
        health_report::HealthReport::empty("".to_string()),
    );
    check_reports_equal(
        &format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"),
        load_snapshot(&env, &host_machine_id)
            .await?
            .host_snapshot
            .health_reports
            .merges
            .values()
            .next()
            .unwrap()
            .clone(),
        health_report::HealthReport::empty("".to_string()),
    );

    let m = find_machine(&env, &host_machine_id).await;
    assert_eq!(
        m.status.as_ref().unwrap().health_sources,
        vec![HealthSourceOrigin {
            mode: HealthReportApplyMode::Merge as i32,
            source: format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health")
        }]
    );
    let aggregate_health = aggregate(m).unwrap();
    assert_eq!(aggregate_health.source, "aggregate-host-health");
    check_time(&aggregate_health);
    assert_eq!(aggregate_health.alerts, vec![]);
    assert_eq!(aggregate_health.successes, vec![]);

    // Let forge-dpu-agent submit a report which claims the DPU is no longer healthy

    let dpu_health = hr(
        "should-get-updated",
        vec![("Success1", None)],
        vec![("Failure1", None, "Failure1")],
    );

    network_configured_with_health(&env, &dpu_machine_id, Some(dpu_health.clone().into())).await;

    check_reports_equal(
        "forge-dpu-agent",
        load_snapshot(&env, &host_machine_id).await?.dpu_snapshots[0]
            .dpu_agent_health_report()
            .cloned()
            .unwrap(),
        dpu_health.clone(),
    );

    // Use the FindMachinesByIds API to verify Health of Host and DPU
    let current_dpu_health = load_health_via_find_machines_by_ids(&env, &dpu_machine_id)
        .await
        .unwrap();
    check_time(&current_dpu_health);
    check_reports_equal("forge-dpu-agent", current_dpu_health, dpu_health.clone());
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    check_time(&aggregate_health);
    check_reports_equal(
        "aggregate-host-health",
        aggregate_health,
        dpu_health.clone(),
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_hardware_health_reporting(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, _) = create_managed_host(&env).await.into();

    // Hardware health should start empty.
    check_reports_equal(
        &format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"),
        load_snapshot(&env, &host_machine_id)
            .await?
            .host_snapshot
            .health_reports
            .merges
            .values()
            .next()
            .unwrap()
            .clone(),
        health_report::HealthReport::empty("".to_string()),
    );

    let report_name: String = format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health");
    let report = hr(
        &report_name,
        vec![("Fan", Some("TestFan"))],
        vec![("Failure", Some("Sensor"), "Failure")],
    );

    simulate_hardware_health_report(&env, &host_machine_id, report.clone()).await;
    let stored_report = load_snapshot(&env, &host_machine_id)
        .await?
        .host_snapshot
        .health_reports
        .merges
        .values()
        .next()
        .unwrap()
        .clone();
    check_time(&stored_report);
    check_reports_equal(&report_name, report, stored_report);

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_health_aggregation(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    // The aggregate health should have no alerts.
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert_eq!(aggregate_health.source, "aggregate-host-health");
    check_time(&aggregate_health);
    assert_eq!(aggregate_health.alerts, vec![]);

    // we start off with no overrides
    let mut override_metrics = env
        .test_meter
        .formatted_metrics("carbide_hosts_health_overrides_count");
    override_metrics.sort();
    assert_eq!(
        override_metrics,
        vec![
            "{fresh=\"true\",in_use=\"false\",override_type=\"merge\"} 1".to_string(),
            "{fresh=\"true\",in_use=\"false\",override_type=\"replace\"} 0".to_string(),
            "{fresh=\"true\",in_use=\"true\",override_type=\"merge\"} 0".to_string(),
            "{fresh=\"true\",in_use=\"true\",override_type=\"replace\"} 0".to_string()
        ]
    );

    // Let forge-dpu-agent submit a report which claims the DPU is no longer healthy
    let dpu_health = hr(
        "dpu-health",
        vec![("Success1", None)],
        vec![("Failure1", None, "Reason1")],
    );
    network_configured_with_health(&env, &dpu_machine_id, Some(dpu_health.clone().into())).await;

    // Aggregate health in snapshot indicates the DPU issue
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    check_time(&aggregate_health);
    check_reports_equal(
        "aggregate-host-health",
        aggregate_health,
        dpu_health.clone(),
    );

    // Simulate the same alert as DPU but with a different message and from hardware health.
    let hardware_health = hr(
        &format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"),
        vec![("Fan", Some("TestFan"))],
        vec![("Failure1", None, "HardwareReason")],
    );
    simulate_hardware_health_report(&env, &host_machine_id, hardware_health.clone()).await;

    // Aggregate health in snapshot reflects merge
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    check_time(&aggregate_health);
    check_reports_equal(
        "aggregate-host-health",
        aggregate_health,
        hr(
            "",
            vec![("Fan", Some("TestFan")), ("Success1", None)],
            vec![("Failure1", None, "Reason1\nHardwareReason")],
        ),
    );

    // Add an alert via override.
    let r#override = hr(
        "add-host-failure",
        vec![],
        vec![("Fan", Some("TestFan"), "Reason")],
    );
    send_health_report_entry(
        &env,
        &host_machine_id,
        (r#override, HealthReportApplyMode::Merge),
    )
    .await;

    // Override is visible in metrics - requires a statecontroller iteration to update metrics
    env.run_machine_state_controller_iteration().await;
    let mut override_metrics = env
        .test_meter
        .formatted_metrics("carbide_hosts_health_overrides_count");
    override_metrics.sort();
    assert_eq!(
        override_metrics,
        vec![
            "{fresh=\"true\",in_use=\"false\",override_type=\"merge\"} 2".to_string(),
            "{fresh=\"true\",in_use=\"false\",override_type=\"replace\"} 0".to_string(),
            "{fresh=\"true\",in_use=\"true\",override_type=\"merge\"} 0".to_string(),
            "{fresh=\"true\",in_use=\"true\",override_type=\"replace\"} 0".to_string()
        ]
    );

    let m = find_machine(&env, &host_machine_id).await;
    assert_eq!(
        m.status.as_ref().unwrap().health_sources,
        vec![
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Merge as i32,
                source: "add-host-failure".to_string()
            },
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Merge as i32,
                source: "hardware-health.health".to_string()
            }
        ]
    );
    let aggregate_health = aggregate(m).unwrap();
    let merged_hr = hr(
        "",
        vec![("Success1", None)],
        vec![
            ("Failure1", None, "Reason1\nHardwareReason"),
            ("Fan", Some("TestFan"), "Reason"),
        ],
    );
    // The success should now be a failure.
    check_reports_equal("aggregate-host-health", aggregate_health, merged_hr.clone());

    // We can also use the FindMachinesByIds API to verify Health of Host and DPU
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    check_reports_equal("aggregate-host-health", aggregate_health, merged_hr.clone());

    // Replace the machine's health report entirely with a blank report.
    let r#override = hr("replace-host-report", vec![], vec![]);
    send_health_report_entry(
        &env,
        &host_machine_id,
        (r#override.clone(), HealthReportApplyMode::Replace),
    )
    .await;
    // Override is visible in metrics - requires a statecontroller iteration to update metrics
    env.run_machine_state_controller_iteration().await;
    let mut override_metrics = env
        .test_meter
        .formatted_metrics("carbide_hosts_health_overrides_count");
    override_metrics.sort();
    assert_eq!(
        override_metrics,
        vec![
            "{fresh=\"true\",in_use=\"false\",override_type=\"merge\"} 2".to_string(),
            "{fresh=\"true\",in_use=\"false\",override_type=\"replace\"} 1".to_string(),
            "{fresh=\"true\",in_use=\"true\",override_type=\"merge\"} 0".to_string(),
            "{fresh=\"true\",in_use=\"true\",override_type=\"replace\"} 0".to_string()
        ]
    );

    let m = find_machine(&env, &host_machine_id).await;
    assert_eq!(
        m.status.as_ref().unwrap().health_sources,
        vec![
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Merge as i32,
                source: "add-host-failure".to_string()
            },
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Merge as i32,
                source: "hardware-health.health".to_string()
            },
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Replace as i32,
                source: "replace-host-report".to_string()
            }
        ]
    );
    let aggregate_health = aggregate(m).unwrap();
    // The whole report should now be empty.
    check_reports_equal(
        "aggregate-host-health",
        aggregate_health,
        r#override.clone(),
    );
    // We can also use the FindMachinesByIds API to verify Health of Host and DPU
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    check_reports_equal("aggregate-host-health", aggregate_health, r#override);

    // Remove the blank report override
    remove_health_report_entry(&env, &host_machine_id, "replace-host-report".to_string()).await;
    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    // The report should be back to as it was.
    check_reports_equal("aggregate-host-health", aggregate_health, merged_hr);

    Ok(())
}

#[crate::sqlx_test]
async fn test_machine_health_history(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    // Get the initial amount of health records. The ingestion history is ignored
    // for the remaining test
    let initial_records = load_host_health_history(&env, &host_machine_id).await;
    let num_ignored_records = initial_records.len();

    // Add an alert via override.
    let mut health1 = hr(
        "test-report-1",
        vec![],
        vec![("Fan", Some("TestFan"), "Reason")],
    );
    health1.observed_at = Some(chrono::Utc::now());
    health1.alerts[0].in_alert_since = Some(chrono::Utc::now());
    send_health_report_entry(
        &env,
        &host_machine_id,
        (health1.clone(), HealthReportApplyMode::Replace),
    )
    .await;

    // Run the state controller twice to update history
    // The 2nd run should not yield a new entry
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    // Change some in-alert times on the health report. They shouldn't add another record
    let mut health1_newdate = health1.clone();
    health1_newdate.observed_at = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
    health1.alerts[0].in_alert_since = Some(chrono::Utc::now() + chrono::Duration::minutes(3));
    send_health_report_entry(
        &env,
        &host_machine_id,
        (health1_newdate.clone(), HealthReportApplyMode::Replace),
    )
    .await;

    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let health2 = hr(
        "test-report-1",
        vec![],
        vec![
            ("Fan", Some("TestFan"), "Reason"),
            ("Fan", Some("TestFan2"), "Other Reason"),
        ],
    );
    send_health_report_entry(
        &env,
        &host_machine_id,
        (health2.clone(), HealthReportApplyMode::Replace),
    )
    .await;

    // Run the state controller twice to update history
    // The 2nd run should not yield a new entry
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    let health3 = hr("test-report-3", vec![], vec![]);
    remove_health_report_entry(&env, &host_machine_id, "test-report-1".to_string()).await;
    env.run_machine_state_controller_iteration().await;
    env.run_machine_state_controller_iteration().await;

    // Check the health history
    let mut records = load_host_health_history(&env, &host_machine_id).await;
    let records_len = records.len();
    let _ = records.split_off(records_len.saturating_sub(num_ignored_records));

    assert_eq!(records.len(), 3);

    check_reports_equal(
        "aggregate-host-health",
        records.remove(0).health.unwrap().try_into().unwrap(),
        health3,
    );
    check_reports_equal(
        "aggregate-host-health",
        records.remove(0).health.unwrap().try_into().unwrap(),
        health2,
    );
    check_reports_equal(
        "aggregate-host-health",
        records.remove(0).health.unwrap().try_into().unwrap(),
        health1,
    );

    // Test limit
    const EXPECTED_LIMIT: usize = 250;
    for i in 0..EXPECTED_LIMIT + 10 {
        let health = hr(
            "override",
            vec![],
            vec![(&format!("Fan {i}"), None, "Reason")],
        );
        send_health_report_entry(
            &env,
            &host_machine_id,
            (health.clone(), HealthReportApplyMode::Replace),
        )
        .await;
        env.run_machine_state_controller_iteration().await;
    }
    let records = load_host_health_history(&env, &host_machine_id).await;
    assert_eq!(records.len(), EXPECTED_LIMIT);

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpu_replace_override_replaces_dpu_health_contribution(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();
    let merge_report = hr(
        "manual-dpu-merge",
        vec![],
        vec![("ManualDpuMerge", Some("DPU"), "Manual DPU merge alert")],
    );
    send_health_report_entry(
        &env,
        &dpu_machine_id,
        (merge_report, HealthReportApplyMode::Merge),
    )
    .await;

    let host_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert!(
        host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuMerge")
    );

    let replace_report = hr(
        "manual-dpu-replace",
        vec![],
        vec![("ManualDpuReplace", Some("DPU"), "Manual DPU replace alert")],
    );
    send_health_report_entry(
        &env,
        &dpu_machine_id,
        (replace_report, HealthReportApplyMode::Replace),
    )
    .await;

    let dpu_health = load_health_via_find_machines_by_ids(&env, &dpu_machine_id)
        .await
        .unwrap();
    assert!(
        dpu_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuReplace")
    );
    assert!(
        !dpu_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuMerge")
    );

    let host_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert!(
        host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuReplace")
    );
    assert!(
        !host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuMerge")
    );

    remove_health_report_entry(&env, &dpu_machine_id, "manual-dpu-replace".to_string()).await;

    let host_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert!(
        host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuMerge")
    );
    assert!(
        !host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuReplace")
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_can_manage_dpu_merge_health_report(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();
    let report = hr(
        "manual-dpu-alert",
        vec![],
        vec![("ManualDpuAlert", Some("DPU"), "Manual DPU alert")],
    );

    send_health_report_entry(
        &env,
        &dpu_machine_id,
        (report, HealthReportApplyMode::Merge),
    )
    .await;

    let dpu = find_machine(&env, &dpu_machine_id).await;
    assert!(
        dpu.status
            .as_ref()
            .unwrap()
            .health_sources
            .iter()
            .any(|source| {
                source.mode == HealthReportApplyMode::Merge as i32
                    && source.source == "manual-dpu-alert"
            })
    );

    let host_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert!(
        host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuAlert")
    );

    remove_health_report_entry(&env, &dpu_machine_id, "manual-dpu-alert".to_string()).await;

    let dpu = find_machine(&env, &dpu_machine_id).await;
    assert!(
        !dpu.status
            .as_ref()
            .unwrap()
            .health_sources
            .iter()
            .any(|source| source.source == "manual-dpu-alert")
    );

    let host_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert!(
        !host_health
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == "ManualDpuAlert")
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_double_insert(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, _) = create_managed_host(&env).await.into();

    let hardware_health = hr(
        &format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"),
        vec![("Fan", None)],
        vec![],
    );
    simulate_hardware_health_report(&env, &host_machine_id, hardware_health.clone()).await;

    // Inserting a Replace override then a Merge override with the same source
    // should result in the Replace override being replaced.
    use rpc::forge::forge_server::Forge;
    use tonic::Request;
    let _ = env
        .api
        .insert_machine_health_report(Request::new(rpc::forge::InsertMachineHealthReportRequest {
            machine_id: Some(host_machine_id.into()),
            health_report_entry: Some(rpc::forge::HealthReportEntry {
                report: Some(health_report::HealthReport::empty("over".to_string()).into()),
                mode: health_report::HealthReportApplyMode::Replace as i32,
            }),
        }))
        .await
        .unwrap();

    let aggregate_health = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    check_reports_equal(
        "aggregate-host-health",
        aggregate_health,
        health_report::HealthReport::empty("".to_string()),
    );

    let merge_hr = hr("over", vec![], vec![("Fan2", None, "")]);
    let _ = env
        .api
        .insert_machine_health_report(Request::new(rpc::forge::InsertMachineHealthReportRequest {
            machine_id: Some(host_machine_id.into()),
            health_report_entry: Some(rpc::forge::HealthReportEntry {
                report: Some(merge_hr.clone().into()),
                mode: health_report::HealthReportApplyMode::Merge as i32,
            }),
        }))
        .await
        .unwrap();
    let m = find_machine(&env, &host_machine_id).await;
    assert_eq!(
        m.status.as_ref().unwrap().health_sources,
        vec![
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Merge as i32,
                source: "hardware-health.health".to_string()
            },
            HealthSourceOrigin {
                mode: HealthReportApplyMode::Merge as i32,
                source: "over".to_string()
            }
        ]
    );
    let aggregate_health = aggregate(m).unwrap();

    let mut expected_health = hardware_health;
    expected_health.merge(&merge_hr);
    check_reports_equal("aggregate-host-health", aggregate_health, expected_health);

    Ok(())
}

/// A continuously-firing alert re-reported through `InsertMachineHealthReport`
/// must keep the `in_alert_since` of its first occurrence, so operators can tell
/// how long the condition has lasted. New alerts appearing in a later report
/// still get a fresh timestamp.
#[crate::sqlx_test]
async fn test_insert_machine_health_report_retains_in_alert_since(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    let insert = async |report: health_report::HealthReport| {
        env.api
            .insert_machine_health_report(Request::new(
                rpc::forge::InsertMachineHealthReportRequest {
                    machine_id: Some(host_machine_id.into()),
                    health_report_entry: Some(rpc::forge::HealthReportEntry {
                        report: Some(report.into()),
                        mode: HealthReportApplyMode::Merge as i32,
                    }),
                },
            ))
            .await
            .unwrap();
    };

    // Alert fires for the first time.
    let report = hr("bmc-sensors", vec![], vec![("Sensor", Some("Fan1"), "hot")]);
    insert(report.clone()).await;

    let first = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    let first_seen = first.alerts[0].in_alert_since.expect("in_alert_since set");

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Same alert still firing on the next poll: the original timestamp is kept.
    insert(report.clone()).await;

    let second = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    assert_eq!(second.alerts.len(), 1);
    assert_eq!(
        second.alerts[0].in_alert_since,
        Some(first_seen),
        "re-reported alert must retain its original in_alert_since"
    );

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // A second alert appears alongside the first: the pre-existing alert keeps
    // its timestamp, the new one is stamped now.
    let grown = hr(
        "bmc-sensors",
        vec![],
        vec![
            ("Sensor", Some("Fan1"), "hot"),
            ("Sensor", Some("Fan2"), "hot"),
        ],
    );
    insert(grown).await;

    let third = load_health_via_find_machines_by_ids(&env, &host_machine_id)
        .await
        .unwrap();
    let find = |target: &str| {
        third
            .alerts
            .iter()
            .find(|a| a.target.as_deref() == Some(target))
            .unwrap_or_else(|| panic!("alert for {target} missing"))
            .in_alert_since
            .expect("in_alert_since set")
    };
    assert_eq!(find("Fan1"), first_seen, "existing alert keeps timestamp");
    assert!(
        find("Fan2") > first_seen,
        "newly appearing alert is stamped with the current time"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_count_unhealthy_nonupgrading_host_machines(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;

    let (host_machine_id, _) = create_managed_host(&env).await.into();

    let mut txn = env.pool.begin().await?;
    let machine_ids = db::machine::find_machine_ids(
        txn.as_mut(),
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?;
    let options = model::machine::LoadSnapshotOptions {
        include_history: false,
        include_instance_data: false,
        host_health_config: HostHealthConfig {
            hardware_health_reports: model::machine::HardwareHealthReportsConfig::Enabled,
            dpu_agent_version_staleness_threshold: chrono::Duration::days(1),
            prevent_allocations_on_stale_dpu_agent_version: false,
            prevent_allocations_on_scout_heartbeat_timeout: false,
            suppress_external_alerting_on_scout_heartbeat_timeout: true,
        },
    };
    let all_machines =
        db::managed_host::load_by_machine_ids(txn.as_mut(), &machine_ids, options).await?;

    assert_eq!(
        db::machine::count_healthy_unhealthy_host_machines(&all_machines),
        (1, 0)
    );
    txn.commit().await?;

    let r#override = hr(
        "add-host-failure",
        vec![],
        vec![("Fan", Some("TestFan"), "Reason")],
    );
    send_health_report_entry(
        &env,
        &host_machine_id,
        (r#override, HealthReportApplyMode::Merge),
    )
    .await;
    let health2 = hr(
        "test-report-1",
        vec![],
        vec![
            ("Fan", Some("TestFan"), "Reason"),
            ("Fan", Some("TestFan2"), "Other Reason"),
        ],
    );
    send_health_report_entry(
        &env,
        &host_machine_id,
        (health2.clone(), HealthReportApplyMode::Replace),
    )
    .await;

    let mut txn = env.pool.begin().await?;
    let machine_ids = db::machine::find_machine_ids(
        txn.as_mut(),
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?;
    let options = model::machine::LoadSnapshotOptions {
        include_history: false,
        include_instance_data: false,
        host_health_config: HostHealthConfig {
            hardware_health_reports: model::machine::HardwareHealthReportsConfig::Enabled,
            dpu_agent_version_staleness_threshold: chrono::Duration::days(1),
            prevent_allocations_on_stale_dpu_agent_version: false,
            prevent_allocations_on_scout_heartbeat_timeout: false,
            suppress_external_alerting_on_scout_heartbeat_timeout: true,
        },
    };
    let all_machines =
        db::managed_host::load_by_machine_ids(txn.as_mut(), &machine_ids, options).await?;

    assert_eq!(
        db::machine::count_healthy_unhealthy_host_machines(&all_machines),
        (1, 1)
    );
    txn.commit().await?;

    Ok(())
}

async fn create_env(pool: sqlx::PgPool) -> TestEnv {
    let mut config = get_config();
    config.host_health.hardware_health_reports = HardwareHealthReportsConfig::Enabled;
    create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await
}

/// Creates a health report.
fn hr(
    source: &str,
    successes: Vec<(&str, Option<&str>)>,
    alerts: Vec<(&str, Option<&str>, &str)>,
) -> health_report::HealthReport {
    health_report::HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: None,
        successes: successes
            .into_iter()
            .map(|(id, target)| health_report::HealthProbeSuccess {
                id: id.to_string().parse().unwrap(),
                target: target.map(|t| t.to_string()),
            })
            .collect(),
        alerts: alerts
            .into_iter()
            .map(|(id, target, message)| health_report::HealthProbeAlert {
                id: id.to_string().parse().unwrap(),
                target: target.map(|t| t.to_string()),
                in_alert_since: None,
                message: message.to_string(),
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_host_state_changes(),
                ],
            })
            .collect(),
    }
}

/// Loads machine snapshot
async fn load_snapshot(
    env: &TestEnv,
    host_machine_id: &::carbide_uuid::machine::MachineId,
) -> Result<model::machine::ManagedHostStateSnapshot, Box<dyn std::error::Error>> {
    let host_health_config = HostHealthConfig {
        hardware_health_reports: HardwareHealthReportsConfig::Enabled,
        dpu_agent_version_staleness_threshold: Default::default(),
        prevent_allocations_on_stale_dpu_agent_version: false,
        prevent_allocations_on_scout_heartbeat_timeout: false,
        suppress_external_alerting_on_scout_heartbeat_timeout: true,
    };
    let snapshot = db::managed_host::load_snapshot(
        &mut env.db_reader(),
        host_machine_id,
        LoadSnapshotOptions::default().with_host_health(host_health_config),
    )
    .await?
    .unwrap();
    Ok(snapshot)
}

/// Calls get_machine api
async fn find_machine(
    env: &TestEnv,
    machine_id: &::carbide_uuid::machine::MachineId,
) -> rpc::Machine {
    env.api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![*machine_id],
            include_history: true,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0)
}

async fn load_host_health_history(
    env: &TestEnv,
    machine_id: &::carbide_uuid::machine::MachineId,
) -> Vec<::rpc::forge::HealthHistoryRecord> {
    env.api
        .find_machine_health_histories(tonic::Request::new(
            ::rpc::forge::MachineHealthHistoriesRequest {
                machine_ids: vec![*machine_id],
                start_time: None,
                end_time: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .histories
        .remove(&machine_id.to_string())
        .unwrap()
        .records
}

/// Loads aggregate health via get_machine api
fn aggregate(m: rpc::Machine) -> Option<health_report::HealthReport> {
    m.status
        .and_then(|s| s.health)
        .map(|r| r.try_into().unwrap())
}

/// Loads aggregate health via FindMachinesByIds api
async fn load_health_via_find_machines_by_ids(
    env: &TestEnv,
    machine_id: &::carbide_uuid::machine::MachineId,
) -> Option<health_report::HealthReport> {
    env.api
        .find_machines_by_ids(Request::new(rpc::forge::MachinesByIdsRequest {
            machine_ids: vec![*machine_id],
            include_history: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .remove(0)
        .status
        .and_then(|s| s.health)
        .map(|r| r.try_into().unwrap())
}

/// Checks that the health report was generated in the past, but less than 60
/// seconds in the past.
fn check_time(report: &health_report::HealthReport) {
    let elapsed_since_report =
        chrono::Utc::now().signed_duration_since(report.observed_at.unwrap());
    assert!(
        elapsed_since_report > chrono::TimeDelta::zero()
            && elapsed_since_report < chrono::TimeDelta::new(60, 0).unwrap()
    );
}

/// Checks that [`reported`] has the specified [`source`]. Updates [`expected`]
/// to have this source and checks that the reports are equal (not considering
/// timestamps).
fn check_reports_equal(
    source: &str,
    reported: health_report::HealthReport,
    mut expected: health_report::HealthReport,
) {
    /// Checks that 2 healthreports are equal, without taking timestamps into consideration
    fn check_health_reports_equal(
        a: &health_report::HealthReport,
        b: &health_report::HealthReport,
    ) {
        fn erase_timestamps(report: &mut health_report::HealthReport) {
            report.observed_at = None;
            for alert in report.alerts.iter_mut() {
                alert.in_alert_since = None;
            }
        }

        let mut a = a.clone();
        let mut b = b.clone();
        erase_timestamps(&mut a);
        erase_timestamps(&mut b);
        assert_eq!(a, b)
    }
    assert_eq!(reported.source, source);
    expected.source = source.to_string();
    check_health_reports_equal(&reported, &expected);
}

/// Loads health alerts by time range via FindMachineHealthHistories RPC with time filtering
async fn load_health_alerts_by_time_range(
    env: &TestEnv,
    machine_id: &::carbide_uuid::machine::MachineId,
    start_time: chrono::DateTime<chrono::Utc>,
    end_time: chrono::DateTime<chrono::Utc>,
) -> Vec<::rpc::forge::HealthHistoryRecord> {
    let response = env
        .api
        .find_machine_health_histories(tonic::Request::new(
            ::rpc::forge::MachineHealthHistoriesRequest {
                machine_ids: vec![*machine_id],
                start_time: Some(start_time.into()),
                end_time: Some(end_time.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let machine_id_str = machine_id.to_string();
    response
        .histories
        .get(&machine_id_str)
        .map(|h| h.records.clone())
        .unwrap_or_default()
}

/// Inserts health report and processes it via state controller
async fn insert_health_and_process(
    env: &TestEnv,
    machine_id: &::carbide_uuid::machine::MachineId,
    health: health_report::HealthReport,
) {
    send_health_report_entry(env, machine_id, (health, HealthReportApplyMode::Replace)).await;
    env.run_machine_state_controller_iteration().await;
}

#[crate::sqlx_test]
async fn test_tenant_reported_issue_health_override_template(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    // Create a TenantReportedIssue health override using the API
    let tenant_issue_override = health_report::HealthReport {
        source: "tenant-reported-issue".to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::from_str("TenantReportedIssue").unwrap(),
            target: Some("tenant-reported".to_string()),
            in_alert_since: None,
            message: "Customer reported intermittent network connectivity issues".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_allocations(),
                health_report::HealthAlertClassification::suppress_external_alerting(),
            ],
        }],
    };

    // Apply the override
    send_health_report_entry(
        &env,
        &host_machine_id,
        (tenant_issue_override.clone(), HealthReportApplyMode::Merge),
    )
    .await;

    // Verify the override is applied
    let machine = find_machine(&env, &host_machine_id).await;

    // Check that the override was stored
    assert_eq!(machine.status.as_ref().unwrap().health_sources.len(), 2);
    assert_eq!(
        machine.status.as_ref().unwrap().health_sources[1].mode,
        HealthReportApplyMode::Merge as i32
    );
    assert_eq!(
        machine.status.as_ref().unwrap().health_sources[1].source,
        "tenant-reported-issue"
    );

    // Verify aggregate health includes the override
    let aggregate_health = aggregate(machine).unwrap();
    assert_eq!(aggregate_health.source, "aggregate-host-health");
    assert_eq!(aggregate_health.alerts.len(), 1);
    assert_eq!(
        aggregate_health.alerts[0].id.to_string(),
        "TenantReportedIssue"
    );
    assert_eq!(
        aggregate_health.alerts[0].target,
        Some("tenant-reported".to_string())
    );
    assert_eq!(
        aggregate_health.alerts[0].message,
        "Customer reported intermittent network connectivity issues"
    );

    // Verify classifications
    assert!(
        aggregate_health.alerts[0]
            .classifications
            .contains(&health_report::HealthAlertClassification::prevent_allocations())
    );
    assert!(
        aggregate_health.alerts[0]
            .classifications
            .contains(&health_report::HealthAlertClassification::suppress_external_alerting())
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_request_repair_health_override_template(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    // Create a RequestRepair health override using the API
    let repair_request_override = health_report::HealthReport {
        source: health_report::REPAIR_REQUEST_MERGE_SOURCE.to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::from_str("RequestRepair").unwrap(),
            target: Some("repair-requested".to_string()),
            in_alert_since: None,
            message: "Hardware diagnostics indicate memory failure requiring replacement"
                .to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_allocations(),
                health_report::HealthAlertClassification::suppress_external_alerting(),
            ],
        }],
    };

    // Apply the override
    send_health_report_entry(
        &env,
        &host_machine_id,
        (
            repair_request_override.clone(),
            HealthReportApplyMode::Merge,
        ),
    )
    .await;

    // Verify the override is applied
    let machine = find_machine(&env, &host_machine_id).await;

    // Check that the override was stored
    assert_eq!(machine.status.as_ref().unwrap().health_sources.len(), 2);
    assert_eq!(
        machine.status.as_ref().unwrap().health_sources[1].mode,
        HealthReportApplyMode::Merge as i32
    );
    assert_eq!(
        machine.status.as_ref().unwrap().health_sources[1].source,
        health_report::REPAIR_REQUEST_MERGE_SOURCE
    );

    // Verify aggregate health includes the override
    let aggregate_health = aggregate(machine).unwrap();
    assert_eq!(aggregate_health.source, "aggregate-host-health");
    assert_eq!(aggregate_health.alerts.len(), 1);
    assert_eq!(aggregate_health.alerts[0].id.to_string(), "RequestRepair");
    assert_eq!(
        aggregate_health.alerts[0].target,
        Some("repair-requested".to_string())
    );
    assert_eq!(
        aggregate_health.alerts[0].message,
        "Hardware diagnostics indicate memory failure requiring replacement"
    );

    // Verify classifications
    assert!(
        aggregate_health.alerts[0]
            .classifications
            .contains(&health_report::HealthAlertClassification::prevent_allocations())
    );
    assert!(
        aggregate_health.alerts[0]
            .classifications
            .contains(&health_report::HealthAlertClassification::suppress_external_alerting())
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_tenant_reported_issue_and_request_repair_combined(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    // Apply both overrides to the same machine
    let tenant_issue_override = health_report::HealthReport {
        source: "tenant-reported-issue".to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::from_str("TenantReportedIssue").unwrap(),
            target: Some("tenant-reported".to_string()),
            in_alert_since: None,
            message: "Customer reports performance degradation".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_allocations(),
                health_report::HealthAlertClassification::suppress_external_alerting(),
            ],
        }],
    };

    let repair_request_override = health_report::HealthReport {
        source: health_report::REPAIR_REQUEST_MERGE_SOURCE.to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::from_str("RequestRepair").unwrap(),
            target: Some("repair-requested".to_string()),
            in_alert_since: None,
            message: "Diagnostics confirm hardware issue needs repair".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_allocations(),
                health_report::HealthAlertClassification::suppress_external_alerting(),
            ],
        }],
    };

    // Apply both overrides
    send_health_report_entry(
        &env,
        &host_machine_id,
        (tenant_issue_override, HealthReportApplyMode::Merge),
    )
    .await;

    send_health_report_entry(
        &env,
        &host_machine_id,
        (repair_request_override, HealthReportApplyMode::Merge),
    )
    .await;

    // Verify both overrides are stored
    let machine = find_machine(&env, &host_machine_id).await;

    // Get aggregate health first to avoid partial move issues
    let aggregate_health = aggregate(machine.clone()).unwrap();

    // Check that both overrides were stored
    assert_eq!(machine.status.as_ref().unwrap().health_sources.len(), 3);
    let sources: Vec<String> = machine
        .status
        .as_ref()
        .unwrap()
        .health_sources
        .iter()
        .map(|o| o.source.clone())
        .collect();
    assert!(sources.contains(&"tenant-reported-issue".to_string()));
    assert!(sources.contains(&health_report::REPAIR_REQUEST_MERGE_SOURCE.to_string()));

    // All should be merge mode
    for override_entry in &machine.status.as_ref().unwrap().health_sources {
        assert_eq!(override_entry.mode, HealthReportApplyMode::Merge as i32);
    }
    assert_eq!(aggregate_health.alerts.len(), 2);

    // Find both alerts by ID
    let alert_ids: Vec<String> = aggregate_health
        .alerts
        .iter()
        .map(|alert| alert.id.to_string())
        .collect();
    assert!(alert_ids.contains(&"TenantReportedIssue".to_string()));
    assert!(alert_ids.contains(&"RequestRepair".to_string()));

    // Verify all alerts have SuppressExternalAlerting
    for alert in &aggregate_health.alerts {
        assert!(
            alert
                .classifications
                .contains(&health_report::HealthAlertClassification::suppress_external_alerting())
        );
        assert!(
            alert
                .classifications
                .contains(&health_report::HealthAlertClassification::prevent_allocations())
        );
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_health_alerts_by_time_range(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // SETUP: Create test environment and machine
    let env = create_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    // Record start time
    let time_before = chrono::Utc::now();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // INSERT 1: First health report with 2 alerts + 1 success
    let health1 = hr(
        "test-source-1",
        vec![("Success1", None)],
        vec![
            ("Failure1", Some("TestComponent1"), "First test failure"),
            ("Failure2", None, "Second test failure"),
        ],
    );
    insert_health_and_process(&env, &host_machine_id, health1).await;

    let time_after_first = chrono::Utc::now();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // INSERT 2: Second health report with 1 alert + 1 success
    let health2 = hr(
        "test-source-2",
        vec![("Success2", Some("TestComponent2"))],
        vec![("Fan", Some("TestFan"), "Fan failure detected")],
    );
    insert_health_and_process(&env, &host_machine_id, health2).await;

    let time_after_second = chrono::Utc::now();
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // INSERT 3: Third health report with 1 alert, no successes
    let health3 = hr(
        "test-source-3",
        vec![],
        vec![("Failure3", Some("TestComponent3"), "Third test failure")],
    );
    insert_health_and_process(&env, &host_machine_id, health3).await;

    let time_after_third = chrono::Utc::now();

    // TEST 1: Query for ALL alerts (full time range)
    let all_alerts =
        load_health_alerts_by_time_range(&env, &host_machine_id, time_before, time_after_third)
            .await;

    assert_eq!(all_alerts.len(), 3, "Should have 3 alert records");

    // Verify first record has 1 alert
    let health2 = all_alerts[0].health.as_ref().unwrap();
    assert_eq!(health2.alerts.len(), 1);
    assert_eq!(health2.alerts[0].id, "Failure3");
    assert_eq!(health2.alerts[0].target, Some("TestComponent3".to_string()));
    assert_eq!(health2.source, "aggregate-host-health");

    // Verify second record has 1 alert (success filtered out!)
    let health1 = all_alerts[1].health.as_ref().unwrap();
    assert_eq!(health1.alerts.len(), 1);
    assert_eq!(health1.alerts[0].id, "Fan");
    assert_eq!(health1.alerts[0].target, Some("TestFan".to_string()));
    assert_eq!(health1.alerts[0].message, "Fan failure detected");
    assert_eq!(health1.source, "aggregate-host-health");

    // Verify third record has 2 alerts (no successes!)
    let health0 = all_alerts[2].health.as_ref().unwrap();
    assert_eq!(health0.alerts.len(), 2);
    assert_eq!(health0.alerts[0].id, "Failure1");
    assert_eq!(health0.alerts[0].target, Some("TestComponent1".to_string()));
    assert_eq!(health0.alerts[0].message, "First test failure");
    assert_eq!(health0.alerts[1].id, "Failure2");
    assert_eq!(health0.source, "aggregate-host-health");

    // TEST 2: Query MIDDLE time range only (should get only second record)
    let middle_alerts = load_health_alerts_by_time_range(
        &env,
        &host_machine_id,
        time_after_first,
        time_after_second,
    )
    .await;

    assert_eq!(
        middle_alerts.len(),
        1,
        "Should have 1 record in middle range"
    );
    assert_eq!(
        middle_alerts[0].health.as_ref().unwrap().alerts[0].id,
        "Fan"
    );

    // TEST 3: Query FUTURE time range (should be empty)
    let future_time = chrono::Utc::now() + chrono::Duration::hours(1);
    let no_alerts = load_health_alerts_by_time_range(
        &env,
        &host_machine_id,
        future_time,
        future_time + chrono::Duration::hours(1),
    )
    .await;

    assert_eq!(
        no_alerts.len(),
        0,
        "Should have no records in future time range"
    );

    // TEST 4: Verify timestamps are present and reasonable
    for record in &all_alerts {
        assert!(record.time.is_some(), "Each record should have a timestamp");
        let timestamp = record.time.as_ref().unwrap();
        assert!(timestamp.seconds > 0, "Timestamp should be valid");
    }

    Ok(())
}
