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
use carbide_uuid::nvlink::NvLinkLogicalPartitionId;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn handle_show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if args.id.is_empty() {
        show_all_logical_partitions(is_json, api_client, page_size, args.name).await?;
        return Ok(());
    }
    show_logical_partition_details(args.id, is_json, api_client).await?;
    Ok(())
}

async fn show_all_logical_partitions(
    json: bool,
    api_client: &ApiClient,
    page_size: usize,
    name: Option<String>,
) -> CarbideCliResult<()> {
    let all_logical_partitions = match api_client.get_all_logical_partitions(name, page_size).await
    {
        Ok(all_logical_partition_ids) => all_logical_partition_ids,
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all_logical_partitions)?);
    } else {
        convert_partitions_to_nice_table(all_logical_partitions).printstd();
    }
    Ok(())
}

async fn show_logical_partition_details(
    id: String,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let partition_id: NvLinkLogicalPartitionId = uuid::Uuid::parse_str(&id)
        .map_err(|_| CarbideCliError::GenericError("UUID Conversion failed.".to_string()))?
        .into();
    let logical_partition = api_client.get_one_logical_partition(partition_id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&logical_partition)?);
    } else {
        println!(
            "{}",
            convert_partition_to_nice_format(logical_partition).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_partitions_to_nice_table(
    partitions: forgerpc::NvLinkLogicalPartitionList,
) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Id", "State",]);

    for partition in partitions.partitions {
        table.add_row(row![
            partition.id.unwrap_or_default(),
            forgerpc::TenantState::try_from(partition.status.unwrap_or_default().state,)
                .unwrap_or_default()
                .as_str_name()
                .to_string()
        ]);
    }

    table.into()
}

fn convert_partition_to_nice_format(
    partition: forgerpc::NvLinkLogicalPartition,
) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let _status = partition.status.unwrap_or_default();
    let data = vec![
        (
            "ID",
            partition
                .id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        (
            "NAME",
            partition
                .config
                .and_then(|c| c.metadata)
                .map(|m| m.name)
                .unwrap_or_default(),
        ),
        (
            "STATUS",
            forgerpc::TenantState::try_from(partition.status.unwrap_or_default().state)
                .unwrap_or_default()
                .as_str_name()
                .to_string(),
        ),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    Ok(lines)
}
