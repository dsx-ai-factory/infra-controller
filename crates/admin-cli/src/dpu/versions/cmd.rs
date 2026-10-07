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

use std::collections::HashMap;

use ::rpc::Machine;
use ::rpc::admin_cli::OutputFormat;
use carbide_uuid::machine::MachineId;
use prettytable::{Row, Table};
use serde::Serialize;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

pub(super) async fn versions(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &ApiClient,
    options: Args,
    page_size: usize,
) -> CarbideCliResult<()> {
    handle_dpu_versions(
        output_file,
        output_format,
        api_client,
        options.updates_only,
        page_size,
    )
    .await
}

#[derive(Serialize)]
struct DpuVersions {
    id: Option<MachineId>,
    dpu_type: Option<String>,
    state: String,
    firmware_version: Option<String>,
    bmc_version: Option<String>,
    bios_version: Option<String>,
    hbn_version: Option<String>,
    agent_version: Option<String>,
}

impl From<Machine> for DpuVersions {
    fn from(machine: Machine) -> Self {
        let state = match machine.state.split_once(' ') {
            Some((state, _)) => state.to_owned(),
            None => machine.state,
        };

        let dpu_type;
        let firmware_version;
        let bios_version;

        let status = machine.status.unwrap_or_default();

        if let Some(discovery_info) = status.discovery_info {
            if let Some(dmi_data) = discovery_info.dmi_data {
                dpu_type = Some(
                    dmi_data
                        .product_name
                        .split(' ')
                        .take(2)
                        .collect::<Vec<&str>>()
                        .join(" "),
                );
                bios_version = Some(dmi_data.bios_version);
            } else {
                dpu_type = None;
                bios_version = None;
            }
            firmware_version = discovery_info.dpu_info.map(|d| d.firmware_version);
        } else {
            dpu_type = None;
            firmware_version = None;
            bios_version = None;
        }

        DpuVersions {
            id: machine.id,
            dpu_type,
            state,
            firmware_version,
            bmc_version: machine.bmc_info.and_then(|bmc| bmc.firmware_version),
            bios_version,
            hbn_version: machine.inventory.and_then(|inv| {
                inv.components
                    .into_iter()
                    .find(|c| c.name.contains("hbn"))
                    .map(|c| c.version)
            }),
            agent_version: status.dpu_agent_version,
        }
    }
}

impl From<DpuVersions> for Row {
    fn from(value: DpuVersions) -> Self {
        Row::from(vec![
            value.id.unwrap_or_default().to_string(),
            value.dpu_type.unwrap_or_default(),
            value.state,
            value.firmware_version.unwrap_or_default(),
            value.bmc_version.unwrap_or_default(),
            value.bios_version.unwrap_or_default(),
            value.hbn_version.unwrap_or_default(),
            value.agent_version.unwrap_or_default(),
        ])
    }
}

fn generate_firmware_status_json(machines: Vec<Machine>) -> CarbideCliResult<String> {
    let machines: Vec<DpuVersions> = machines.into_iter().map(DpuVersions::from).collect();
    Ok(serde_json::to_string_pretty(&machines)?)
}

fn generate_firmware_status_table(machines: Vec<Machine>) -> Box<Table> {
    let mut table = Table::new();

    let headers = vec![
        "DPU Id", "DPU Type", "State", "NIC FW", "BMC", "BIOS", "HBN", "Agent",
    ];

    table.set_titles(Row::from(headers));

    machines.into_iter().map(DpuVersions::from).for_each(|f| {
        table.add_row(f.into());
    });

    Box::new(table)
}

async fn handle_dpu_versions(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &ApiClient,
    updates_only: bool,
    page_size: usize,
) -> CarbideCliResult<()> {
    let expected_versions: HashMap<String, String> = if updates_only {
        let bi = api_client.0.version(true).await?;
        let rc = bi.runtime_config.unwrap_or_default();
        rc.dpu_nic_firmware_update_version
    } else {
        HashMap::default()
    };

    let dpus = api_client
        .get_all_machines(
            rpc::forge::MachineSearchConfig {
                include_dpus: true,
                exclude_hosts: true,
                ..Default::default()
            },
            page_size,
        )
        .await?
        .machines
        .into_iter()
        .filter(|m| {
            if updates_only {
                let discovery_info = m
                    .status
                    .as_ref()
                    .and_then(|status| status.discovery_info.as_ref());
                let product_name = discovery_info
                    .and_then(|di| di.dmi_data.as_ref())
                    .map(|dmi_data| dmi_data.product_name.as_str())
                    .unwrap_or_default();

                if let Some(expected_version) = expected_versions.get(product_name) {
                    expected_version
                        != discovery_info
                            .and_then(|di| di.dpu_info.as_ref())
                            .map(|dpu| dpu.firmware_version.as_str())
                            .unwrap_or("")
                } else {
                    true
                }
            } else {
                true
            }
        })
        .collect();

    match output_format {
        OutputFormat::Json => {
            let json_output = generate_firmware_status_json(dpus)?;
            async_write!(output_file, "{}", json_output)?;
        }
        OutputFormat::Csv => {
            let result = generate_firmware_status_table(dpus);
            async_write_table_as_csv!(output_file, result)?;
        }
        _ => {
            let result = generate_firmware_status_table(dpus);
            async_write!(output_file, "{}", result)?;
        }
    }
    Ok(())
}
