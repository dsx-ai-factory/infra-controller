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

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::ManagedHostNetworkConfigResponse;
use carbide_uuid::machine::{DpuMachineId, MachineId};
use prettytable::{Table, format, row};

use crate::async_write;
use crate::errors::CarbideCliResult;
use crate::machine::network::Args as NetworkCommand;
use crate::rpc::ApiClient;

pub(super) async fn network(
    api_client: &ApiClient,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    cmd: NetworkCommand,
    output_format: OutputFormat,
    page_size: usize,
) -> CarbideCliResult<()> {
    match cmd {
        NetworkCommand::Config(query) => {
            show_dpu_network_config(api_client, output_file, query.machine_id, output_format).await
        }
        NetworkCommand::Status => show_dpu_status(api_client, output_file, page_size).await,
    }
}

fn deny_prefix(config: &ManagedHostNetworkConfigResponse) -> String {
    let mut deny_prefixes = Vec::new();
    for chunk in config.deny_prefixes.chunks(5) {
        deny_prefixes.push(chunk.join(", "));
    }

    deny_prefixes.join("\n")
}

// This diagnostic view preserves the compatibility fields exposed to older agents.
#[allow(deprecated)]
async fn show_dpu_network_config(
    api_client: &ApiClient,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    dpu_id: DpuMachineId,
    output_format: OutputFormat,
) -> CarbideCliResult<()> {
    let config = api_client.0.get_managed_host_network_config(dpu_id).await?;
    match output_format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&config)?);
        }
        OutputFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&config)?);
        }
        OutputFormat::AsciiTable => {
            let mut table = Table::new();
            table.set_format(*format::consts::FORMAT_NO_LINESEP);
            table.add_row(row!["Primary DPU", config.is_primary_dpu]);
            table.add_row(row!["ASN", config.asn]);
            table.add_row(row!["VNI Device", config.vni_device]);
            table.add_row(row![
                "Config Loopback IP",
                config
                    .managed_host_config
                    .as_ref()
                    .map(|x| x.loopback_ip.as_str())
                    .unwrap_or_default()
            ]);
            table.add_row(row!["Config Version", config.managed_host_config_version]);
            table.add_row(row!["Use Admin Network", config.use_admin_network]);
            table.add_row(row![
                "Instance Config Version",
                config.instance_network_config_version
            ]);
            table.add_row(row![
                "Instance ID",
                config
                    .instance_id
                    .map(|x| x.to_string())
                    .unwrap_or_default()
            ]);

            let virt_type = ::rpc::forge::VpcVirtualizationType::try_from(
                config.network_virtualization_type.unwrap_or_default(),
            )
            .unwrap_or_default()
            .as_str_name()
            .to_string();
            table.add_row(row!["Virtualization Type", virt_type]);
            table.add_row(row!["VPC VNI", config.vpc_vni()]);
            table.add_row(row!["Internet L3 VNI", config.internet_l3_vni()]);
            table.add_row(row!["Route Servers", config.route_servers.join(", ")]);
            table.add_row(row!["Deny Prefixes", deny_prefix(&config)]);
            table.add_row(row!["Network Pinger", config.dpu_network_pinger_type()]);
            table.add_row(row!["Host Interface ID", config.host_interface_id()]);
            table.add_row(row![
                "Min Functioning Link",
                config
                    .min_dpu_functioning_links
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "Not Set".to_string())
            ]);
            async_write!(output_file, "{}", table)?;

            println!("Admin Interface:");

            if let Some(aintf) = config.admin_interface.as_ref() {
                let mut table = Table::new();
                table.set_format(*format::consts::FORMAT_NO_LINESEP);
                table.get_format().indent(4);
                table.add_row(row!["Vlan ID", aintf.vlan_id]);
                table.add_row(row!["VNI", aintf.vni]);
                table.add_row(row!["IP", aintf.ip()]);
                table.add_row(row!["Gateway", aintf.gateway()]);
                table.add_row(row!["Prefix", aintf.prefix()]);
                table.add_row(row!["Is L2 Segment", aintf.is_l2_segment]);
                table.add_row(row!["FQDN", aintf.fqdn]);
                table.add_row(row!["VPC Prefixes", aintf.vpc_prefixes.join(", ")]);
                table.add_row(row!["VPC VNI", aintf.vpc_vni]);
                table.add_row(row!["SVI IP", aintf.svi_ip()]);
                table.add_row(row!["Tenant VRF Loopback", aintf.tenant_vrf_loopback_ip()]);
                table.add_row(row!["Boot URL", aintf.booturl()]);

                async_write!(output_file, "{}", table)?;
            }

            println!("Tenant Interfaces:");
            for (idx, tintf) in config.tenant_interfaces.iter().enumerate() {
                println!("    Interface #{idx}");
                let mut table = Table::new();
                table.set_format(*format::consts::FORMAT_NO_LINESEP);
                table.get_format().indent(4);
                table.add_row(row![
                    "Function Type",
                    format!("{:?}", tintf.function_type())
                ]);
                table.add_row(row![
                    "Virtual Function ID",
                    tintf
                        .virtual_function_id
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| "NA".to_string())
                ]);
                table.add_row(row!["Vlan ID", tintf.vlan_id]);
                table.add_row(row!["VNI", tintf.vni]);
                table.add_row(row!["IP", tintf.ip()]);
                table.add_row(row!["Gateway", tintf.gateway()]);
                table.add_row(row!["Prefix", tintf.prefix()]);
                table.add_row(row!["Is L2 Segment", tintf.is_l2_segment]);
                table.add_row(row!["FQDN", tintf.fqdn]);
                table.add_row(row!["VPC Prefixes", tintf.vpc_prefixes.join(", ")]);
                table.add_row(row![
                    "VPC Peer Prefixes",
                    tintf.vpc_peer_prefixes.join(", ")
                ]);
                table.add_row(row![
                    "VPC Peer VNIs",
                    tintf
                        .vpc_peer_vnis
                        .iter()
                        .map(|vni| vni.to_string())
                        .collect::<Vec<String>>()
                        .join(", ")
                ]);
                table.add_row(row!["VPC VNI", tintf.vpc_vni]);
                table.add_row(row!["SVI IP", tintf.svi_ip()]);
                table.add_row(row!["Tenant VRF Loopback", tintf.tenant_vrf_loopback_ip()]);
                table.add_row(row!["Boot URL", tintf.booturl()]);

                async_write!(output_file, "{}", table)?;
            }
        }
        _ => {
            todo!()
        }
    }

    Ok(())
}

#[allow(deprecated)]
pub(crate) async fn show_dpu_status(
    api_client: &ApiClient,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    page_size: usize,
) -> CarbideCliResult<()> {
    let all_status = api_client
        .0
        .get_all_managed_host_network_status()
        .await?
        .all;
    if all_status.is_empty() {
        println!("No reported network status");
    } else {
        let all_ids: Vec<DpuMachineId> = all_status
            .iter()
            .filter_map(|status| status.dpu_machine_id)
            .collect();
        // The Forge API rejects requests carrying more than 100 IDs, and a zero
        // page size would panic chunks(), so clamp into the valid 1..=100 range
        // and fetch the backing machines in pages instead of one call (see #2137).
        const MAX_IDS_PER_REQUEST: usize = 100;
        let page_size = page_size.clamp(1, MAX_IDS_PER_REQUEST);
        let mut dpus_by_id = HashMap::new();
        for ids in all_ids.chunks(page_size) {
            let dpus = api_client.get_machines_by_ids(ids).await?.machines;
            for dpu in dpus.into_iter() {
                if let Some(id) = dpu.id {
                    dpus_by_id.insert(id, dpu);
                }
            }
        }

        let mut table = Table::new();
        table.set_titles(row![
            "Observed at",
            "DPU machine ID",
            "Network config version",
            "Healthy?",
            "Health Probe Alerts",
            "Agent version",
        ]);
        for st in all_status.into_iter() {
            let Some(dpu_id) = st.dpu_machine_id else {
                continue;
            };
            let Some(dpu) = dpus_by_id.get(&MachineId::from(dpu_id)) else {
                continue;
            };
            let observed_at = st
                .observed_at
                .and_then(|ts| chrono::DateTime::<chrono::Utc>::try_from(ts).ok())
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S.%3f").to_string())
                .unwrap_or_default();
            let mut probe_alerts = String::new();
            if let Some(health) = &dpu.health {
                for alert in health.alerts.iter() {
                    if !probe_alerts.is_empty() {
                        probe_alerts.push('\n');
                    }
                    if let Some(target) = &alert.target {
                        probe_alerts += &format!("{} [Target: {}]", alert.id, target)
                    } else {
                        probe_alerts += &alert.id.to_string();
                    }
                }
            }
            table.add_row(row![
                observed_at,
                st.dpu_machine_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
                st.network_config_version.unwrap_or_default(),
                dpu.health
                    .as_ref()
                    .map(|health| health.alerts.is_empty().to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                probe_alerts,
                st.dpu_agent_version.unwrap_or("".to_string())
            ]);
        }
        async_write!(output_file, "{}", table)?;
    }
    Ok(())
}
