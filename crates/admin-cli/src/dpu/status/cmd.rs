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

use ::rpc::Machine;
use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::BuildInfo;
use carbide_uuid::machine::MachineId;
use prettytable::{Row, Table};
use serde::Serialize;

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

pub(super) async fn status(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    handle_dpu_status(output_file, output_format, api_client, page_size).await
}

#[derive(Serialize)]
struct DpuStatus {
    id: Option<MachineId>,
    dpu_type: Option<String>,
    state: String,
    healthy: String,
    version_status: Option<String>,
}

impl From<Machine> for DpuStatus {
    fn from(machine: Machine) -> Self {
        let state = match machine.state.split_once(' ') {
            Some((state, _)) => state.to_owned(),
            None => machine.state,
        };

        let status = machine.status.unwrap_or_default();

        let dpu_type = status
            .discovery_info
            .and_then(|di| di.dmi_data)
            .map(|dmi_data| {
                dmi_data
                    .product_name
                    .split(' ')
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ")
            });

        DpuStatus {
            id: machine.id,
            dpu_type,
            state,
            healthy: status
                .health
                .map(|health| {
                    if health.alerts.is_empty() {
                        "Yes".to_string()
                    } else {
                        let mut alerts = String::new();
                        for alert in health.alerts.iter() {
                            if !alerts.is_empty() {
                                alerts.push('\n');
                            }
                            if let Some(target) = &alert.target {
                                alerts += &format!("{} [Target: {}]", alert.id, target);
                            } else {
                                alerts += &alert.id.to_string();
                            }
                        }
                        alerts
                    }
                })
                .unwrap_or("Unknown".to_string()),
            version_status: None,
        }
    }
}

impl From<DpuStatus> for Row {
    fn from(value: DpuStatus) -> Self {
        Row::from(vec![
            value.id.unwrap_or_default().to_string(),
            value.dpu_type.unwrap_or_default(),
            value.state,
            value.healthy,
            value.version_status.unwrap_or_default(),
        ])
    }
}

fn get_dpu_version_status(build_info: &BuildInfo, machine: &Machine) -> String {
    let mut version_statuses = Vec::default();

    let Some(runtime_config) = build_info.runtime_config.as_ref() else {
        return "No runtime config".to_owned();
    };

    let expected_agent_version = &build_info.build_version;
    let status = machine.status.as_ref();
    let agent_version = status
        .and_then(|status| status.dpu_agent_version.as_deref())
        .unwrap_or_default();
    if agent_version != expected_agent_version {
        version_statuses.push("Agent update needed");
    }

    let expected_nic_versions = &runtime_config.dpu_nic_firmware_update_version;

    let discovery_info = status.and_then(|status| status.discovery_info.as_ref());

    let product_name = discovery_info
        .and_then(|di| di.dmi_data.as_ref())
        .map(|dmi_data| dmi_data.product_name.as_str())
        .unwrap_or_default();

    if let Some(expected_version) = expected_nic_versions.get(product_name)
        && expected_version
            != discovery_info
                .and_then(|di| di.dpu_info.as_ref())
                .map(|dpu| dpu.firmware_version.as_str())
                .unwrap_or_default()
    {
        version_statuses.push("NIC Firmware update needed");
    }

    /* TODO add bmc version check when available
    let expected_bmc_versions: HashMap<String, String> = HashMap::default();
    let bmc_version = machine.bmc_info.as_ref().map(|bi| bi.firmware_version.clone().unwrap_or_default());

    if let Some(bmc_version) = bmc_version {
        if let Some(expected_bmc_version) = expected_bmc_versions.get(&product_name) {
            if expected_bmc_version != &bmc_version {
                version_statuses.push("BMC Firmware update needed");
            }
        } else {
            version_statuses.push("Unknown expected BMC Firmware version");
        }
    } else {
        version_statuses.push("Unknown BMC Firmware version");
    }
    */

    if version_statuses.is_empty() {
        "Up to date".to_owned()
    } else {
        version_statuses.join("\n")
    }
}

async fn handle_dpu_status(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
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
        .machines;

    match output_format {
        OutputFormat::Json => {
            let machines: Vec<DpuStatus> = generate_dpu_status_data(api_client, dpus).await?;
            async_write!(output_file, "{}", serde_json::to_string(&machines)?)?;
        }
        OutputFormat::Csv => {
            let result = generate_dpu_status_table(api_client, dpus).await?;
            async_write_table_as_csv!(output_file, result)?;
        }
        _ => {
            let result = generate_dpu_status_table(api_client, dpus).await?;
            async_write!(output_file, "{}", result)?;
        }
    }
    Ok(())
}

async fn generate_dpu_status_data(
    api_client: &ApiClient,
    machines: Vec<Machine>,
) -> CarbideCliResult<Vec<DpuStatus>> {
    let mut dpu_status = Vec::new();
    let build_info = api_client.0.version(true).await?;
    for machine in machines {
        let version_status = get_dpu_version_status(&build_info, &machine);
        let mut status = DpuStatus::from(machine);
        status.version_status = Some(version_status);
        dpu_status.push(status);
    }

    Ok(dpu_status)
}

async fn generate_dpu_status_table(
    api_client: &ApiClient,
    machines: Vec<Machine>,
) -> CarbideCliResult<Box<Table>> {
    let mut table = Table::new();

    let headers = vec!["DPU Id", "DPU Type", "State", "Healthy", "Version Status"];

    table.set_titles(Row::from(headers));

    generate_dpu_status_data(api_client, machines)
        .await?
        .into_iter()
        .for_each(|status| {
            table.add_row(status.into());
        });

    Ok(Box::new(table))
}
