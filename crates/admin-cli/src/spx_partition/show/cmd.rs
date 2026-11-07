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
use carbide_uuid::spx::SpxPartitionId;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if let Some(id) = args.id {
        show_spx_partition_details(id, is_json, api_client).await?;
    } else {
        show_spx_partitions(
            is_json,
            api_client,
            page_size,
            args.tenant_org_id,
            args.name,
        )
        .await?;
    }
    Ok(())
}

async fn show_spx_partitions(
    json: bool,
    api_client: &ApiClient,
    page_size: usize,
    tenant_org_id: Option<String>,
    name: Option<String>,
) -> CarbideCliResult<()> {
    let all_spx_partitions = match api_client
        .get_all_spx_partitions(tenant_org_id, name, page_size)
        .await
    {
        Ok(all_spx_partition_ids) => all_spx_partition_ids,
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all_spx_partitions)?);
    } else {
        convert_spx_partitions_to_nice_table(all_spx_partitions).printstd();
    }
    Ok(())
}

async fn show_spx_partition_details(
    id: SpxPartitionId,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let spx_partitions = match api_client.get_one_spx_partition(id).await {
        Ok(instances) => instances,
        Err(e) => return Err(e),
    };

    let Some(spx_partition) = spx_partitions.spx_partitions.into_iter().next() else {
        return Err(CarbideCliError::GenericError(
            "Unknown SPX Partition ID".to_string(),
        ));
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&spx_partition)?);
    } else {
        println!(
            "{}",
            convert_spx_partition_to_nice_format(spx_partition).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_spx_partitions_to_nice_table(spx_partitions: forgerpc::SpxPartitionList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row!["Id", "Name", "TenantOrg", "Vni",]);

    for spx_partition in spx_partitions.spx_partitions {
        let metadata = spx_partition.metadata.as_ref();

        table.add_row(row![
            spx_partition.id.unwrap_or_default(),
            metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            spx_partition.tenant_organization_id,
            spx_partition.vni,
        ]);
    }

    table.into()
}

fn convert_spx_partition_to_nice_format(
    spx_partition: forgerpc::SpxPartition,
) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let tenant_organization_id = spx_partition.tenant_organization_id;
    let metadata = spx_partition.metadata;
    let labels = crate::metadata::fmt_labels_as_kv_pairs(metadata.as_ref());

    let id = spx_partition.id.map(|i| i.to_string()).unwrap_or_default();
    let labels = labels.join(", ");

    let vni = spx_partition.vni.to_string();

    let data: Vec<(&str, &str)> = vec![
        ("ID", &id),
        (
            "NAME",
            metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
        ),
        ("TENANT ORG", &tenant_organization_id),
        ("VNI", &vni),
        ("LABELS", &labels),
        (
            "DESCRIPTION",
            metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
        ),
    ];

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    Ok(lines)
}
