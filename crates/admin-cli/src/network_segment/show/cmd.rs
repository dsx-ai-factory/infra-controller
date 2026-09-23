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
use std::str::FromStr as _;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use carbide_uuid::domain::DomainId;
use carbide_uuid::network::NetworkSegmentId;
use prettytable::{Table, row};
use serde::Deserialize;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

#[derive(Deserialize)]
struct NetworkState {
    state: String,
}

fn format_free_ip_count(legacy_count: u32, count_v2: Option<u64>, saturated: bool) -> String {
    match (count_v2, saturated) {
        (Some(_), true) => "Effectively unlimited".to_string(),
        (Some(count), false) => count.to_string(),
        (None, _) => u64::from(legacy_count).to_string(),
    }
}

pub(in crate::network_segment) async fn convert_network_to_nice_format(
    segment: forgerpc::NetworkSegment,
    history: Option<Vec<forgerpc::StateHistoryRecord>>,
    api_client: &ApiClient,
) -> CarbideCliResult<String> {
    let name = segment
        .metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_default();

    let config = segment.config.unwrap_or_default();

    let state = segment
        .status
        .and_then(|status| status.lifecycle)
        .map(|lifecycle| {
            serde_json::from_str::<NetworkState>(&lifecycle.state)
                .map(|ns| ns.state)
                .unwrap_or(lifecycle.state)
        })
        .unwrap_or_default();

    let width = 18;
    let mut lines = String::new();

    let data = vec![
        (
            "ID",
            segment.id.map(|id| id.to_string()).unwrap_or_default(),
        ),
        ("NAME", name),
        ("CREATED", segment.created.unwrap_or_default().to_string()),
        ("UPDATED", segment.updated.unwrap_or_default().to_string()),
        (
            "DELETED",
            segment
                .deleted
                .map(|x| x.to_string())
                .unwrap_or("Not Deleted".to_string()),
        ),
        ("STATE", state),
        ("VPC", config.vpc_id.unwrap_or_default().to_string()),
        (
            "DOMAIN",
            format!(
                "{}/{}",
                config.subdomain_id.unwrap_or_default(),
                get_domain_name(config.subdomain_id, api_client).await
            ),
        ),
        (
            "TYPE",
            format!(
                "{:?}",
                forgerpc::NetworkSegmentType::try_from(config.segment_type).unwrap_or_default()
            ),
        ),
        (
            "INFER SLAAC EUI-64",
            config.infer_slaac_eui64_addresses.to_string(),
        ),
    ];
    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    writeln!(&mut lines, "{:<width$}: ", "PREFIXES")?;
    let width = 15;
    if config.prefixes.is_empty() {
        writeln!(&mut lines, "\tEMPTY")?;
    } else {
        for (i, prefix) in config.prefixes.into_iter().enumerate() {
            let range = ipnet::IpNet::from_str(&prefix.prefix)
                .map(|net| format!("{} - {}", net.network(), net.broadcast()))
                .unwrap_or_else(|_| "invalid prefix".to_string());
            let data = vec![
                ("SN", i.to_string()),
                ("ID", prefix.id.unwrap_or_default().to_string()),
                ("Prefix", prefix.prefix),
                ("Range", range),
                (
                    "Gateway",
                    prefix.gateway.unwrap_or_else(|| "Unknown".to_string()),
                ),
                ("SVI IP", prefix.svi_ip.unwrap_or_default()),
                ("Reserve First", prefix.reserve_first.to_string()),
                (
                    "Free IP Count",
                    format_free_ip_count(
                        prefix.free_ip_count,
                        prefix.free_ip_count_v2,
                        prefix.free_ip_count_saturated,
                    ),
                ),
            ];

            for (key, value) in data {
                writeln!(&mut lines, "\t{key:<width$}: {value}")?;
            }
            writeln!(
                &mut lines,
                "\t------------------------------------------------------------"
            )?;
        }
    }

    if let Some(history) = history {
        writeln!(&mut lines, "STATE HISTORY: (Latest 5 only)")?;
        if history.is_empty() {
            writeln!(&mut lines, "\tEMPTY")?;
        } else {
            writeln!(
                &mut lines,
                "\tState          Version                      Time"
            )?;
            writeln!(
                &mut lines,
                "\t---------------------------------------------------"
            )?;
            for x in history.iter().rev().take(5).rev() {
                writeln!(
                    &mut lines,
                    "\t{:<15} {:25} {}",
                    serde_json::from_str::<NetworkState>(&x.state)
                        .map(|ns| ns.state)
                        .unwrap_or_else(|_| x.state.clone()),
                    x.version,
                    x.time.unwrap_or_default()
                )?;
            }
        }
    }

    Ok(lines)
}

async fn get_domain_name(domain_id: Option<DomainId>, api_client: &ApiClient) -> String {
    match domain_id {
        Some(id) => match api_client.get_domains(Some(id)).await {
            Ok(domain_list) => {
                let Some(first) = domain_list.domains.into_iter().next() else {
                    return "Not Found in db".to_string();
                };

                first.name
            }
            Err(x) => x.to_string(),
        },
        None => "NA".to_owned(),
    }
}

fn convert_network_to_nice_table(
    segments: forgerpc::NetworkSegmentList,
) -> CarbideCliResult<Box<Table>> {
    let mut table = Table::new();

    table.set_titles(row![
        "Id", "Name", "Created", "State", "Vpc ID", "MTU", "Prefixes", "Last IP", "Version",
        "Type",
    ]);

    for segment in segments.network_segments {
        let name = segment
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or_default()
            .to_string();

        let config = segment.config.unwrap_or_default();

        let lifecycle = segment
            .status
            .and_then(|status| status.lifecycle)
            .unwrap_or_default();
        let state = serde_json::from_str::<NetworkState>(&lifecycle.state)
            .map(|ns| ns.state)
            .unwrap_or_else(|_| lifecycle.state.clone());
        let version = lifecycle.version;

        let Some(first_prefix) = config.prefixes.first() else {
            continue;
        };
        let Ok(net) = ipnet::IpNet::from_str(&first_prefix.prefix) else {
            continue;
        };
        let end_ip = net.broadcast().to_string();

        table.add_row(row![
            segment.id.unwrap_or_default(),
            name,
            segment.created.unwrap_or_default(),
            state,
            config.vpc_id.unwrap_or_default(),
            config.mtu.unwrap_or(-1),
            config
                .prefixes
                .iter()
                .map(|x| x.prefix.to_string())
                .collect::<Vec<String>>()
                .join(", "),
            end_ip,
            version,
            format!(
                "{:?}",
                forgerpc::NetworkSegmentType::try_from(config.segment_type).unwrap_or_default()
            ),
        ]);
    }

    Ok(table.into())
}

async fn show_all_segments(
    json: bool,
    api_client: &ApiClient,
    tenant_org_id: Option<String>,
    name: Option<String>,
    page_size: usize,
) -> CarbideCliResult<()> {
    let all_segments = match api_client
        .get_all_segments(tenant_org_id, name, page_size)
        .await
    {
        Ok(all_segment_ids) => all_segment_ids,
        Err(e) => return Err(e),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&all_segments)?);
    } else {
        convert_network_to_nice_table(all_segments)?.printstd();
    }
    Ok(())
}

async fn show_network_information(
    segment_id: NetworkSegmentId,
    json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let segment = match api_client.get_one_segment(segment_id).await {
        Ok(instances) => instances,
        Err(e) => return Err(e),
    };

    let Some(segment) = segment.network_segments.into_iter().next() else {
        return Err(CarbideCliError::SegmentNotFound);
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&segment)?);
    } else {
        let history = api_client.get_segment_state_history(segment_id).await?;
        println!(
            "{}",
            convert_network_to_nice_format(segment, Some(history), api_client).await?
        );
    }
    Ok(())
}

pub(crate) async fn handle_show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if let Some(network) = args.network {
        show_network_information(network, is_json, api_client).await?;
    } else {
        show_all_segments(
            is_json,
            api_client,
            args.tenant_org_id,
            args.name,
            page_size,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ::rpc::forge as forgerpc;
    use carbide_test_support::{Check, check_values};

    use super::{convert_network_to_nice_table, format_free_ip_count};

    fn prefix(cidr: &str) -> forgerpc::NetworkPrefix {
        forgerpc::NetworkPrefix {
            prefix: cidr.to_string(),
            id: None,
            gateway: None,
            reserve_first: 0,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }
    }

    #[test]
    fn free_ip_count_display_marks_saturated_values() {
        struct FreeIpCountRow {
            legacy: u32,
            v2: Option<u64>,
            saturated: bool,
        }

        check_values(
            [
                Check {
                    scenario: "wide count",
                    input: FreeIpCountRow {
                        legacy: 42,
                        v2: Some(1u64 << 32),
                        saturated: false,
                    },
                    expect: (1u64 << 32).to_string(),
                },
                Check {
                    scenario: "saturated",
                    input: FreeIpCountRow {
                        legacy: u32::MAX,
                        v2: Some(u64::MAX),
                        saturated: true,
                    },
                    expect: "Effectively unlimited".to_string(),
                },
                Check {
                    scenario: "legacy server fallback",
                    input: FreeIpCountRow {
                        legacy: 42,
                        v2: None,
                        saturated: false,
                    },
                    expect: "42".to_string(),
                },
                Check {
                    scenario: "legacy fallback ignores an unknown saturation flag",
                    input: FreeIpCountRow {
                        legacy: 42,
                        v2: None,
                        saturated: true,
                    },
                    expect: "42".to_string(),
                },
            ],
            |row| format_free_ip_count(row.legacy, row.v2, row.saturated),
        );
    }

    fn segment(name: &str, vpc_uuid: &str, cidr: &str, state: &str) -> forgerpc::NetworkSegment {
        forgerpc::NetworkSegment {
            id: None,
            config: Some(forgerpc::NetworkSegmentConfig {
                vpc_id: Some(vpc_uuid.parse().unwrap()),
                subdomain_id: None,
                mtu: Some(9000),
                segment_type: forgerpc::NetworkSegmentType::Tenant as i32,
                prefixes: vec![prefix(cidr)],
                infer_slaac_eui64_addresses: false,
            }),
            status: Some(forgerpc::NetworkSegmentStatus {
                flags: vec![],
                lifecycle: Some(forgerpc::LifecycleStatus {
                    state: state.to_string(),
                    version: "2".to_string(),
                    state_reason: None,
                    sla: None,
                }),
                tenant_state: forgerpc::TenantState::Ready as i32,
            }),
            metadata: Some(forgerpc::Metadata {
                name: name.to_string(),
                description: String::new(),
                labels: vec![],
            }),
            created: None,
            updated: None,
            deleted: None,
        }
    }

    fn table_string(segments: Vec<forgerpc::NetworkSegment>) -> String {
        let list = forgerpc::NetworkSegmentList {
            network_segments: segments,
        };
        let table = convert_network_to_nice_table(list).expect("table build failed");
        format!("{table}")
    }

    #[test]
    fn table_uses_structured_fields() {
        let out = table_string(vec![segment(
            "seg-new",
            "00000000-0000-0000-0000-000000000001",
            "10.0.0.0/24",
            "Ready",
        )]);
        assert!(out.contains("seg-new"), "name from metadata missing: {out}");
        assert!(out.contains("Ready"), "state from lifecycle missing: {out}");
        assert!(
            out.contains("00000000-0000-0000-0000-000000000001"),
            "vpc_id from config missing: {out}"
        );
    }
}
