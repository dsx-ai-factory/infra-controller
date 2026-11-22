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

/// Detailed show requests counts with `include_num_free_ips` and fetches
/// history separately. Mutation replies include neither.
enum SegmentDetails {
    CountedRead(Vec<forgerpc::StateHistoryRecord>),
    Mutation,
}

impl SegmentDetails {
    fn free_ip_count(&self, index: usize, prefix: &forgerpc::NetworkPrefix) -> String {
        match prefix.free_ip_count_v2 {
            Some(_) if prefix.free_ip_count_saturated => "Effectively unlimited".to_string(),
            Some(count) => count.to_string(),
            None if prefix.free_ip_count != 0 => prefix.free_ip_count.to_string(),
            // With accounting requested, legacy Core stored its count on the
            // first returned prefix. Other default zeros do not report a count.
            None if index == 0 && matches!(self, Self::CountedRead(_)) => "0".to_string(),
            None => "Unknown".to_string(),
        }
    }
}

/// Renders the result of create or append without treating it as a counted read.
pub(in crate::network_segment) async fn format_segment_after_mutation(
    segment: forgerpc::NetworkSegment,
    api_client: &ApiClient,
) -> CarbideCliResult<String> {
    format_segment_details(segment, SegmentDetails::Mutation, api_client).await
}

async fn format_segment_details(
    segment: forgerpc::NetworkSegment,
    details: SegmentDetails,
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
                .unwrap_or_else(|| "Not Deleted".to_string()),
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
            let free_ip_count = details.free_ip_count(i, &prefix);
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
                (
                    "DHCPv6 Link",
                    prefix.dhcpv6_link_address.unwrap_or_default(),
                ),
                ("Reserve First", prefix.reserve_first.to_string()),
                ("Free IP Count", free_ip_count),
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

    if let SegmentDetails::CountedRead(history) = details {
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
                let state = serde_json::from_str::<NetworkState>(&x.state);
                let state = state
                    .as_ref()
                    .map(|state| state.state.as_str())
                    .unwrap_or(&x.state);
                writeln!(
                    &mut lines,
                    "\t{:<15} {:25} {}",
                    state,
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
        let mut prefixes = String::new();
        for (index, prefix) in config.prefixes.iter().enumerate() {
            if index > 0 {
                prefixes.push_str(", ");
            }
            prefixes.push_str(&prefix.prefix);
        }

        table.add_row(row![
            segment.id.unwrap_or_default(),
            name,
            segment.created.unwrap_or_default(),
            state,
            config.vpc_id.unwrap_or_default(),
            config.mtu.unwrap_or(-1),
            prefixes,
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
            format_segment_details(segment, SegmentDetails::CountedRead(history), api_client)
                .await?
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
    use carbide_test_support::value_scenarios;

    use super::{SegmentDetails, convert_network_to_nice_table};

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
            dhcpv6_link_address: None,
        }
    }

    /// Free-address counts follow the fields Core supplied for this prefix,
    /// including whether an omitted legacy zero represents a measured value.
    #[test]
    fn free_ip_count_from_counted_read() {
        // Reverse the family order used by the command test: either family
        // can be the first prefix in a counted response.
        let cidrs = ["192.0.2.0/24", "2001:db8::/64"];
        let details = SegmentDetails::CountedRead(Vec::new());
        value_scenarios!(
            run = |(index, legacy, v2, saturated)| {
                let prefix = forgerpc::NetworkPrefix {
                    free_ip_count: legacy,
                    free_ip_count_v2: v2,
                    free_ip_count_saturated: saturated,
                    ..prefix(cidrs[index])
                };
                details.free_ip_count(index, &prefix)
            };
            "only the first legacy zero was counted" {
                (0, 0, None, false) => "0".to_string(),
                (1, 0, None, false) => "Unknown".to_string(),
            }
            "keep a positive legacy count wherever it was supplied" {
                (0, 42, None, false) => "42".to_string(),
                (1, 7, None, false) => "7".to_string(),
            }
            "a saturation flag without a wide count does not hide the legacy value" {
                (1, 42, None, true) => "42".to_string(),
            }
            "an explicit sibling count takes precedence over the legacy scalar" {
                (1, 0, Some(0), false) => "0".to_string(),
                (1, u32::MAX, Some(1_u64 << 32), false) => (1_u64 << 32).to_string(),
            }
            "the saturation flag distinguishes an exact maximum from a capped count" {
                (1, u32::MAX, Some(u64::MAX), false) => u64::MAX.to_string(),
                (1, u32::MAX, Some(u64::MAX), true) => "Effectively unlimited".to_string(),
            }
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
