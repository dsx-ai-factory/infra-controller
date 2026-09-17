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
use carbide_uuid::nvlink::NvLinkDomainId;
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use rpc::forge::{self as rpc_forge};
use tonic::Request;

fn alert_report(source: &str) -> Result<HealthReport, Box<dyn std::error::Error>> {
    Ok(HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: "NvLinkDomainUnhealthy".parse()?,
            target: None,
            in_alert_since: Some(chrono::Utc::now()),
            message: "NVLink domain health issue detected".to_string(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::prevent_allocations(),
                HealthAlertClassification::hardware(),
            ],
        }],
    })
}

#[sqlx_test]
async fn test_insert_list_remove_nvlink_domain_health_report(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let domain_id: NvLinkDomainId = "00000000-0000-0000-0000-000000000001".parse()?;
    let report = alert_report("external-monitor")?;

    env.api()
        .insert_nv_link_domain_health_report(Request::new(
            rpc_forge::InsertNvLinkDomainHealthReportRequest {
                domain_id: Some(domain_id),
                health_report_entry: Some(rpc_forge::HealthReportEntry {
                    report: Some(report.clone().into()),
                    mode: rpc_forge::HealthReportApplyMode::Merge as i32,
                }),
            },
        ))
        .await?;

    let list_resp = env
        .api()
        .list_nv_link_domain_health_reports(Request::new(
            rpc_forge::ListNvLinkDomainHealthReportsRequest {
                domain_id: Some(domain_id),
            },
        ))
        .await?
        .into_inner();

    assert_eq!(list_resp.health_report_entries.len(), 1);

    let listed_entry = list_resp.health_report_entries[0].clone();
    let listed_report: HealthReport = listed_entry.report.ok_or("missing report")?.try_into()?;

    assert_eq!(listed_report.source, "external-monitor");
    assert_eq!(listed_report.alerts.len(), 1);

    env.api()
        .remove_nv_link_domain_health_report(Request::new(
            rpc_forge::RemoveNvLinkDomainHealthReportRequest {
                domain_id: Some(domain_id),
                source: "external-monitor".to_string(),
            },
        ))
        .await?;

    let list_resp = env
        .api()
        .list_nv_link_domain_health_reports(Request::new(
            rpc_forge::ListNvLinkDomainHealthReportsRequest {
                domain_id: Some(domain_id),
            },
        ))
        .await?
        .into_inner();

    assert_eq!(list_resp.health_report_entries.len(), 0);

    Ok(())
}

#[sqlx_test]
async fn test_remove_nonexistent_nvlink_domain_health_report_source(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let domain_id: NvLinkDomainId = "00000000-0000-0000-0000-000000000002".parse()?;

    let result = env
        .api()
        .remove_nv_link_domain_health_report(Request::new(
            rpc_forge::RemoveNvLinkDomainHealthReportRequest {
                domain_id: Some(domain_id),
                source: "nonexistent-source".to_string(),
            },
        ))
        .await;

    assert!(result.is_err());

    let status = result.err().ok_or("missing error")?;

    assert_eq!(status.code(), tonic::Code::NotFound);

    Ok(())
}
