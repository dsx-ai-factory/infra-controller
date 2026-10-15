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
use std::borrow::Cow;
use std::collections::HashMap;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::site_explorer::{ExploredEndpoint, ExploredManagedHost, SiteExplorationReport};
use carbide_utils::none_if_empty::NoneIfEmpty;
use prettytable::{Cell, Row, Table, format, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

/// Build an index into the ExploredEndpoints slice for ones matching this ExploredManaged, using
/// the same lifetime as the endpoints slice.
fn get_endpoints_for_managed_host<'ep>(
    managedhost: &ExploredManagedHost,
    endpoints: &'ep [ExploredEndpoint],
) -> HashMap<&'ep str, &'ep ExploredEndpoint> {
    let mut wanted_ips = managedhost
        .dpus
        .iter()
        .map(|x| x.bmc_ip.as_str())
        .collect::<Vec<&str>>();
    wanted_ips.push(managedhost.host_bmc_ip.as_str());

    endpoints
        .iter()
        .filter_map(|x| {
            if wanted_ips.contains(&x.address.as_str()) {
                Some((x.address.as_str(), x))
            } else {
                None
            }
        })
        .collect::<HashMap<&str, &ExploredEndpoint>>()
}

fn convert_managed_host_to_nice_table(
    explored_endpoints: SiteExplorationReport,
    vendor: Option<String>,
) -> Box<Table> {
    let mut table = Table::new();

    let headers = vec![
        "Host BMC Ip",
        "Vendor",
        "DPUs (BMC IP   |                         Machine Id                           |Serial Number|   HostPFMacAddress   | oob_net0 MAC | )",
    ];

    table.set_titles(Row::new(
        headers.into_iter().map(Cell::new).collect::<Vec<Cell>>(),
    ));

    for managedhost in explored_endpoints.managed_hosts {
        let endpoints = get_endpoints_for_managed_host(&managedhost, &explored_endpoints.endpoints);
        if let Some(vendor) = &vendor
            && let Some(report) = endpoints.get(&managedhost.host_bmc_ip.as_str())
            && report
                .report
                .as_ref()
                .and_then(|x| x.vendor.as_deref())
                .unwrap_or("")
                != vendor
        {
            continue;
        }
        table.add_row(managed_host_to_row(managedhost, endpoints));
    }

    Box::new(table)
}

fn managed_host_to_row(
    value: ExploredManagedHost,
    endpoints: HashMap<&str, &ExploredEndpoint>,
) -> Row {
    let mut dpu_table = Table::new();
    dpu_table.set_format(*format::consts::FORMAT_NO_LINESEP);
    value.dpus.into_iter().for_each(|dpu| {
        let dpu_report = endpoints
            .get(&dpu.bmc_ip.as_str())
            .and_then(|x| x.report.as_ref());

        let system = dpu_report.as_ref().and_then(|x| x.systems.first());
        let oob_mac = system
            .as_ref()
            .map(|x| {
                x.ethernet_interfaces
                    .iter()
                    .find_map(|x| {
                        if x.id().contains("oob") {
                            x.mac_address.as_deref()
                        } else {
                            None
                        }
                    })
                    .unwrap_or("oob_net0 not found.")
            })
            .unwrap_or("Unknown");

        dpu_table.add_row(
            vec![
                dpu.bmc_ip.as_str(),
                dpu_report
                    .as_ref()
                    .and_then(|x| x.machine_id.as_deref())
                    .unwrap_or("Unknown"),
                system
                    .as_ref()
                    .and_then(|x| x.serial_number.as_deref())
                    .unwrap_or("Unknown"),
                dpu.host_pf_mac_address
                    .as_deref()
                    .unwrap_or("Unknown MacAddress"),
                oob_mac,
            ]
            .into(),
        );
    });

    let host_report = endpoints
        .get(&value.host_bmc_ip.as_str())
        .and_then(|x| x.report.as_ref());

    Row::new(vec![
        Cell::new(value.host_bmc_ip.as_str()),
        Cell::new(
            host_report
                .and_then(|x| x.vendor.as_deref())
                .unwrap_or("Unknown"),
        ),
        Cell::new(dpu_table.to_string().as_str()),
    ])
}

async fn get_exploration_report_for_bmc_address(
    ip: &String,
    api_client: &ApiClient,
    page_size: usize,
) -> CarbideCliResult<SiteExplorationReport> {
    // get managed host with host bmc
    let mut managed_host = api_client
        .0
        .find_explored_managed_hosts_by_ids(std::slice::from_ref(&ip.to_string()))
        .await?
        .managed_hosts;

    if managed_host.is_empty() {
        // We didn't find anything here. Lets search all managed hosts.
        // // This is costly. We have to add a api to fetch only needed info.
        let managed_hosts = api_client.get_all_explored_managed_hosts(page_size).await?;
        managed_host = managed_hosts
            .into_iter()
            .filter(|x| x.host_bmc_ip.eq(ip) || x.dpus.iter().any(|a| a.bmc_ip.eq(ip)))
            .collect();
    }

    let ips = if let Some(managed_host) = managed_host.first() {
        let mut ips = vec![managed_host.host_bmc_ip.clone()];
        ips.extend(managed_host.dpus.iter().map(|x| x.bmc_ip.clone()));
        ips
    } else {
        vec![ip.to_string()]
    };

    let endpoints = api_client.get_explored_endpoints_by_ids(&ips).await?;

    Ok(::rpc::site_explorer::SiteExplorationReport {
        endpoints: endpoints.endpoints,
        managed_hosts: managed_host,
        last_run: None,
    })
}

pub(crate) async fn show_discovered_managed_host(
    api_client: &ApiClient,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    internal_page_size: usize,
    mode: Args,
) -> CarbideCliResult<()> {
    match mode {
        Args::All => {
            let exploration_report = api_client
                .get_site_exploration_report(internal_page_size)
                .await?;

            async_write!(
                output_file,
                "{}",
                serde_json::to_string_pretty(&exploration_report)?
            )?;
            return Ok(());
        }

        Args::ManagedHost(managed_host_info) => {
            if let Some(address) = managed_host_info.address {
                // Match the API's `IpAddr::to_string()` representation.
                let address = address.to_string();
                let exploration_report = get_exploration_report_for_bmc_address(
                    &address,
                    api_client,
                    internal_page_size,
                )
                .await?;
                let Some(managed_host) = exploration_report.managed_hosts.iter().find(|x| {
                    x.host_bmc_ip == address || x.dpus.iter().any(|a| a.bmc_ip == address)
                }) else {
                    async_writeln!(output_file, "Could not find IP in discovered managed host.")?;
                    return Ok(());
                };
                if output_format == OutputFormat::Json {
                    async_writeln!(
                        output_file,
                        "{}",
                        serde_json::to_string_pretty(&managed_host)?
                    )?;
                    return Ok(());
                }
                let endpoints =
                    get_endpoints_for_managed_host(managed_host, &exploration_report.endpoints);
                print_managed_host_info(output_file, managed_host, endpoints).await?;
            } else {
                let exploration_report = api_client
                    .get_site_exploration_report(internal_page_size)
                    .await?;
                if output_format == OutputFormat::Json {
                    async_writeln!(
                        output_file,
                        "{}",
                        serde_json::to_string_pretty(&exploration_report.managed_hosts)?
                    )?;
                    return Ok(());
                }
                let table = convert_managed_host_to_nice_table(
                    exploration_report,
                    managed_host_info.vendor,
                );
                async_write!(output_file, "{}", table)?;
            }
        }
        Args::Endpoint(endpoint_info) => {
            if let Some(address) = endpoint_info.address {
                // Match the API's `IpAddr::to_string()` representation.
                let address = address.to_string();
                let exploration_report = get_exploration_report_for_bmc_address(
                    &address,
                    api_client,
                    internal_page_size,
                )
                .await?;

                let endpoint = exploration_report
                    .endpoints
                    .into_iter()
                    .find(|x| x.address == address)
                    .ok_or_else(|| {
                        CarbideCliError::GenericError("Endpoint not found.".to_string())
                    })?;

                if output_format == OutputFormat::Json {
                    async_writeln!(output_file, "{}", serde_json::to_string_pretty(&endpoint)?)?;
                    return Ok(());
                }

                display_endpoint(output_file, endpoint).await?;
            } else {
                let exploration_report = api_client
                    .get_site_exploration_report(internal_page_size)
                    .await?;
                let mut paired_ips = vec![];
                if endpoint_info.unpairedonly {
                    for managed_host in &exploration_report.managed_hosts {
                        paired_ips.push(managed_host.host_bmc_ip.as_str());

                        for dpu in &managed_host.dpus {
                            paired_ips.push(dpu.bmc_ip.as_str());
                        }
                    }
                }

                let endpoints = filter_endpoints(
                    exploration_report.endpoints,
                    endpoint_info.erroronly,
                    endpoint_info.successonly,
                    paired_ips,
                    endpoint_info.vendor,
                );

                if output_format == OutputFormat::Json {
                    async_writeln!(output_file, "{}", serde_json::to_string_pretty(&endpoints)?)?;
                    return Ok(());
                }
                let table = convert_endpoints_to_nice_table(&endpoints);
                async_write!(output_file, "{}", table)?;
            }
        }
    }

    Ok(())
}

fn filter_endpoints(
    endpoints: Vec<ExploredEndpoint>,
    erroronly: bool,
    successonly: bool,
    paired_ips: Vec<&str>,
    vendor: Option<String>,
) -> Vec<ExploredEndpoint> {
    endpoints
        .into_iter()
        .filter(|explored_endpoint| {
            let paired_filter = if !paired_ips.is_empty() {
                !paired_ips.contains(&explored_endpoint.address.as_str())
            } else {
                true
            };

            let vendor_filter = if let Some(vendor) = &vendor {
                explored_endpoint
                    .report
                    .as_ref()
                    .and_then(|x| x.vendor.as_deref())
                    .unwrap_or_default()
                    == vendor.as_str()
            } else {
                true
            };

            vendor_filter
                && paired_filter
                && explored_endpoint
                    .report
                    .as_ref()
                    .map(|x| {
                        let has_error = x.has_last_exploration_error();
                        if erroronly {
                            has_error
                        } else if successonly {
                            !has_error
                        } else {
                            // Don't filter
                            true
                        }
                    })
                    .unwrap_or_default()
        })
        .collect::<Vec<ExploredEndpoint>>()
}

async fn print_managed_host_info(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    managed_host: &ExploredManagedHost,
    endpoints: HashMap<&str, &ExploredEndpoint>,
) -> CarbideCliResult<()> {
    let host_report = endpoints
        .get(&managed_host.host_bmc_ip.as_str())
        .and_then(|x| x.report.as_ref());

    async_writeln!(output_file, "Host BMC IP : {}", managed_host.host_bmc_ip)?;
    async_writeln!(
        output_file,
        "Vendor      : {}",
        host_report
            .and_then(|x| x.vendor.as_deref())
            .unwrap_or("Unknown")
    )?;
    for (i, x) in managed_host.dpus.iter().enumerate() {
        let dpu_report = endpoints
            .get(&x.bmc_ip.as_str())
            .and_then(|x| x.report.as_ref());
        let system = dpu_report.and_then(|x| x.systems.first());
        let oob_mac = system
            .as_ref()
            .map(|x| {
                x.ethernet_interfaces
                    .iter()
                    .find_map(|x| {
                        if x.id().contains("oob") {
                            x.mac_address.as_deref()
                        } else {
                            None
                        }
                    })
                    .unwrap_or("oob_net0 not found.")
            })
            .unwrap_or("Unknown");
        async_writeln!(output_file)?;
        async_writeln!(output_file, "DPU{i}")?;
        async_writeln!(
            output_file,
            "------------------------------------------------"
        )?;
        async_writeln!(output_file, "    BMC IP               : {}", x.bmc_ip)?;
        async_writeln!(
            output_file,
            "    Machine ID           : {}",
            dpu_report
                .as_ref()
                .and_then(|x| x.machine_id.as_deref())
                .unwrap_or("Unknown")
        )?;
        async_writeln!(
            output_file,
            "    Serial Number        : {}",
            system
                .as_ref()
                .and_then(|x| x.serial_number.as_deref())
                .unwrap_or("Unknown")
        )?;
        async_writeln!(
            output_file,
            "    Host PF Mac Address  : {}",
            x.host_pf_mac_address.as_deref().unwrap_or("Unknown")
        )?;
        async_writeln!(output_file, "    oob net0 Mac Address : {oob_mac}")?;
    }

    Ok(())
}

fn convert_endpoints_to_nice_table(endpoints: &[ExploredEndpoint]) -> Box<Table> {
    let mut table = Table::new();
    let headers = vec![
        "Address",
        "Type",
        "BMC Mac Address",
        "Vendor",
        "MachineId",
        "Pre-ingestion State",
        "Serial Number",
        "Last Exploration Error",
    ];

    table.set_titles(Row::new(
        headers.into_iter().map(Cell::new).collect::<Vec<Cell>>(),
    ));

    for endpoint in endpoints {
        table.add_row(endpoint_to_row(endpoint));
    }
    Box::new(table)
}

fn endpoint_to_row(endpoint: &ExploredEndpoint) -> Row {
    let report = &endpoint.report;
    let bmc_macs = report
        .as_ref()
        .and_then(|x| x.managers.first())
        .map(|x| {
            x.ethernet_interfaces
                .iter()
                .map(|a| {
                    let mac = a.mac_address.as_deref().unwrap_or_default();
                    if a.interface_enabled() {
                        if let Some(ls) = a.link_status.as_deref().none_if_empty() {
                            format!("{mac} [{}]", ls)
                        } else {
                            mac.to_string()
                        }
                    } else {
                        format!("{mac} - Disabled")
                    }
                })
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    let last_error = report
        .as_ref()
        .map(|report| report.last_exploration_error_display())
        .unwrap_or_default();

    let error_segmented = last_error
        .chars()
        .collect::<Vec<char>>()
        .chunks(70)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join("\n");

    let state = endpoint
        .preingestion_state
        .split_once(" ")
        .map(|(x, y)| {
            Cow::Owned(format!(
                "{}\n{}",
                x,
                y.chars()
                    .collect::<Vec<char>>()
                    .chunks(17)
                    .map(|x| x.iter().collect::<String>())
                    .collect::<Vec<String>>()
                    .join("\n")
            ))
        })
        .unwrap_or(Cow::Borrowed(endpoint.preingestion_state.as_str()));

    Row::new(vec![
        Cell::new(endpoint.address.as_str()),
        Cell::new(
            report
                .as_ref()
                .map(|x| x.endpoint_type.as_str())
                .unwrap_or_default(),
        ),
        Cell::new(bmc_macs.join("\n").as_str()),
        Cell::new(report.as_ref().map(|x| x.vendor()).unwrap_or_default()),
        Cell::new(report.as_ref().map(|x| x.machine_id()).unwrap_or_default()),
        Cell::new(&state),
        Cell::new(
            report
                .as_ref()
                .and_then(|x| x.systems.first())
                .map(|x| x.serial_number())
                .unwrap_or_default(),
        ),
        Cell::new(error_segmented.as_str()),
    ])
}

async fn display_endpoint(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    endpoint: ExploredEndpoint,
) -> CarbideCliResult<()> {
    let report = &endpoint.report;

    let mut table = Table::new();
    table.add_row(row!["Address", endpoint.address]);
    table.add_row(row![
        "Endpoint Type",
        report
            .as_ref()
            .map(|x| x.endpoint_type.as_str())
            .unwrap_or_default()
    ]);
    table.add_row(row![
        "Vendor",
        report.as_ref().map(|x| x.vendor()).unwrap_or_default()
    ]);
    table.add_row(row![
        "Machine ID",
        report.as_ref().map(|x| x.machine_id()).unwrap_or_default()
    ]);
    table.add_row(row!["Preingestion State", endpoint.preingestion_state]);
    let last_error = report
        .as_ref()
        .map(|report| report.last_exploration_error_display())
        .unwrap_or_default();

    let error_segmented = last_error
        .chars()
        .collect::<Vec<char>>()
        .chunks(175)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<String>>()
        .join("\n");

    table.add_row(row!["Last Exploration Error", error_segmented]);
    table.add_row(row!["Report Version", endpoint.report_version]);
    table.add_row(row![
        "Exploration Requested",
        endpoint.exploration_requested
    ]);

    async_write!(output_file, "{}", table)?;

    // Systems
    if let Some(system) = report.as_ref().and_then(|x| x.systems.first()) {
        let mut table = Table::new();
        async_writeln!(output_file)?;
        async_writeln!(output_file, "Systems (First only)")?;
        table.add_row(row!["Id", system.id]);
        table.add_row(row!["Manufacturer", system.manufacturer()]);
        table.add_row(row!["Model", system.model()]);
        table.add_row(row!["Serial Number", system.serial_number()]);

        let mut ethernet_interface_table = Table::new();
        ethernet_interface_table.set_titles(row!["Id", "Mac Address", "Enabled", "Link status"]);

        for eth in &system.ethernet_interfaces {
            ethernet_interface_table.add_row(row![
                eth.id(),
                eth.mac_address(),
                eth.interface_enabled.unwrap_or_default(),
                eth.link_status.as_deref().unwrap_or("")
            ]);
        }
        table.add_row(row![
            "Ethernet Interfaces",
            ethernet_interface_table.to_string()
        ]);

        async_write!(output_file, "{}", table)?;
    }

    // Managers
    if let Some(manager) = report.as_ref().and_then(|x| x.managers.first()) {
        let mut table = Table::new();
        async_writeln!(output_file)?;
        async_writeln!(output_file, "Managers (First only)")?;
        table.add_row(row!["Id", manager.id]);

        let mut ethernet_interface_table = Table::new();
        ethernet_interface_table.set_titles(row!["Id", "Mac Address", "Enabled", "Link status"]);

        for eth in &manager.ethernet_interfaces {
            ethernet_interface_table.add_row(row![
                eth.id(),
                eth.mac_address(),
                eth.interface_enabled.unwrap_or_default(),
                eth.link_status.as_deref().unwrap_or("")
            ]);
        }
        table.add_row(row![
            "Ethernet Interfaces",
            ethernet_interface_table.to_string()
        ]);

        async_write!(output_file, "{}", table)?;
    }

    // Chassis
    if let Some(chassis) = report.as_ref().and_then(|x| x.chassis.first()) {
        let mut table = Table::new();
        async_writeln!(output_file)?;
        async_writeln!(output_file, "Chassis (First only)")?;
        table.add_row(row!["Id", chassis.id]);
        table.add_row(row!["Manufacturer", chassis.manufacturer()]);
        table.add_row(row!["Model", chassis.model()]);
        table.add_row(row!["Serial Number", chassis.serial_number()]);
        table.add_row(row!["Part Number", chassis.part_number()]);

        let mut ethernet_interface_table = Table::new();
        ethernet_interface_table.set_titles(row![
            "Id",
            "Manufacturer",
            "Model",
            "Part Number",
            "Serial Number"
        ]);

        for eth in &chassis.network_adapters {
            ethernet_interface_table.add_row(row![
                eth.id.as_str(),
                eth.manufacturer(),
                eth.model(),
                eth.part_number(),
                eth.serial_number()
            ]);
        }
        table.add_row(row![
            "Ethernet Interfaces",
            ethernet_interface_table.to_string()
        ]);

        async_write!(output_file, "{}", table)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use ::rpc::site_explorer::{EndpointExplorationReport, ExploredEndpoint, OperatorErrorSchema};
    use carbide_test_support::{Check, check_values};

    use super::{convert_endpoints_to_nice_table, filter_endpoints};

    fn operator_error_schema() -> OperatorErrorSchema {
        OperatorErrorSchema {
            error_code: "NICO-SITEEXPLORER-122".to_string(),
            mitigation: Some("Check the HCL".to_string()),
            text: "BMC vendor missing".to_string(),
        }
    }

    fn endpoint(address: &str, report: EndpointExplorationReport) -> ExploredEndpoint {
        ExploredEndpoint {
            address: address.to_string(),
            report: Some(report),
            ..Default::default()
        }
    }

    #[test]
    fn endpoint_error_filter_and_table_rendering_use_structured_errors() {
        let error = endpoint(
            "192.0.2.10",
            EndpointExplorationReport {
                last_exploration_error_schema: Some(operator_error_schema()),
                ..Default::default()
            },
        );
        let clear = endpoint("192.0.2.11", EndpointExplorationReport::default());
        let endpoints = vec![error.clone(), clear];

        check_values(
            [
                Check {
                    scenario: "error-only",
                    input: (true, false),
                    expect: vec!["192.0.2.10".to_string()],
                },
                Check {
                    scenario: "success-only",
                    input: (false, true),
                    expect: vec!["192.0.2.11".to_string()],
                },
                Check {
                    scenario: "all endpoints",
                    input: (false, false),
                    expect: vec!["192.0.2.10".to_string(), "192.0.2.11".to_string()],
                },
            ],
            |(error_only, success_only)| {
                filter_endpoints(
                    endpoints.clone(),
                    error_only,
                    success_only,
                    Vec::new(),
                    None,
                )
                .into_iter()
                .map(|endpoint| endpoint.address)
                .collect::<Vec<_>>()
            },
        );

        let rendered = convert_endpoints_to_nice_table(&[error]).to_string();
        assert!(rendered.contains("NICO-SITEEXPLORER-122"));
        assert!(rendered.contains("BMC vendor missing"));
    }
}
