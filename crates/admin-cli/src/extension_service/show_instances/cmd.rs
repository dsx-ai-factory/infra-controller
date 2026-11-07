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

use ::rpc::admin_cli::output::OutputFormat;
use ::rpc::forge::InstanceDpuExtensionServiceInfo;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn handle_show_instances(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let response = api_client
        .0
        .find_instances_by_dpu_extension_service(args)
        .await?;

    if is_json {
        let instances_json: Vec<serde_json::Value> = response
            .instances
            .iter()
            .map(|i| {
                serde_json::json!({
                    "instance_id": i.instance_id,
                    "service_id": i.service_id,
                    "version": i.version,
                    "removing": i.removed,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&instances_json)?);
    } else {
        convert_instances_to_table(&response.instances).printstd();
    }

    Ok(())
}

fn convert_instances_to_table(instances: &[InstanceDpuExtensionServiceInfo]) -> Box<Table> {
    let mut table = Table::new();

    table.set_titles(row![
        "Instance ID",
        "Service ID",
        "Version",
        "Config Status",
    ]);

    for instance in instances {
        let status = if instance.removed.is_some() {
            "Removing"
        } else {
            "Active"
        };

        table.add_row(row![
            instance.instance_id,
            instance.service_id,
            instance.version,
            status,
        ]);
    }

    table.into()
}
