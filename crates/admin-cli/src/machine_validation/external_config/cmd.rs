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
use eyre::WrapErr;
use prettytable::{Table, row};
use tokio::io::AsyncWrite;

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

pub(super) async fn external_config_show(
    api_client: &ApiClient,
    config_names: Vec<String>,
    extended: bool,
    output_format: OutputFormat,
    output: &mut (impl AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    let ret = api_client
        .0
        .get_machine_validation_external_configs(config_names)
        .await?;

    if extended {
        show_external_config_show_details(ret.configs, is_json, output).await
    } else {
        show_external_config_show(ret.configs, is_json, output).await
    }
}

async fn show_external_config_show_details(
    configs: Vec<forgerpc::MachineValidationExternalConfig>,
    json: bool,
    output: &mut (impl AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    if json {
        let configs_json = serde_json::to_string_pretty(&configs).wrap_err(
            "while attempting to serialize machine validation external configs as JSON",
        )?;
        async_writeln!(output, "{configs_json}")
            .wrap_err("while attempting to write machine validation external configs as JSON")?;
    } else {
        let details = convert_external_config_to_nice_format(configs)
            .wrap_err("while attempting to format machine validation external config details")?;
        async_writeln!(output, "{details}")
            .wrap_err("while attempting to write machine validation external config details")?;
    }
    Ok(())
}

async fn show_external_config_show(
    configs: Vec<forgerpc::MachineValidationExternalConfig>,
    json: bool,
    output: &mut (impl AsyncWrite + Unpin),
) -> CarbideCliResult<()> {
    if json {
        let configs_json = serde_json::to_string_pretty(&configs).wrap_err(
            "while attempting to serialize machine validation external configs as JSON",
        )?;
        async_writeln!(output, "{configs_json}")
            .wrap_err("while attempting to write machine validation external configs as JSON")?;
    } else {
        async_write!(output, "{}", convert_external_config_to_nice_table(configs))
            .wrap_err("while attempting to write machine validation external config table")?;
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
        let timestamp = config
            .timestamp
            .map(|timestamp| timestamp.to_string())
            .unwrap_or_default();
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
            config
                .timestamp
                .map(|timestamp| timestamp.to_string())
                .unwrap_or_default(),
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

#[cfg(test)]
mod tests {
    use super::{forgerpc, show_external_config_show};
    use crate::errors::CarbideCliError;

    #[tokio::test]
    async fn table_output_failure_preserves_context_and_io_cause() {
        let configs = vec![forgerpc::MachineValidationExternalConfig {
            name: "validation".to_string(),
            description: None,
            config: b"enabled = true".to_vec(),
            version: "v1".to_string(),
            timestamp: None,
        }];
        // Closing the reader fails the output write after valid config data renders.
        let (mut writer, reader) = tokio::io::duplex(64);
        drop(reader);

        let error = show_external_config_show(configs, false, &mut writer)
            .await
            .expect_err("closed output must fail");
        let CarbideCliError::EyreReport(report) = error else {
            panic!("expected an EyreReport");
        };
        assert_eq!(
            report.to_string(),
            "while attempting to write machine validation external config table"
        );
        let io_error = report
            .chain()
            .find_map(|cause| cause.downcast_ref::<std::io::Error>())
            .expect("I/O source remains in the error chain");
        assert_eq!(io_error.kind(), std::io::ErrorKind::BrokenPipe);
    }
}
