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
use ::rpc::forge as forgerpc;
use prettytable::{Table, row};

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn external_config_show(
    api_client: &ApiClient,
    config_names: Vec<String>,
    extended: bool,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    let ret = api_client
        .0
        .get_machine_validation_external_configs(config_names)
        .await?;

    if extended {
        show_external_config_show_details(ret.configs, is_json)?;
    } else {
        show_external_config_show(ret.configs, is_json)?;
    }
    Ok(())
}

fn show_external_config_show_details(
    configs: Vec<forgerpc::MachineValidationExternalConfig>,
    json: bool,
) -> CarbideCliResult<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&configs)?);
    } else {
        println!("{}", convert_external_config_to_nice_format(configs)?);
    }
    Ok(())
}

fn show_external_config_show(
    configs: Vec<forgerpc::MachineValidationExternalConfig>,
    json: bool,
) -> CarbideCliResult<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&configs)?);
    } else {
        convert_external_config_to_nice_table(configs).printstd();
    }
    Ok(())
}

fn convert_external_config_to_nice_format(
    configs: Vec<forgerpc::MachineValidationExternalConfig>,
) -> CarbideCliResult<String> {
    let width = 14;
    let mut lines = String::new();
    if configs.is_empty() {
        return Ok(lines);
    }
    for config in configs {
        writeln!(
            &mut lines,
            "\t------------------------------------------------------------------------"
        )?;
        let timestamp = if config.timestamp.is_some() {
            "".to_string()
        } else {
            config.timestamp.unwrap_or_default().to_string()
        };
        let config_string = String::from_utf8(config.config)
            .map_err(|e| CarbideCliError::GenericError(e.to_string()))?;

        let details = vec![
            ("Name", config.name),
            ("Description", config.description.unwrap_or_default()),
            ("Version", config.version),
            ("TimeStamp", timestamp),
            ("Config", config_string),
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

fn convert_external_config_to_nice_table(
    configs: Vec<forgerpc::MachineValidationExternalConfig>,
) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Name", "Description", "Version", "Timestamp"]);

    for config in configs {
        table.add_row(row![
            config.name,
            config.description.unwrap_or_default(),
            config.version,
            config.timestamp.unwrap_or_default(),
        ]);
    }

    table.into()
}

pub(super) async fn external_config_add_update(
    api_client: &ApiClient,
    config_name: String,
    file_name: String,
    description: String,
) -> CarbideCliResult<()> {
    // Read the file data from disk
    let file_data = std::fs::read(&file_name)?;
    api_client
        .add_update_machine_validation_external_config(config_name, description, file_data)
        .await?;
    Ok(())
}

pub(super) async fn remove_external_config(
    api_client: &ApiClient,
    name: String,
) -> CarbideCliResult<()> {
    api_client
        .0
        .remove_machine_validation_external_config(name)
        .await?;
    Ok(())
}
