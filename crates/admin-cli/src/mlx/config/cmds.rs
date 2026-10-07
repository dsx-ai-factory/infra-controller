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
use libmlx::runner::result_types::{ComparisonResult, QueryResult, SyncResult};
use prettytable::{Cell, Row, Table};
use rpc::protos::mlx_device as mlx_device_pb;

use super::super::{
    print_comparison_result_csv, print_comparison_result_table, print_sync_result_csv,
    print_sync_result_table, wrap_text,
};
use super::args::{ConfigCompareCommand, ConfigQueryCommand, ConfigSetCommand, ConfigSyncCommand};
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};

impl Run for ConfigQueryCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminConfigQueryRequest = self.into();
        let response = ctx.api_client.0.mlx_admin_config_query(request).await?;

        let query_result_pb = response
            .query_result
            .ok_or_else(|| CarbideCliError::GenericError("no query result returned".to_string()))?;

        let query_result: QueryResult = query_result_pb.try_into()?;

        match &ctx.config.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&query_result)?);
            }
            OutputFormat::Yaml => {
                println!("{}", serde_yaml::to_string(&query_result)?);
            }
            OutputFormat::AsciiTable => {
                print_query_result_table(&query_result);
            }
            OutputFormat::Csv => {
                println!("CSV not supported yet")
            }
        }

        Ok(())
    }
}

impl Run for ConfigSetCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminConfigSetRequest = self.try_into()?;
        let response = ctx.api_client.0.mlx_admin_config_set(request).await?;

        println!(
            "Successfully applied {} variable assignments.",
            response.total_applied
        );
        Ok(())
    }
}

impl Run for ConfigSyncCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminConfigSyncRequest = self.try_into()?;
        let response = ctx.api_client.0.mlx_admin_config_sync(request).await?;

        let sync_result_pb = response
            .sync_result
            .ok_or_else(|| CarbideCliError::GenericError("no sync result returned".to_string()))?;

        let sync_result: SyncResult = sync_result_pb.try_into()?;

        match &ctx.config.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&sync_result)?);
            }
            OutputFormat::Yaml => {
                println!("{}", serde_yaml::to_string(&sync_result)?);
            }
            OutputFormat::AsciiTable => {
                print_sync_result_table(&sync_result);
            }
            OutputFormat::Csv => {
                print_sync_result_csv(&sync_result);
            }
        }

        Ok(())
    }
}

impl Run for ConfigCompareCommand {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        let request: mlx_device_pb::MlxAdminConfigCompareRequest = self.try_into()?;
        let response = ctx.api_client.0.mlx_admin_config_compare(request).await?;

        let comparison_result_pb = response.comparison_result.ok_or_else(|| {
            CarbideCliError::GenericError("no comparison result returned".to_string())
        })?;

        let comparison_result: ComparisonResult = comparison_result_pb.try_into()?;

        // Output the comparison result in the requested format.
        match &ctx.config.format {
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(&comparison_result)?);
            }
            OutputFormat::Yaml => {
                println!("{}", serde_yaml::to_string(&comparison_result)?);
            }
            OutputFormat::AsciiTable => {
                print_comparison_result_table(&comparison_result);
            }
            OutputFormat::Csv => {
                print_comparison_result_csv(&comparison_result);
            }
        }

        Ok(())
    }
}

// print_query_result_table displays a QueryResult in ASCII table format.
fn print_query_result_table(result: &QueryResult) {
    let mut table = Table::new();

    // Add header row.
    table.add_row(Row::new(vec![
        Cell::new("Variable"),
        Cell::new("Current"),
        Cell::new("Next"),
        Cell::new("Default"),
        Cell::new("Modified"),
        Cell::new("Read-Only"),
    ]));

    // Add variable rows.
    for var in &result.variables {
        let modified_str = if var.modified { "Yes" } else { "No" };
        let read_only_str = if var.read_only { "Yes" } else { "No" };

        let wrapped_current = wrap_text(&var.current_value.to_string(), 60);
        let wrapped_next = wrap_text(&var.next_value.to_string(), 60);
        let wrapped_default = wrap_text(&var.default_value.to_string(), 60);

        table.add_row(Row::new(vec![
            Cell::new(&var.variable.name),
            Cell::new(&wrapped_current),
            Cell::new(&wrapped_next),
            Cell::new(&wrapped_default),
            Cell::new(modified_str),
            Cell::new(read_only_str),
        ]));
    }

    table.printstd();
}
