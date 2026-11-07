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

use ::rpc::forge::{self as rpc, HealthReportEntry};
use carbide_uuid::nvlink::NvLinkDomainId;
use health_report::HealthReportApplyMode;
use model::health::HealthReportSources;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::auth::AuthContext;

pub(crate) async fn list_nv_link_domain_health_reports(
    api: &Api,
    request: Request<rpc::ListNvLinkDomainHealthReportsRequest>,
) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
    log_request_data(&request);

    let domain_id = request
        .into_inner()
        .domain_id
        .ok_or_else(|| CarbideError::MissingArgument("domain_id"))?;

    let health_reports =
        db::nvlink_domain_health_report::find(api.db_reader().as_mut(), &domain_id)
            .await?
            .unwrap_or_default();

    Ok(Response::new(list_response(health_reports)))
}

pub(crate) async fn insert_nv_link_domain_health_report(
    api: &Api,
    request: Request<rpc::InsertNvLinkDomainHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);

    let rpc::InsertNvLinkDomainHealthReportRequest {
        domain_id,
        health_report_entry: Some(rpc::HealthReportEntry { report, mode }),
    } = request.into_inner()
    else {
        return Err(CarbideError::MissingArgument("health_report_entry").into());
    };

    let domain_id = domain_id.ok_or_else(|| CarbideError::MissingArgument("domain_id"))?;

    let Some(report) = report else {
        return Err(CarbideError::MissingArgument("report").into());
    };

    let Ok(mode) = rpc::HealthReportApplyMode::try_from(mode) else {
        return Err(CarbideError::InvalidArgument("mode".to_string()).into());
    };

    let mode: HealthReportApplyMode = mode.into();

    let mut txn = api.txn_begin().await?;
    let health_reports = db::nvlink_domain_health_report::find(&mut txn, &domain_id)
        .await?
        .unwrap_or_default();

    let mut report = health_report::HealthReport::try_from(report.clone())
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    if report.observed_at.is_none() {
        report.observed_at = Some(chrono::Utc::now());
    }
    report.triggered_by = triggered_by;
    report.update_in_alert_since(health_reports.by_source(&report.source));

    match remove_by_source(&mut txn, &domain_id, &health_reports, report.source.clone()).await {
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    }

    db::nvlink_domain_health_report::insert_health_report(&mut txn, &domain_id, mode, &report)
        .await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn remove_nv_link_domain_health_report(
    api: &Api,
    request: Request<rpc::RemoveNvLinkDomainHealthReportRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let rpc::RemoveNvLinkDomainHealthReportRequest { domain_id, source } = request.into_inner();
    let domain_id = domain_id.ok_or_else(|| CarbideError::MissingArgument("domain_id"))?;

    let mut txn = api.txn_begin().await?;
    let health_reports = db::nvlink_domain_health_report::find(&mut txn, &domain_id)
        .await?
        .unwrap_or_default();

    remove_by_source(&mut txn, &domain_id, &health_reports, source).await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

async fn remove_by_source(
    txn: &mut db::Transaction<'_>,
    domain_id: &NvLinkDomainId,
    health_reports: &HealthReportSources,
    source: String,
) -> Result<(), CarbideError> {
    let mode = if health_reports.replace.as_ref().map(|o| &o.source) == Some(&source) {
        HealthReportApplyMode::Replace
    } else if health_reports.merges.contains_key(&source) {
        HealthReportApplyMode::Merge
    } else {
        return Err(CarbideError::NotFoundError {
            kind: "NVLink domain health report with source",
            id: source,
        });
    };

    db::nvlink_domain_health_report::remove_health_report(&mut *txn, domain_id, mode, &source)
        .await?;

    Ok(())
}

fn list_response(health_reports: HealthReportSources) -> rpc::ListHealthReportResponse {
    rpc::ListHealthReportResponse {
        health_report_entries: health_reports
            .into_iter()
            .map(|o| HealthReportEntry {
                report: Some(o.0.into()),
                mode: o.1 as i32,
            })
            .collect(),
    }
}
