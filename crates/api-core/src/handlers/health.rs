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
use carbide_uuid::machine::MachineId;
use health_report::HealthReportApplyMode;
use model::machine::machine_search_config::MachineSearchConfig;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::Api;
use crate::auth::AuthContext;
use crate::handlers::utils::convert_and_log_machine_id;

pub(crate) async fn list_machine_health_reports(
    api: &Api,
    machine_id: Request<MachineId>,
) -> Result<Response<rpc::ListHealthReportResponse>, Status> {
    let mut txn = api.txn_begin().await?;

    let machine_id: MachineId = convert_and_log_machine_id(Some(&machine_id.into_inner()))?;

    let machine = db::machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;

    txn.commit().await?;

    Ok(Response::new(rpc::ListHealthReportResponse {
        health_report_entries: machine
            .health_reports
            .clone()
            .into_iter()
            .map(|o| HealthReportEntry {
                report: Some(o.0.into()),
                mode: o.1 as i32,
            })
            .collect(),
    }))
}

async fn remove_by_source(
    txn: &mut PgConnection,
    machine_id: MachineId,
    source: String,
) -> Result<(), CarbideError> {
    let machine = db::machine::find_one(
        &mut *txn,
        &machine_id,
        MachineSearchConfig {
            // Technically,  an update is going to happen,
            // but we don't seem to need coordination/locking.
            for_update: false,
            ..Default::default()
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "machine",
        id: machine_id.to_string(),
    })?;

    // Ensure this source already exists in override list
    let mode = if machine.health_reports.replace.as_ref().map(|o| &o.source) == Some(&source) {
        HealthReportApplyMode::Replace
    } else if machine.health_reports.merges.contains_key(&source) {
        HealthReportApplyMode::Merge
    } else {
        return Err(CarbideError::NotFoundError {
            kind: "machine with source",
            id: source.to_string(),
        });
    };

    db::machine::remove_health_report(txn, &machine_id, mode, &source).await?;

    Ok(())
}

pub(crate) async fn insert_machine_health_report(
    api: &Api,
    request: Request<rpc::InsertMachineHealthReportRequest>,
) -> Result<Response<()>, Status> {
    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);

    let rpc::InsertMachineHealthReportRequest {
        machine_id,
        health_report_entry: Some(rpc::HealthReportEntry { report, mode }),
    } = request.into_inner()
    else {
        return Err(CarbideError::MissingArgument("health_report_entry").into());
    };
    let machine_id: MachineId = convert_and_log_machine_id(machine_id.as_ref())?;
    let Some(report) = report else {
        return Err(CarbideError::MissingArgument("report").into());
    };
    let Ok(mode) = rpc::HealthReportApplyMode::try_from(mode) else {
        return Err(CarbideError::InvalidArgument("mode".to_string()).into());
    };
    let mode: HealthReportApplyMode = mode.into();
    let mut txn = api.txn_begin().await?;
    let machine = db::machine::find_one(&mut txn, &machine_id, MachineSearchConfig::default())
        .await?
        .ok_or_else(|| CarbideError::NotFoundError {
            kind: "machine",
            id: machine_id.to_string(),
        })?;

    let mut report = health_report::HealthReport::try_from(report.clone())
        .map_err(|e| CarbideError::internal(e.to_string()))?;
    if report.observed_at.is_none() {
        report.observed_at = Some(chrono::Utc::now());
    }
    report.triggered_by = triggered_by;
    report.update_in_alert_since(machine.health_reports.by_source(&report.source));

    // In case a report with the same source exists, either as merge or replace,
    // remove it. If such a report does not exist, ignore error.
    match remove_by_source(&mut txn, machine_id, report.source.clone()).await {
        Ok(_) | Err(CarbideError::NotFoundError { .. }) => {}
        Err(e) => return Err(e.into()),
    }

    db::machine::insert_health_report(&mut txn, &machine_id, mode, &report, false).await?;

    txn.commit().await?;

    Ok(Response::new(()))
}

pub(crate) async fn remove_machine_health_report(
    api: &Api,
    request: Request<rpc::RemoveMachineHealthReportRequest>,
) -> Result<Response<()>, Status> {
    let mut txn = api.txn_begin().await?;

    let rpc::RemoveMachineHealthReportRequest { machine_id, source } = request.into_inner();
    let machine_id = convert_and_log_machine_id(machine_id.as_ref())?;

    remove_by_source(&mut txn, machine_id, source).await?;
    txn.commit().await?;

    Ok(Response::new(()))
}

/// Shared implementation of the `Find<Type>HealthHistories` RPCs. Each per-type
/// handler unwraps its request and forwards the object IDs, the target history
/// table, and the optional inclusive time range; the map returned by
/// [`db::health_history::find_by_object_ids`] is keyed by the object-id string,
/// which is what the shared `HealthHistories` response expects.
pub(crate) async fn find_health_histories<Id, Ts>(
    api: &Api,
    ids: Vec<Id>,
    table_id: db::health_history::HealthHistoryTableId,
    start_time: Option<Ts>,
    end_time: Option<Ts>,
) -> Result<Response<rpc::HealthHistories>, Status>
where
    Id: std::fmt::Display,
    chrono::DateTime<chrono::Utc>: TryFrom<Ts>,
{
    let max_find_by_ids = api.runtime_config.max_find_by_ids as usize;
    if ids.len() > max_find_by_ids {
        return Err(CarbideError::InvalidArgument(format!(
            "no more than {max_find_by_ids} IDs can be accepted"
        ))
        .into());
    } else if ids.is_empty() {
        return Err(
            CarbideError::InvalidArgument("at least one ID must be provided".to_string()).into(),
        );
    }

    // Convert protobuf timestamps to chrono DateTime
    let start_time = start_time
        .map(chrono::DateTime::<chrono::Utc>::try_from)
        .transpose()
        .map_err(|_| CarbideError::InvalidArgument("invalid start_time timestamp".to_string()))?;
    let end_time = end_time
        .map(chrono::DateTime::<chrono::Utc>::try_from)
        .transpose()
        .map_err(|_| CarbideError::InvalidArgument("invalid end_time timestamp".to_string()))?;

    let mut txn = api.txn_begin().await?;

    let results =
        db::health_history::find_by_object_ids(&mut txn, table_id, &ids, start_time, end_time)
            .await?;

    let mut response = rpc::HealthHistories::default();
    for (object_id, records) in results {
        response.histories.insert(
            object_id,
            ::rpc::forge::HealthHistoryRecords {
                records: records.into_iter().map(Into::into).collect(),
            },
        );
    }

    txn.commit().await?;

    Ok(Response::new(response))
}
