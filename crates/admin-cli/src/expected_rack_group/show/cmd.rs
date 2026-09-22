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

use color_eyre::eyre::WrapErr;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{ExpectedRackGroupList, ExpectedRackGroupRequest};

use super::Args;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::{async_write, async_write_table_as_csv};

pub(super) async fn show(args: Args, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
    let one = args.rack_group_id.is_some();
    let groups = if let Some(id) = args.rack_group_id {
        let group = ctx
            .api_client
            .0
            .get_expected_rack_group(ExpectedRackGroupRequest {
                rack_group_id: id.to_string(),
            })
            .await
            .wrap_err("getting expected rack group")?;
        ExpectedRackGroupList {
            expected_rack_groups: vec![group],
        }
    } else {
        let ids = ctx
            .api_client
            .0
            .find_expected_rack_group_ids()
            .await
            .wrap_err("listing expected rack group IDs")?;
        let page_size = ctx
            .api_client
            .effective_chunk_size(ctx.config.page_size.max(1))
            .await?;
        let mut result = ExpectedRackGroupList::default();
        for ids in ids.rack_group_ids.chunks(page_size) {
            let page = ctx
                .api_client
                .0
                .find_expected_rack_groups_by_ids(rpc::forge::ExpectedRackGroupsByIdsRequest {
                    rack_group_ids: ids.to_vec(),
                })
                .await
                .wrap_err("loading expected rack group batch")?;
            result
                .expected_rack_groups
                .extend(page.expected_rack_groups);
        }
        result
    };
    match ctx.config.format {
        OutputFormat::Json => {
            let json = if one {
                serde_json::to_string_pretty(&groups.expected_rack_groups[0])
            } else {
                serde_json::to_string_pretty(&groups)
            }
            .wrap_err("serializing expected rack groups")?;
            async_write!(ctx.output_file, "{json}\n").wrap_err("writing expected rack groups")?;
            return Ok(());
        }
        OutputFormat::Yaml => {
            let yaml = if one {
                serde_yaml::to_string(&groups.expected_rack_groups[0])
            } else {
                serde_yaml::to_string(&groups)
            }
            .wrap_err("serializing expected rack groups as YAML")?;
            async_write!(ctx.output_file, "{yaml}").wrap_err("writing expected rack groups")?;
            return Ok(());
        }
        OutputFormat::AsciiTable | OutputFormat::Csv => {}
    }
    let mut table = Table::new();
    table.set_titles(row![
        "Rack Group ID",
        "Topology",
        "Rack IDs",
        "Members",
        "Name",
        "Description",
        "Labels"
    ]);
    for group in groups.expected_rack_groups {
        let members = group
            .members
            .iter()
            .map(|m| format!("{} / {} / {}", m.r#type, m.manufacturer, m.id))
            .collect::<Vec<_>>()
            .join(", ");
        table.add_row(row![
            group
                .rack_group_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or_default(),
            group.topology,
            group
                .rack_ids
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            members,
            group
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            group
                .metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
            crate::metadata::fmt_labels_as_kv_pairs(group.metadata.as_ref()).join(", ")
        ]);
    }
    if ctx.config.format == OutputFormat::Csv {
        async_write_table_as_csv!(ctx.output_file, table)
            .wrap_err("writing expected rack group CSV")?;
    } else {
        async_write!(ctx.output_file, "{table}").wrap_err("writing expected rack group table")?;
    }
    Ok(())
}
