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
use carbide_uuid::infiniband::IBPartitionId;
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
        show_ib_partition_details(id, is_json, api_client).await?;
    } else {
        show_ib_partitions(
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

async fn show_ib_partitions(
    json: bool,
    api_client: &ApiClient,
    page_size: usize,
    tenant_org_id: Option<String>,
    name: Option<String>,
) -> CarbideCliResult<()> {
    let all_ib_partitions = match api_client
        .get_all_ib_partitions(tenant_org_id, name, page_size)
        .await
    {
        Ok(all_ib_partition_ids) => all_ib_partition_ids,
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all_ib_partitions)?);
    } else {
        convert_ib_partitions_to_nice_table(all_ib_partitions).printstd();
    }
    Ok(())
}

async fn show_ib_partition_details(
    id: IBPartitionId,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let ib_partitions = match api_client.get_one_ib_partition(id).await {
        Ok(instances) => instances,
        Err(e) => return Err(e),
    };

    let Some(ib_partition) = ib_partitions.ib_partitions.into_iter().next() else {
        return Err(CarbideCliError::GenericError(
            "Unknown InfiniBand Partition ID".to_string(),
        ));
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&ib_partition)?);
    } else {
        println!(
            "{}",
            convert_ib_partition_to_nice_format(ib_partition).unwrap_or_else(|x| x.to_string())
        );
    }
    Ok(())
}

fn convert_ib_partitions_to_nice_table(ib_partitions: forgerpc::IbPartitionList) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "Name",
        "TenantOrg",
        "State",
        "Pkey",
        "Labels",
        "Description",
    ]);

    for ib_partition in ib_partitions.ib_partitions {
        let metadata = ib_partition.metadata.as_ref();
        let labels = crate::metadata::fmt_labels_as_kv_pairs(metadata);

        table.add_row(row![
            ib_partition.id.unwrap_or_default(),
            metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            ib_partition
                .config
                .as_ref()
                .map(|c| c.tenant_organization_id.as_str())
                .unwrap_or_default(),
            forgerpc::TenantState::try_from(
                ib_partition
                    .status
                    .as_ref()
                    .map(|s| s.state)
                    .unwrap_or_default(),
            )
            .map(|t| t.as_str_name())
            .unwrap_or_default(),
            ib_partition
                .status
                .as_ref()
                .and_then(|s| s.pkey.as_deref())
                .unwrap_or_default(),
            labels.join(", "),
            metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
        ]);
    }

    table.into()
}

fn convert_ib_partition_to_nice_format(
    ib_partition: forgerpc::IbPartition,
) -> CarbideCliResult<String> {
    let width = 25;
    let mut lines = String::new();

    let tenant_organization_id = ib_partition
        .config
        .unwrap_or_default()
        .tenant_organization_id;
    let metadata = ib_partition.metadata;
    let labels = crate::metadata::fmt_labels_as_kv_pairs(metadata.as_ref());

    let status = ib_partition.status.unwrap_or_default();
    let state_reason = status.state_reason.unwrap_or_default();

    let id = ib_partition.id.map(|i| i.to_string()).unwrap_or_default();
    let service_level = status
        .service_level
        .map(|i| i.to_string())
        .unwrap_or_default();
    let rate = status.rate_limit.map(|i| i.to_string()).unwrap_or_default();
    let mtu = status.mtu.map(|i| i.to_string()).unwrap_or_default();
    let labels = labels.join(", ");

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
        (
            "STATE",
            forgerpc::TenantState::try_from(status.state)
                .unwrap_or_default()
                .as_str_name(),
        ),
        (
            "STATE MACHINE",
            match forgerpc::ControllerStateOutcome::try_from(state_reason.outcome)
                .unwrap_or_default()
            {
                forgerpc::ControllerStateOutcome::Transition
                | forgerpc::ControllerStateOutcome::DoNothing
                | forgerpc::ControllerStateOutcome::Todo => "OK",
                forgerpc::ControllerStateOutcome::Wait
                | forgerpc::ControllerStateOutcome::Error => {
                    state_reason.outcome_msg.as_deref().unwrap_or_default()
                }
            },
        ),
        ("PKEY", status.pkey.as_deref().unwrap_or_default()),
        ("PARTITION", status.partition.as_deref().unwrap_or_default()),
        ("SERVICE LEVEL", &service_level),
        ("RATE", &rate),
        ("MTU", &mtu),
        (
            "SHARP APPS",
            if status.enable_sharp.unwrap_or_default() {
                "YES"
            } else {
                "NO"
            },
        ),
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
