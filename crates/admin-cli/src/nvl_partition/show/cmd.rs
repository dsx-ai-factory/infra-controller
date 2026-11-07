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
use carbide_uuid::nvlink::NvLinkPartitionId;
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
        show_nvl_partitions(
            is_json,
            api_client,
            page_size,
            args.tenant_org_id,
            args.name,
        )
        .await?;
        return Ok(());
    }
    show_nvl_partition_details(args.id, is_json, api_client).await?;
    Ok(())
}

async fn show_nvl_partitions(
    json: bool,
    api_client: &ApiClient,
    page_size: usize,
    tenant_org_id: Option<String>,
    name: Option<String>,
) -> CarbideCliResult<()> {
    let all_nvl_partitions = api_client
        .get_all_nv_link_partitions(tenant_org_id, name, page_size)
        .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&all_nvl_partitions)?);
    } else {
        convert_nvl_partitions_to_nice_table(all_nvl_partitions).printstd();
    }
    Ok(())
}

async fn show_nvl_partition_details(
    id: String,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let nvl_partition_id: NvLinkPartitionId = uuid::Uuid::parse_str(&id)
        .map_err(|_| CarbideCliError::GenericError("UUID Conversion failed.".to_string()))?
        .into();
    let nvl_partition = api_client
        .get_one_nv_link_partition(nvl_partition_id)
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&nvl_partition)?);
    } else {
        println!(
            "{}",
            convert_nvl_partition_to_nice_format(nvl_partition).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_nvl_partitions_to_nice_table(
    nvl_partitions: forgerpc::NvLinkPartitionList,
) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Id", "Name", "TenantOrg"]);

    for nvl_partition in nvl_partitions.partitions {
        table.add_row(row![
            nvl_partition.id.unwrap_or_default(),
            nvl_partition.name,
        ]);
    }

    table.into()
}

fn convert_nvl_partition_to_nice_format(
    nvl_partition: forgerpc::NvLinkPartition,
) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let data = vec![
        ("ID", nvl_partition.id.unwrap_or_default().to_string()),
        ("NAME", nvl_partition.name),
        (
            "LOGICAL PARTITION ID",
            nvl_partition
                .logical_partition_id
                .map(|logical_partition_id| logical_partition_id.to_string())
                .unwrap_or_default(),
        ),
        ("NMX-C PARTITION ID", nvl_partition.nmx_m_id),
        (
            "NVLINK DOMAIN UUID",
            nvl_partition.domain_uuid.unwrap_or_default().to_string(),
        ),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    Ok(lines)
}
