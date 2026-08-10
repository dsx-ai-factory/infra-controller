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

use std::collections::BTreeMap;
use std::fmt::Write;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge as forgerpc;
use carbide_uuid::domain::DomainId;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use prettytable::{Cell, Row, Table};
use rpc::forge::InterfaceAssociationType;
use tracing::warn;

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(crate) async fn handle_show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;
    if let (false, Some(interface_id)) = (args.all, args.interface_id) {
        show_machine_interfaces_information(Some(interface_id), is_json, api_client).await?;
    } else {
        // TODO(chet): Remove this ~March 2024.
        // Use tracing::warn for this so its both a little more
        // noticeable, and a little more annoying/naggy. If people
        // complain, it means its working.
        if args.all && output_format == OutputFormat::AsciiTable {
            warn!(
                "redundant `--all` with basic `show` is deprecated. just do `machine-interfaces show`"
            )
        }
        show_all_machine_interfaces(is_json, args.more, api_client).await?;
    }
    Ok(())
}

async fn show_all_machine_interfaces(
    is_json: bool,
    has_more: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine_interfaces = api_client.get_all_machines_interfaces(None).await?;

    if is_json {
        println!("{}", serde_json::to_string_pretty(&machine_interfaces)?);
    } else {
        let domain_list = api_client.get_domains(None).await?;

        convert_machines_to_nice_table(has_more, machine_interfaces, domain_list).printstd();
    }
    Ok(())
}

async fn show_machine_interfaces_information(
    interface_id: Option<MachineInterfaceId>,
    is_json: bool,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine_interfaces = api_client.get_all_machines_interfaces(interface_id).await?;
    if !machine_interfaces.interfaces.is_empty() {
        if is_json {
            println!(
                "{}",
                serde_json::to_string_pretty(&machine_interfaces.interfaces.first())?
            );
        } else {
            let interface = machine_interfaces.interfaces.first().unwrap().to_owned();
            let domain_list = api_client.get_domains(interface.domain_id).await?;
            println!(
                "{}",
                convert_machine_to_nice_format(interface, domain_list)
                    .unwrap_or_else(|x| x.to_string())
            );
        }
    }
    Ok(())
}

const UNAVAILABLE_DOMAIN_NAME: &str = "NA";

fn resolve_domain_name<'a>(
    domain_id: Option<&DomainId>,
    domains: &'a BTreeMap<DomainId, String>,
) -> &'a str {
    domain_id
        .and_then(|id| domains.get(id))
        .map(String::as_str)
        .unwrap_or(UNAVAILABLE_DOMAIN_NAME)
}

fn convert_machines_to_nice_table(
    has_more: bool,
    machine_interfaces: forgerpc::InterfaceList,
    domain_list: ::rpc::protos::dns::DomainList,
) -> Box<Table> {
    let mut table = Table::new();

    let domainlist_map = domain_list
        .domains
        .into_iter()
        .map(|x| (x.id.unwrap_or_default(), x.name))
        .collect::<BTreeMap<_, _>>();
    let mut headers = vec![
        "Id",
        "MAC Address",
        "IP Address",
        "Associated Node ID",
        "Association Type",
        "Hostname",
        "Vendor",
    ];
    if has_more {
        headers.extend_from_slice(&["Domain Name"]);
    }
    table.set_titles(Row::new(
        headers.into_iter().map(Cell::new).collect::<Vec<Cell>>(),
    ));

    for machine_interface in machine_interfaces.interfaces {
        let domain_name =
            resolve_domain_name(machine_interface.domain_id.as_ref(), &domainlist_map);
        let mut row = vec![
            machine_interface.id.unwrap_or_default().to_string(),
            machine_interface.mac_address,
            machine_interface.address.join(","),
            machine_interface
                .machine_id
                .as_ref()
                .map(MachineId::to_string)
                .unwrap_or_default(),
            machine_interface.hostname,
            machine_interface.vendor.unwrap_or_default(),
        ];
        if has_more {
            row.extend_from_slice(&[domain_name.to_owned()]);
        }
        table.add_row(row.into());
    }

    table.into()
}

///Function to print the machine interface in Table format
fn convert_machine_to_nice_format(
    machine_interface: forgerpc::MachineInterface,
    domain_list: ::rpc::protos::dns::DomainList,
) -> CarbideCliResult<String> {
    let domainlist_map = domain_list
        .domains
        .into_iter()
        .map(|x| (x.id.unwrap_or_default(), x.name))
        .collect::<BTreeMap<_, _>>();
    let domain_name = resolve_domain_name(machine_interface.domain_id.as_ref(), &domainlist_map);

    let width = 13;

    let association_type = machine_interface
        .association_type
        .and_then(|v| InterfaceAssociationType::try_from(v).ok());
    let associated_node_id = match association_type {
        Some(InterfaceAssociationType::Machine) => {
            machine_interface.machine_id.unwrap_or_default().to_string()
        }
        Some(InterfaceAssociationType::Switch) => {
            machine_interface.switch_id.unwrap_or_default().to_string()
        }
        Some(InterfaceAssociationType::Powershelf) => machine_interface
            .power_shelf_id
            .unwrap_or_default()
            .to_string(),
        Some(InterfaceAssociationType::None) | None => "N/A".to_string(),
    };

    let data = vec![
        ("ID", machine_interface.id.unwrap_or_default().to_string()),
        (
            "DPU ID",
            machine_interface
                .attached_dpu_machine_id
                .as_ref()
                .map(MachineId::to_string)
                .unwrap_or_default(),
        ),
        ("Associated Node ID", associated_node_id),
        (
            "Association Type",
            association_type
                .map(|v| v.as_str_name())
                .unwrap_or_default()
                .to_string(),
        ),
        (
            "Segment ID",
            machine_interface.segment_id.unwrap_or_default().to_string(),
        ),
        (
            "Domain Id",
            machine_interface.domain_id.unwrap_or_default().to_string(),
        ),
        ("Domain Name", domain_name.to_owned()),
        ("Hostname", machine_interface.hostname),
        ("Primary", machine_interface.primary_interface.to_string()),
        ("MAC Address", machine_interface.mac_address),
        ("Addresses", machine_interface.address.join(",")),
        ("Vendor", machine_interface.vendor.unwrap_or_default()),
    ];
    let mut lines = String::new();

    for (key, value) in data {
        writeln!(&mut lines, "\t{key:<width$}: {value}")?;
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ::rpc::forge as forgerpc;
    use carbide_test_support::{Check, check_values};
    use carbide_uuid::domain::DomainId;
    use uuid::Uuid;

    use super::{
        UNAVAILABLE_DOMAIN_NAME, convert_machine_to_nice_format, convert_machines_to_nice_table,
        resolve_domain_name,
    };

    fn domain_id(value: u128) -> DomainId {
        Uuid::from_u128(value).into()
    }

    #[test]
    fn domain_name_resolution_handles_domain_id_variants() {
        let known_domain_id = domain_id(1);
        let unknown_domain_id = domain_id(2);
        let domains = BTreeMap::from([(known_domain_id, "example.test".to_string())]);

        check_values(
            [
                Check {
                    scenario: "known domain",
                    input: Some(known_domain_id),
                    expect: "example.test".to_string(),
                },
                Check {
                    scenario: "missing domain",
                    input: None,
                    expect: UNAVAILABLE_DOMAIN_NAME.to_string(),
                },
                Check {
                    scenario: "unknown domain",
                    input: Some(unknown_domain_id),
                    expect: UNAVAILABLE_DOMAIN_NAME.to_string(),
                },
            ],
            |domain_id| resolve_domain_name(domain_id.as_ref(), &domains).to_string(),
        );
    }

    #[test]
    fn formatters_render_unavailable_domain_name_for_missing_domains() {
        check_values(
            [
                Check {
                    scenario: "domainless interface",
                    input: None,
                    expect: (true, true),
                },
                Check {
                    scenario: "unknown domain",
                    input: Some(domain_id(3)),
                    expect: (true, true),
                },
            ],
            |domain_id| {
                let interface = forgerpc::MachineInterface {
                    domain_id,
                    ..Default::default()
                };

                let detail = convert_machine_to_nice_format(
                    interface.clone(),
                    ::rpc::protos::dns::DomainList::default(),
                )
                .expect("interface detail should render");
                let detail_contains_unavailable_domain = detail
                    .lines()
                    .any(|line| line.trim() == "Domain Name  : NA");

                let table = convert_machines_to_nice_table(
                    true,
                    forgerpc::InterfaceList {
                        interfaces: vec![interface],
                    },
                    ::rpc::protos::dns::DomainList::default(),
                )
                .to_string();
                let table_contains_unavailable_domain = table
                    .lines()
                    .flat_map(|line| line.split('|'))
                    .any(|cell| cell.trim() == UNAVAILABLE_DOMAIN_NAME);

                (
                    detail_contains_unavailable_domain,
                    table_contains_unavailable_domain,
                )
            },
        );
    }
}
