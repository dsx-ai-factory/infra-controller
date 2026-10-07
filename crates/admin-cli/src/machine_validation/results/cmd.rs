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

use std::borrow::Cow;
use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use prettytable::{Table, row};

use super::args::ShowResultsOptions;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn handle_results_show(
    args: ShowResultsOptions,
    output_format: OutputFormat,
    api_client: &ApiClient,
    _page_size: usize,
    extended: bool,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if extended {
        show_results_details(is_json, api_client, args).await?;
    } else {
        show_results(is_json, api_client, args).await?;
    }

    Ok(())
}

async fn show_results(
    json: bool,
    api_client: &ApiClient,
    args: ShowResultsOptions,
) -> CarbideCliResult<()> {
    let mut results = match api_client
        .get_machine_validation_results(args.machine, args.history, args.validation_id)
        .await
    {
        Ok(results) => results,
        Err(e) => return Err(e),
    };

    if let Some(test_name) = args.test_name {
        results.results.retain(|x| x.name == test_name)
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        convert_results_to_nice_table(results).printstd();
    }
    Ok(())
}

async fn show_results_details(
    json: bool,
    api_client: &ApiClient,
    args: ShowResultsOptions,
) -> CarbideCliResult<()> {
    let mut results = match api_client
        .get_machine_validation_results(args.machine, args.history, args.validation_id)
        .await
    {
        Ok(results) => results,
        Err(e) => return Err(e),
    };
    if let Some(test_name) = args.test_name {
        results.results.retain(|x| x.name == test_name)
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        println!(
            "{}",
            convert_to_nice_format(results).unwrap_or_else(|x| x.to_string())
        );
    }

    Ok(())
}

fn convert_results_to_nice_table(results: forgerpc::MachineValidationResultList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "RunID",
        "Name",
        "Context",
        "ExitCode",
        "StartTime",
        "EndTime",
    ]);

    for result in results.results {
        table.add_row(row![
            result.validation_id.unwrap_or_default(),
            result.name,
            result.context,
            result.exit_code,
            result.start_time.unwrap_or_default(),
            result.end_time.unwrap_or_default(),
        ]);
    }

    table.into()
}

fn convert_to_nice_format(
    results: forgerpc::MachineValidationResultList,
) -> CarbideCliResult<String> {
    let width = 14;
    let mut lines = String::new();
    let Some(first) = results.results.first() else {
        return Ok(lines);
    };
    let data = vec![
        (
            "ID",
            Cow::Owned(
                first
                    .validation_id
                    .as_ref()
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            ),
        ),
        ("CONTEXT", Cow::Borrowed(first.context.as_str())),
    ];
    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }
    // data.clear();
    for result in results.results {
        writeln!(
            &mut lines,
            "\t------------------------------------------------------------------------"
        )?;
        let details = vec![
            ("Name", result.name),
            ("Description", result.description),
            ("Command", result.command),
            ("Args", result.args),
            ("StdOut", result.std_out),
            ("StdErr", result.std_err),
            ("ExitCode", result.exit_code.to_string()),
            (
                "StartTime",
                result.start_time.unwrap_or_default().to_string(),
            ),
            ("EndTime", result.end_time.unwrap_or_default().to_string()),
        ];

        for (key, value) in details {
            writeln!(&mut lines, "{key:<width$}: {value}")?;
        }
        writeln!(
            &mut lines,
            "\t------------------------------------------------------------------------"
        )?;
    }
    Ok(lines)
}
