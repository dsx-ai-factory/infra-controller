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

use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::ExpectedRackRequest;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn show(
    query: &Args,
    api_client: &ApiClient,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    let req: Option<ExpectedRackRequest> = query.into();

    if let Some(req) = req {
        let expected_rack = api_client.0.get_expected_rack(req).await?;
        println!("{:#?}", expected_rack);
        return Ok(());
    }

    let expected_racks = api_client.0.get_all_expected_racks().await?;
    if output_format == OutputFormat::Json {
        println!("{}", serde_json::to_string_pretty(&expected_racks)?);
        return Ok(());
    }

    convert_and_print_into_nice_table(&expected_racks)?;

    Ok(())
}

fn convert_and_print_into_nice_table(
    expected_racks: &::rpc::forge::ExpectedRackList,
) -> CarbideCliResult<()> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "Rack ID",
        "Rack Profile",
        "Name",
        "Description",
        "Labels"
    ]);

    for expected_rack in &expected_racks.expected_racks {
        let labels = crate::metadata::fmt_labels_as_kv_pairs(expected_rack.metadata.as_ref());

        table.add_row(row![
            expected_rack
                .rack_id
                .clone()
                .map(|r| r.to_string())
                .unwrap_or_default(),
            expected_rack
                .rack_profile_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
            expected_rack
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            expected_rack
                .metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
            labels.join(", ")
        ]);
    }

    table.printstd();

    Ok(())
}
