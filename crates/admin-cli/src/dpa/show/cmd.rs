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
use ::rpc::forge::{self as forgerpc};
use carbide_uuid::dpa_interface::DpaInterfaceId;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(crate) async fn show(
    args: &Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if let Some(id) = args.id {
        show_dpa_details(id, is_json, api_client).await?
    } else {
        show_dpas(is_json, api_client, page_size).await?
    }
    Ok(())
}

// Show a table of all the DPA interfaces in the system
async fn show_dpas(json: bool, api_client: &ApiClient, page_size: usize) -> CarbideCliResult<()> {
    let all_dpas = api_client.get_all_dpas(page_size).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&all_dpas)?);
    } else {
        convert_dpas_to_nice_table(all_dpas).printstd();
    }
    Ok(())
}

// Show detailed information about the DPA interface specified by id
async fn show_dpa_details(
    dpa_id: DpaInterfaceId,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let dpas = api_client.get_one_dpa(dpa_id).await?;

    let dpa = match dpas.interfaces.len() {
        1 => &dpas.interfaces[0],
        _ => return Err(CarbideCliError::GenericError("Unknown DPA ID".to_string())),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(dpa)?);
    } else {
        println!(
            "{}",
            convert_dpa_to_nice_format(dpa).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_dpas_to_nice_table(dpas: forgerpc::DpaInterfaceList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Id", "Machine", "state", "Created",]);

    for dpa in dpas.interfaces {
        table.add_row(row![
            dpa.id.unwrap_or_default(),
            dpa.machine_id.map(|id| id.to_string()).unwrap_or_default(),
            dpa.controller_state,
            dpa.created.unwrap_or_default().to_string(),
        ]);
    }

    table.into()
}

fn convert_dpa_to_nice_format(dpa: &forgerpc::DpaInterface) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let data = vec![
        ("ID", dpa.id.map(|id| id.to_string()).unwrap_or_default()),
        (
            "MACHINE ID",
            dpa.machine_id.map(|id| id.to_string()).unwrap_or_default(),
        ),
        ("CREATED", dpa.created.unwrap_or_default().to_string()),
        ("UPDATED", dpa.updated.unwrap_or_default().to_string()),
        (
            "DELETED",
            match dpa.deleted {
                Some(ts) => ts.to_string(),
                None => "".to_string(),
            },
        ),
        ("STATE", dpa.controller_state.to_string()),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    writeln!(&mut lines, "STATE HISTORY: (Latest 5 only)")?;
    if dpa.history.is_empty() {
        writeln!(&mut lines, "\tEMPTY")?;
    } else {
        writeln!(
            &mut lines,
            "\tState          Version                      Time"
        )?;
        writeln!(
            &mut lines,
            "\t---------------------------------------------------"
        )?;
        for x in dpa.history.iter().rev().take(5).rev() {
            writeln!(
                &mut lines,
                "\t{:<15} {:25} {}",
                x.state,
                x.version,
                x.time.unwrap_or_default()
            )?;
        }
    }

    Ok(lines)
}
