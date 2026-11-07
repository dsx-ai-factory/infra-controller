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
//! `measurement site` subcommand dispatcher + backing functions.

use std::fs::File;
use std::io::BufReader;

use ::rpc::measured_boot::FromGrpcOpt;
use ::rpc::protos::measured_boot::ImportSiteMeasurementsRequest;
use measured_boot::records::{MeasurementApprovedMachineRecord, MeasurementApprovedProfileRecord};
use measured_boot::site::{ImportResult, SiteModel};
use measured_boot::{ToTable, set_summary};
use serde::Serialize;

use crate::attestation::measured_boot::site::args::{
    ApproveMachine, ApproveProfile, Export, Import, ListMachines, ListProfiles,
    RemoveMachineByApprovalId, RemoveMachineByMachineId, RemoveProfileByApprovalId,
    RemoveProfileByProfileId,
};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

impl Run for Import {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            import(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for Export {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let dest = match &self.path {
            Some(p) => crate::Destination::Path(p.clone()),
            None => crate::Destination::Stdout(),
        };
        crate::cli_output(
            export(&ctx.api_client, self).await?,
            &ctx.config.format,
            dest,
        )
    }
}

impl Run for ApproveMachine {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            approve_machine(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListMachines {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_machines(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for RemoveMachineByApprovalId {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            remove_machine_by_approval_id(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for RemoveMachineByMachineId {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            remove_machine_by_machine_id(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ApproveProfile {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            approve_profile(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for ListProfiles {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            list_profiles(&ctx.api_client).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for RemoveProfileByApprovalId {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            remove_profile_by_approval_id(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

impl Run for RemoveProfileByProfileId {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        crate::cli_output(
            remove_profile_by_profile_id(&ctx.api_client, self).await?,
            &ctx.config.format,
            crate::Destination::Stdout(),
        )
    }
}

/// Import imports a serialized SiteModel back into the database.
async fn import(grpc_conn: &ApiClient, import: Import) -> CarbideCliResult<ImportResult> {
    // Prepare.
    let reader = BufReader::new(File::open(import.path)?);
    let site_model: SiteModel = serde_json::from_reader(reader)?;

    // Request.
    let request = ImportSiteMeasurementsRequest {
        model: Some(site_model.into()),
    };

    // Response + process and return.
    Ok(ImportResult::from(
        &grpc_conn.0.import_site_measurements(request).await?,
    ))
}

/// Export grabs all of the data needed to build a SiteModel.
/// Summary is explicitly set to false so all data is serialized.
async fn export(grpc_conn: &ApiClient, _export: Export) -> CarbideCliResult<SiteModel> {
    // Prepare.
    // Force != summarized output, so all keys
    // accompany the serialized data.
    set_summary(false);

    let response = grpc_conn.0.export_site_measurements().await?;

    SiteModel::from_grpc_opt(response.model)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// approve_machine is used to approve a trusted machine by machine ID.
async fn approve_machine(
    grpc_conn: &ApiClient,
    approve: ApproveMachine,
) -> CarbideCliResult<MeasurementApprovedMachineRecord> {
    let response = grpc_conn.0.add_measurement_trusted_machine(approve).await?;

    MeasurementApprovedMachineRecord::from_grpc_opt(response.approval_record)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// remove_machine_by_approval_id removes a trusted machine approval
/// by its approval ID.
async fn remove_machine_by_approval_id(
    grpc_conn: &ApiClient,
    by_approval_id: RemoveMachineByApprovalId,
) -> CarbideCliResult<MeasurementApprovedMachineRecord> {
    let response = grpc_conn
        .0
        .remove_measurement_trusted_machine(by_approval_id)
        .await?;

    MeasurementApprovedMachineRecord::from_grpc_opt(response.approval_record)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// remove_machine_by_machine_id removes a trusted machine approval
/// by its machine ID.
async fn remove_machine_by_machine_id(
    grpc_conn: &ApiClient,
    by_machine_id: RemoveMachineByMachineId,
) -> CarbideCliResult<MeasurementApprovedMachineRecord> {
    let response = grpc_conn
        .0
        .remove_measurement_trusted_machine(by_machine_id)
        .await?;

    MeasurementApprovedMachineRecord::from_grpc_opt(response.approval_record)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// list_machines lists all trusted machine approvals.
async fn list_machines(
    grpc_conn: &ApiClient,
) -> CarbideCliResult<MeasurementApprovedMachineRecordList> {
    Ok(MeasurementApprovedMachineRecordList(
        grpc_conn
            .0
            .list_measurement_trusted_machines()
            .await?
            .approval_records
            .into_iter()
            .map(|record| {
                MeasurementApprovedMachineRecord::try_from(record)
                    .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MeasurementApprovedMachineRecord>>>()?,
    ))
}

/// approve_profile is used to approve a trusted profile by profile ID.
async fn approve_profile(
    grpc_conn: &ApiClient,
    approve: ApproveProfile,
) -> CarbideCliResult<MeasurementApprovedProfileRecord> {
    let response = grpc_conn.0.add_measurement_trusted_profile(approve).await?;

    MeasurementApprovedProfileRecord::from_grpc_opt(response.approval_record)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// remove_profile_by_approval_id removes a trusted profile approval
/// by its approval ID.
async fn remove_profile_by_approval_id(
    grpc_conn: &ApiClient,
    by_approval_id: RemoveProfileByApprovalId,
) -> CarbideCliResult<MeasurementApprovedProfileRecord> {
    let response = grpc_conn
        .0
        .remove_measurement_trusted_profile(by_approval_id)
        .await?;

    MeasurementApprovedProfileRecord::from_grpc_opt(response.approval_record)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// remove_profile_by_machine_id removes a trusted machine approval
/// by its profile ID.
async fn remove_profile_by_profile_id(
    grpc_conn: &ApiClient,
    by_profile_id: RemoveProfileByProfileId,
) -> CarbideCliResult<MeasurementApprovedProfileRecord> {
    let response = grpc_conn
        .0
        .remove_measurement_trusted_profile(by_profile_id)
        .await?;

    MeasurementApprovedProfileRecord::from_grpc_opt(response.approval_record)
        .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
}

/// list_profiles lists all trusted profile approvals.
async fn list_profiles(
    grpc_conn: &ApiClient,
) -> CarbideCliResult<MeasurementApprovedProfileRecordList> {
    Ok(MeasurementApprovedProfileRecordList(
        grpc_conn
            .0
            .list_measurement_trusted_profiles()
            .await?
            .approval_records
            .into_iter()
            .map(|record| {
                MeasurementApprovedProfileRecord::try_from(record)
                    .map_err(|e| crate::CarbideCliError::GenericError(e.to_string()))
            })
            .collect::<CarbideCliResult<Vec<MeasurementApprovedProfileRecord>>>()?,
    ))
}

/// MeasurementApprovedMachineRecordList just implements a newtype
/// pattern for a Vec<MeasurementApprovedMachineRecord> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementApprovedMachineRecordList(Vec<MeasurementApprovedMachineRecord>);

impl ToTable for MeasurementApprovedMachineRecordList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row![
            "approval_id",
            "machine_id",
            "approval_type",
            "ts",
            "comments",
        ]);
        for rec in self.0 {
            let pcr_registers: String = match rec.pcr_registers {
                Some(pcr_registers) => pcr_registers,
                None => "".to_string(),
            };
            let comments: String = match rec.comments {
                Some(comments) => comments,
                None => "".to_string(),
            };
            table.add_row(prettytable::row![
                rec.approval_id,
                rec.machine_id,
                rec.approval_type,
                rec.ts,
                pcr_registers,
                comments,
            ]);
        }
        Ok(table.to_string())
    }
}

/// MeasurementApprovedProfileRecordList just implements a newtype
/// pattern for a Vec<MeasurementApprovedProfileRecord> so the ToTable
/// trait can be leveraged (since we don't define Vec).
#[derive(Serialize)]
struct MeasurementApprovedProfileRecordList(Vec<MeasurementApprovedProfileRecord>);

impl ToTable for MeasurementApprovedProfileRecordList {
    fn into_table(self) -> eyre::Result<String> {
        let mut table = prettytable::Table::new();
        table.add_row(prettytable::row![
            "approval_id",
            "profile_id",
            "approval_type",
            "ts",
            "comments",
        ]);
        for rec in self.0 {
            let pcr_registers: String = match rec.pcr_registers {
                Some(pcr_registers) => pcr_registers,
                None => "".to_string(),
            };
            let comments: String = match rec.comments {
                Some(comments) => comments,
                None => "".to_string(),
            };
            table.add_row(prettytable::row![
                rec.approval_id,
                rec.profile_id,
                rec.approval_type,
                rec.ts,
                pcr_registers,
                comments,
            ]);
        }
        Ok(table.to_string())
    }
}
