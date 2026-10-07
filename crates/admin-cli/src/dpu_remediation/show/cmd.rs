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

use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use carbide_uuid::dpu_remediations::RemediationId;
use prettytable::{Table, row};
use rpc::forge::{Remediation, RemediationList};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

pub(super) async fn handle_show(
    args: Args,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    if let Some(remediation_id) = args.id {
        show_remediation_information(
            remediation_id,
            output_format,
            output_file,
            args.display_script,
            api_client,
        )
        .await
    } else {
        show_all_remediations(output_format, output_file, api_client, page_size).await
    }
}

fn convert_remediation_to_nice_format(
    remediation: Remediation,
    display_script: bool,
) -> CarbideCliResult<String> {
    let mut lines = String::new();

    let data = vec![
        ("ID", remediation.id.unwrap_or_default().to_string()),
        ("AUTHOR", remediation.script_author),
        (
            "REVIEWER",
            remediation.script_reviewed_by.unwrap_or_default(),
        ),
        (
            "CREATION_TIME",
            remediation.creation_time.unwrap_or_default().to_string(),
        ),
        ("RETRIES", remediation.retries.to_string()),
        ("ENABLED", remediation.enabled.to_string()),
    ];

    let width = 1 + data
        .iter()
        .fold(0, |accum, (key, _value)| std::cmp::max(accum, key.len()));

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    crate::metadata::write_metadata_in_nice_format(
        &mut lines,
        width,
        remediation.metadata.as_ref(),
    )?;

    if display_script {
        writeln!(
            &mut lines,
            "{:<width$}:\n***************************************BEGIN-SCRIPT********************************\n{}\n***************************************END-SCRIPT**********************************",
            "SCRIPT", remediation.script
        )?;
    }

    Ok(lines)
}

async fn show_remediation_information(
    remediation_id: RemediationId,
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    display_script: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let remediation = api_client.get_remediation(remediation_id).await?;

    match output_format {
        OutputFormat::AsciiTable => {
            let table = convert_remediation_to_nice_format(remediation, display_script)?;
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Json => {
            async_writeln!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&remediation)?
            )?;
        }
        OutputFormat::Csv | OutputFormat::Yaml => {
            unimplemented!()
        }
    }

    Ok(())
}

async fn show_all_remediations(
    output_format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let remediations = api_client.get_all_remediations(page_size).await?;
    match output_format {
        OutputFormat::AsciiTable => {
            let table = convert_remediations_to_nice_table(remediations);
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Json => {
            async_writeln!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&remediations)?
            )?;
        }
        OutputFormat::Csv | OutputFormat::Yaml => {
            unimplemented!()
        }
    }
    Ok(())
}

fn convert_remediations_to_nice_table(remediations: RemediationList) -> Box<Table> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "Id",
        "Creation Time",
        "Author",
        "Reviewer",
        "Enabled",
        "Retries",
        "Labels",
    ]);

    if remediations.remediations.is_empty() {
        table.add_row(row!["None", "None", "None", "None", "None", "None", "None"]);
    } else {
        for remediation in remediations.remediations.into_iter() {
            let labels = crate::metadata::fmt_labels_as_kv_pairs(remediation.metadata.as_ref());

            table.add_row(row![
                remediation.id.unwrap_or_default().to_string(),
                remediation.creation_time.unwrap_or_default(),
                remediation.script_author,
                remediation.script_reviewed_by.unwrap_or_default(),
                remediation.enabled,
                remediation.retries,
                labels.join(", ")
            ]);
        }
    }

    table
}
