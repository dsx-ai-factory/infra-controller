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

use ::rpc::admin_cli::OutputFormat;
use carbide_uuid::dpu_remediations::RemediationId;
use carbide_uuid::machine::DpuMachineId;
use prettytable::{Table, row};
use rpc::forge::{
    AppliedRemediationIdList, AppliedRemediationList, FindAppliedRemediationIdsRequest,
    FindAppliedRemediationsRequest,
};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

pub(super) async fn handle_list_applied(
    args: Args,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    match (args.remediation_id, args.machine_id) {
        (Some(remediation_id), Some(machine_id)) => {
            show_applied_remediation_details(
                remediation_id,
                machine_id,
                output_format,
                output_file,
                api_client,
                page_size,
            )
            .await?;
        }
        (Some(remediation_id), None) => {
            show_machines_for_applied_remediation(
                remediation_id,
                output_format,
                output_file,
                api_client,
                page_size,
            )
            .await?;
        }
        (None, Some(machine_id)) => {
            show_applied_remediations_for_machine(
                machine_id,
                output_format,
                output_file,
                api_client,
                page_size,
            )
            .await?;
        }
        (None, None) => {
            return Err(CarbideCliError::GenericError(
                "Invalid arguments, must provide at least one of remediation_id or machine_id"
                    .to_string(),
            ));
        }
    }

    Ok(())
}

async fn show_applied_remediation_details(
    remediation_id: RemediationId,
    machine_id: DpuMachineId,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    _page_size: usize,
) -> CarbideCliResult<()> {
    let applied_remediations = api_client
        .0
        .find_applied_remediations(FindAppliedRemediationsRequest {
            remediation_id: Some(remediation_id),
            dpu_machine_id: Some(machine_id),
        })
        .await?;
    match output_format {
        OutputFormat::AsciiTable => {
            let table = convert_applied_remediations_to_nice_table(applied_remediations);
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Json => {
            async_writeln!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&applied_remediations)?
            )?;
        }
        OutputFormat::Csv | OutputFormat::Yaml => {
            unimplemented!()
        }
    }

    Ok(())
}

async fn show_machines_for_applied_remediation(
    remediation_id: RemediationId,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    _page_size: usize,
) -> CarbideCliResult<()> {
    let applied_remediation_ids = api_client
        .0
        .find_applied_remediation_ids(FindAppliedRemediationIdsRequest {
            remediation_id: Some(remediation_id),
            dpu_machine_id: None,
        })
        .await?;
    match output_format {
        OutputFormat::AsciiTable => {
            let table = show_machines_applied_for_remediation(applied_remediation_ids);
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Json => {
            async_writeln!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&applied_remediation_ids)?
            )?;
        }
        OutputFormat::Csv | OutputFormat::Yaml => {
            unimplemented!()
        }
    }

    Ok(())
}

async fn show_applied_remediations_for_machine(
    machine_id: DpuMachineId,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    _page_size: usize,
) -> CarbideCliResult<()> {
    let applied_remediation_ids = api_client
        .0
        .find_applied_remediation_ids(FindAppliedRemediationIdsRequest {
            remediation_id: None,
            dpu_machine_id: Some(machine_id),
        })
        .await?;
    match output_format {
        OutputFormat::AsciiTable => {
            let table = show_remediations_applied_for_machine(applied_remediation_ids);
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Json => {
            async_writeln!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&applied_remediation_ids)?
            )?;
        }
        OutputFormat::Csv | OutputFormat::Yaml => {
            unimplemented!()
        }
    }

    Ok(())
}

fn show_machines_applied_for_remediation(
    applied_remediation_ids: AppliedRemediationIdList,
) -> Box<Table> {
    assert_eq!(applied_remediation_ids.remediation_ids.len(), 1);
    let remediation_id = applied_remediation_ids.remediation_ids[0];
    let mut table = Box::new(Table::new());

    table.set_titles(row!["Id", "Machine Id",]);
    if applied_remediation_ids.dpu_machine_ids.is_empty() {
        table.add_row(row![remediation_id.to_string(), "None"]);
    } else {
        for machine_id in applied_remediation_ids.dpu_machine_ids.into_iter() {
            table.add_row(row![remediation_id.to_string(), machine_id.to_string(),]);
        }
    }

    table
}

fn show_remediations_applied_for_machine(
    applied_remediation_ids: AppliedRemediationIdList,
) -> Box<Table> {
    assert_eq!(applied_remediation_ids.dpu_machine_ids.len(), 1);
    let machine_id = applied_remediation_ids.dpu_machine_ids[0];
    let mut table = Box::new(Table::new());

    table.set_titles(row!["Machine Id", "Remediation Id",]);
    if applied_remediation_ids.remediation_ids.is_empty() {
        table.add_row(row![machine_id.to_string(), "None"]);
    } else {
        for remediation_id in applied_remediation_ids.remediation_ids.into_iter() {
            table.add_row(row![machine_id.to_string(), remediation_id.to_string(),]);
        }
    }

    table
}

fn convert_applied_remediations_to_nice_table(
    applied_remediations: AppliedRemediationList,
) -> Box<Table> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "Id",
        "Machine Id",
        "Applied Time",
        "Succeeded",
        "Attempt #",
        "Labels",
    ]);

    if applied_remediations.applied_remediations.is_empty() {
        table.add_row(row!["None", "None", "None", "None", "None", "None",]);
    } else {
        for applied_remediations in applied_remediations.applied_remediations.into_iter() {
            let labels =
                crate::metadata::fmt_labels_as_kv_pairs(applied_remediations.metadata.as_ref());

            table.add_row(row![
                applied_remediations
                    .remediation_id
                    .unwrap_or_default()
                    .to_string(),
                applied_remediations
                    .dpu_machine_id
                    .unwrap_or_default()
                    .to_string(),
                applied_remediations.applied_time.unwrap_or_default(),
                applied_remediations.succeeded,
                applied_remediations.attempt,
                labels.join(", ")
            ]);
        }
    }

    table
}
