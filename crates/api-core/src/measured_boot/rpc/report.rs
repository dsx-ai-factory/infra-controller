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

/*!
 * gRPC handlers for measurement report related API calls.
 */

use std::str::FromStr;

use ::rpc::errors::RpcDataConversionError;
use carbide_uuid::machine::MachineId;
use db::measured_boot::interface::report::{
    get_all_measurement_report_records, get_measurement_report_records_for_machine_id,
    match_latest_reports,
};
use measured_boot::pcr::{PcrSet, parse_pcr_index_input};
use rpc::protos::measured_boot::{
    CreateMeasurementReportRequest, CreateMeasurementReportResponse,
    DeleteMeasurementReportRequest, DeleteMeasurementReportResponse, ListMeasurementReportRequest,
    ListMeasurementReportResponse, MatchMeasurementReportRequest, MatchMeasurementReportResponse,
    MeasurementReportRecordPb, PromoteMeasurementReportRequest, PromoteMeasurementReportResponse,
    RevokeMeasurementReportRequest, RevokeMeasurementReportResponse,
    ShowMeasurementReportForIdRequest, ShowMeasurementReportForIdResponse,
    ShowMeasurementReportsForMachineRequest, ShowMeasurementReportsForMachineResponse,
    ShowMeasurementReportsRequest, ShowMeasurementReportsResponse, list_measurement_report_request,
};
use tonic::Status;

use crate::CarbideError;
use crate::api::Api;
use crate::measured_boot::convert_vec;

/// handle_create_measurement_report handles the CreateMeasurementReport
/// API endpoint.
pub(crate) async fn handle_create_measurement_report(
    api: &Api,
    req: CreateMeasurementReportRequest,
) -> Result<CreateMeasurementReportResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let report = db::measured_boot::report::new(
        &mut txn,
        MachineId::from_str(&req.machine_id).map_err(|_| {
            CarbideError::from(RpcDataConversionError::InvalidMachineId(req.machine_id))
        })?,
        &convert_vec(req.pcr_values),
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("report creation failed: {e}"),
    })?;

    txn.commit().await?;
    Ok(CreateMeasurementReportResponse {
        report: Some(report.into()),
    })
}

/// handle_delete_measurement_report handles the DeleteMeasurementReport
/// API endpoint.
pub(crate) async fn handle_delete_measurement_report(
    api: &Api,
    req: DeleteMeasurementReportRequest,
) -> Result<DeleteMeasurementReportResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let report = db::measured_boot::report::delete_for_id(
        &mut txn,
        req.report_id
            .ok_or(CarbideError::MissingArgument("report_id"))?,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("delete failed: {e}"),
    })?;

    txn.commit().await?;
    Ok(DeleteMeasurementReportResponse {
        report: Some(report.into()),
    })
}

/// handle_promote_measurement_report handles the PromoteMeasurementReport
/// API endpoint.
pub(crate) async fn handle_promote_measurement_report(
    api: &Api,
    req: PromoteMeasurementReportRequest,
) -> Result<PromoteMeasurementReportResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let pcr_set: Option<PcrSet> = match !req.pcr_registers.is_empty() {
        true => Some(parse_pcr_index_input(&req.pcr_registers).map_err(|e| {
            CarbideError::InvalidArgument(format!("pcr_register parsing failed: {e}"))
        })?),
        false => None,
    };

    let report = db::measured_boot::report::from_id(
        &mut txn,
        req.report_id
            .ok_or(CarbideError::MissingArgument("report_id"))?,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("promotion failed fetching report: {e}"),
    })?;

    let bundle = db::measured_boot::report::create_active_bundle(&mut txn, &report, &pcr_set)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("promotion failed promoting into active bundle: {e}"),
        })?;

    txn.commit().await?;
    Ok(PromoteMeasurementReportResponse {
        bundle: Some(bundle.into()),
    })
}

/// handle_revoke_measurement_report handles the RevokeMeasurementReport
/// API endpoint.
pub(crate) async fn handle_revoke_measurement_report(
    api: &Api,
    req: RevokeMeasurementReportRequest,
) -> Result<RevokeMeasurementReportResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let pcr_set: Option<PcrSet> = match &req.pcr_registers.len() {
        n if n < &1 => None,
        _ => Some(parse_pcr_index_input(&req.pcr_registers).map_err(|e| {
            CarbideError::InvalidArgument(format!("pcr_register parsing failed: {e}"))
        })?),
    };

    let report = db::measured_boot::report::from_id(
        &mut txn,
        req.report_id
            .ok_or(CarbideError::MissingArgument("report_id"))?,
    )
    .await
    .map_err(|e| CarbideError::Internal {
        message: format!("promotion failed fetching report: {e}"),
    })?;

    let bundle = db::measured_boot::report::create_revoked_bundle(&mut txn, &report, &pcr_set)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("promotion failed promoting into revoked bundle: {e}"),
        })?;

    txn.commit().await?;
    Ok(RevokeMeasurementReportResponse {
        bundle: Some(bundle.into()),
    })
}

/// handle_show_measurement_report_for_id handles the
/// ShowMeasurementReportForId API endpoint.
pub(crate) async fn handle_show_measurement_report_for_id(
    api: &Api,
    req: ShowMeasurementReportForIdRequest,
) -> Result<ShowMeasurementReportForIdResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let result = Ok(ShowMeasurementReportForIdResponse {
        report: Some(
            db::measured_boot::report::from_id(
                &mut txn,
                req.report_id
                    .ok_or(CarbideError::MissingArgument("report_id"))?,
            )
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
            .into(),
        ),
    });
    txn.commit().await?;
    result
}

/// handle_show_measurement_reports_for_machine handles the
/// ShowMeasurementReportsForMachine API endpoint.
pub(crate) async fn handle_show_measurement_reports_for_machine(
    api: &Api,
    req: ShowMeasurementReportsForMachineRequest,
) -> Result<ShowMeasurementReportsForMachineResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let result = Ok(ShowMeasurementReportsForMachineResponse {
        reports: db::measured_boot::report::get_all_for_machine_id(
            &mut txn,
            MachineId::from_str(&req.machine_id).map_err(|_| {
                CarbideError::from(RpcDataConversionError::InvalidMachineId(req.machine_id))
            })?,
        )
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("{e}"),
        })?
        .into_iter()
        .map(|report| report.into())
        .collect(),
    });
    txn.commit().await?;
    result
}

/// handle_show_measurement_reports handles the ShowMeasurementReports
/// API endpoint.
pub(crate) async fn handle_show_measurement_reports(
    api: &Api,
    _req: ShowMeasurementReportsRequest,
) -> Result<ShowMeasurementReportsResponse, Status> {
    Ok(ShowMeasurementReportsResponse {
        reports: db::measured_boot::report::get_all(&mut api.db_reader())
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("{e}"),
            })?
            .into_iter()
            .map(|report| report.into())
            .collect(),
    })
}

/// handle_list_measurement_report handles the ListMeasurementReport
/// API endpoint.
pub(crate) async fn handle_list_measurement_report(
    api: &Api,
    req: ListMeasurementReportRequest,
) -> Result<ListMeasurementReportResponse, Status> {
    let mut txn = api.txn_begin().await?;
    let reports: Vec<MeasurementReportRecordPb> = match req.selector {
        Some(list_measurement_report_request::Selector::MachineId(machine_id)) => {
            get_measurement_report_records_for_machine_id(
                &mut txn,
                MachineId::from_str(&machine_id).map_err(|_| {
                    CarbideError::from(RpcDataConversionError::InvalidMachineId(machine_id))
                })?,
            )
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed loading report records: {e}"),
            })?
            .into_iter()
            .map(|report| report.into())
            .collect()
        }
        None => get_all_measurement_report_records(&mut txn)
            .await
            .map_err(|e| CarbideError::Internal {
                message: format!("failed loading report records: {e}"),
            })?
            .into_iter()
            .map(|report| report.into())
            .collect(),
    };
    txn.commit().await?;
    Ok(ListMeasurementReportResponse { reports })
}

/// handle_match_measurement_report handles the MatchMeasurementReport
/// API endpoint.
pub(crate) async fn handle_match_measurement_report(
    api: &Api,
    req: MatchMeasurementReportRequest,
) -> Result<MatchMeasurementReportResponse, Status> {
    let pcr_register = convert_vec(req.pcr_values);
    let mut reports = match_latest_reports(&api.database_connection, &pcr_register)
        .await
        .map_err(|e| CarbideError::Internal {
            message: format!("failure during report matching: {e}"),
        })?;

    reports.sort_by_key(|a| a.ts);

    let report_pbs: Vec<MeasurementReportRecordPb> =
        reports.iter().map(|report| report.clone().into()).collect();

    Ok(MatchMeasurementReportResponse {
        reports: report_pbs,
    })
}
