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

//!
//! `measurement report` subcommand dispatcher + backing functions.

use ::rpc::measured_boot::FromGrpcOpt;
use ::rpc::protos::measured_boot::ListMeasurementReportRequest;
use measured_boot::ToTable;
use measured_boot::bundle::MeasurementBundle;
use measured_boot::records::MeasurementReportRecord;
use measured_boot::report::MeasurementReport;
use serde::Serialize;

use crate::attestation::measured_boot::report::args::{
    Create, Delete, ListAll, ListMachines, Match, Promote, Revoke, ShowForAll, ShowForId,
    ShowForMachine,
};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

impl Run for Create {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            create_for_id(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Delete {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            delete(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Promote {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            promote(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Revoke {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            revoke(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ShowForId {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            show_for_id(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ShowForMachine {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            show_for_machine(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ShowForAll {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            show_all(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListAll {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_all(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListMachines {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_machines(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Match {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            match_values(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

/// create_for_id creates a new measurement report.
async fn create_for_id(
    grpc_conn: &ApiClient,
    create: Create,
) -> CarbideCliResult<MeasurementReport> {
    let response = grpc_conn.0.create_measurement_report(create).await?;

    MeasurementReport::from_grpc_opt(response.report)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// delete deletes a measurement report with the provided ID.
async fn delete(grpc_conn: &ApiClient, delete: Delete) -> CarbideCliResult<MeasurementReport> {
    let response = grpc_conn.0.delete_measurement_report(delete).await?;

    MeasurementReport::from_grpc_opt(response.report)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// promote promotes a report to an active bundle.
///
/// `report promote <report-id> [pcr-selector]`
pub(in crate::attestation::measured_boot) async fn promote(
    grpc_conn: &ApiClient,
    promote: Promote,
) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn.0.promote_measurement_report(promote).await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// revoke "promotes" a journal entry into a revoked bundle,
/// which is a way of being able to say "any journals that come in
/// matching this should be marked as rejected.
///
/// `journal revoke <journal-id> [pcr-selector]`
async fn revoke(grpc_conn: &ApiClient, revoke: Revoke) -> CarbideCliResult<MeasurementBundle> {
    let response = grpc_conn.0.revoke_measurement_report(revoke).await?;

    MeasurementBundle::from_grpc_opt(response.bundle)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// show_for_id dumps all info about a report for the given ID.
async fn show_for_id(
    grpc_conn: &ApiClient,
    show_for_id: ShowForId,
) -> CarbideCliResult<MeasurementReport> {
    let response = grpc_conn
        .0
        .show_measurement_report_for_id(show_for_id)
        .await?;

    MeasurementReport::from_grpc_opt(response.report)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// show_for_machine dumps reports for a given machine.
async fn show_for_machine(
    grpc_conn: &ApiClient,
    show_for_machine: ShowForMachine,
) -> CarbideCliResult<MeasurementReportList> {
    Ok(MeasurementReportList(
        grpc_conn
            .0
            .show_measurement_reports_for_machine(show_for_machine)
            .await?
            .reports
            .into_iter()
            .map(|report| {
                MeasurementReport::try_from(report)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementReport>>>()?,
    ))
}

/// show_all dumps all info about all reports.
async fn show_all(grpc_conn: &ApiClient) -> CarbideCliResult<MeasurementReportList> {
    Ok(MeasurementReportList(
        grpc_conn
            .0
            .show_measurement_reports()
            .await?
            .reports
            .into_iter()
            .map(|report| {
                MeasurementReport::try_from(report)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementReport>>>()?,
    ))
}

/// list lists all bundle ids.
async fn list_all(grpc_conn: &ApiClient) -> CarbideCliResult<MeasurementReportRecordList> {
    // Request.
    let request = ListMeasurementReportRequest { selector: None };

    // Response.
    Ok(MeasurementReportRecordList(
        grpc_conn
            .0
            .list_measurement_report(request)
            .await?
            .reports
            .into_iter()
            .map(|report| {
                MeasurementReportRecord::try_from(report)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementReportRecord>>>()?,
    ))
}

/// list_machines lists all reports for the given machine ID.
async fn list_machines(
    grpc_conn: &ApiClient,
    list_machines: ListMachines,
) -> CarbideCliResult<MeasurementReportRecordList> {
    Ok(MeasurementReportRecordList(
        grpc_conn
            .0
            .list_measurement_report(list_machines)
            .await?
            .reports
            .into_iter()
            .map(|report| {
                MeasurementReportRecord::try_from(report)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementReportRecord>>>()?,
    ))
}

/// match_values matches all reports with the provided PCR values.
///
/// `report match <pcr_register:val>,...`
async fn match_values(
    grpc_conn: &ApiClient,
    match_args: Match,
) -> CarbideCliResult<MeasurementReportRecordList> {
    Ok(MeasurementReportRecordList(
        grpc_conn
            .0
            .match_measurement_report(match_args)
            .await?
            .reports
            .into_iter()
            .map(|report| {
                MeasurementReportRecord::try_from(report)
                    .map_err(|e| CarbideCliError::GenericError(format!("conversion failed: {e}")))
            })
            .collect::<CarbideCliResult<Vec<MeasurementReportRecord>>>()?,
    ))
}

/// MeasurementReportRecordList just implements a newtype pattern
/// for a Vec<MeasurementReportRecord> so the ToTable trait can
/// be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementReportRecordList(Vec<MeasurementReportRecord>);

impl ToTable for MeasurementReportRecordList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["report_id", "machine_id", "created_ts"]);
        for report in self.0.iter() {
            table.add_row(prettytable::row![
                report.report_id,
                report.machine_id,
                report.ts
            ]);
        }
        Ok(table.to_string())
    }
}

/// MeasurementReportList just implements a newtype
/// pattern for a Vec<MeasurementReport> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementReportList(Vec<MeasurementReport>);

// When `report show` gets called (for all entries), and the output format
// is the default table view, this gets used to print a pretty table.
impl ToTable for MeasurementReportList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row!["report_id", "details", "values"]);
        for report in self.0.iter() {
            let mut details_table = prettytable::Table::new();
            details_table.add_row(prettytable::row!["report_id", report.report_id]);
            details_table.add_row(prettytable::row!["machine_id", report.machine_id]);
            details_table.add_row(prettytable::row!["created_ts", report.ts]);
            let mut values_table = prettytable::Table::new();
            values_table.add_row(prettytable::row!["pcr_register", "value"]);
            for value_record in report.values.iter() {
                values_table.add_row(prettytable::row![
                    value_record.pcr_register,
                    value_record.sha_any
                ]);
            }
            table.add_row(prettytable::row![
                report.report_id,
                details_table,
                values_table
            ]);
        }
        Ok(table.to_string())
    }
}
