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
use clap::ValueEnum;
use mac_address::MacAddress;
use prettytable::{Table, row};
use rpc::forge::ExpectedMachineRequest;

use super::args::Args;
use crate::async_write;
use crate::errors::CarbideCliResult;
use crate::expected_machines::common::HostDpuPolicy;
use crate::rpc::ApiClient;

pub(super) async fn show_expected_machines(
    expected_machine_query: &Args,
    api_client: &ApiClient,
    output_format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let req: Option<ExpectedMachineRequest> = expected_machine_query.try_into()?;

    if let Some(req) = req {
        let expected_machine = api_client.0.get_expected_machine(req).await?;
        if output_format == OutputFormat::Json {
            async_write!(
                output,
                "{}",
                serde_json::ser::to_string_pretty(&expected_machine)?
            )?;
        } else {
            async_write!(output, "{:#?}", expected_machine)?;
        }
        return Ok(());
    }

    let expected_machines = api_client.0.get_all_expected_machines().await?;
    if output_format == OutputFormat::Json {
        async_write!(
            output,
            "{}",
            serde_json::to_string_pretty(&expected_machines)?
        )?;
        return Ok(());
    }

    // TODO: This should be optimised. `find_interfaces` should accept a list of macs also and
    // return related interfaces details.
    let all_mi = api_client.get_all_machines_interfaces(None).await?;
    let expected_macs = expected_machines
        .expected_machines
        .iter()
        .filter_map(|x| x.bmc_mac_address.parse().ok())
        .collect::<Vec<MacAddress>>();

    let expected_mi: HashMap<MacAddress, ::rpc::forge::MachineInterface> =
        HashMap::from_iter(all_mi.interfaces.into_iter().filter_map(|x| {
            let mac = x.mac_address.parse().ok()?;
            if expected_macs.contains(&mac) {
                Some((mac, x))
            } else {
                None
            }
        }));

    let bmc_ips = expected_mi
        .values()
        .filter_map(|interface| {
            let ip = interface.address.first()?;
            Some(ip.clone())
        })
        .collect::<Vec<_>>();

    let expected_bmc_ip_vs_ids = HashMap::from_iter(
        api_client
            .0
            .find_machine_ids_by_bmc_ips(bmc_ips)
            .await?
            .pairs
            .into_iter()
            .map(|x| {
                (
                    x.bmc_ip,
                    x.machine_id
                        .map(|x| x.to_string())
                        .unwrap_or("Unlinked".to_string()),
                )
            }),
    );

    convert_and_print_into_nice_table(
        output,
        &expected_machines,
        &expected_bmc_ip_vs_ids,
        &expected_mi,
    )
    .await?;

    Ok(())
}

async fn convert_and_print_into_nice_table(
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    expected_machines: &::rpc::forge::ExpectedMachineList,
    expected_discovered_machine_ids: &HashMap<String, String>,
    expected_discovered_machine_interfaces: &HashMap<MacAddress, ::rpc::forge::MachineInterface>,
) -> CarbideCliResult<()> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "Serial Number",
        "BMC Mac",
        "Interface IP",
        "Fallback DPUs",
        "Associated Machine",
        "Name",
        "Description",
        "Labels",
        "SKU ID",
        "Pause On Ingestion",
        "DPF Enabled",
        "Disable Lockdown",
        "DPU Policy",
        "DPU Loopbacks",
    ]);

    for expected_machine in &expected_machines.expected_machines {
        let default_pause_ingestion_and_poweron =
            expected_machine.default_pause_ingestion_and_poweron();

        let machine_interface = expected_machine
            .bmc_mac_address
            .parse::<MacAddress>()
            .ok()
            .and_then(|m| expected_discovered_machine_interfaces.get(&m));
        let machine_id = expected_discovered_machine_ids
            .get(
                machine_interface
                    .and_then(|x| x.address.first().map(String::as_str))
                    .unwrap_or("unknown"),
            )
            .map(String::as_str);

        let labels = crate::metadata::fmt_labels_as_kv_pairs(expected_machine.metadata.as_ref());

        let dpu_policy_display = dpu_policy_display(expected_machine.dpu_mode);

        table.add_row(row![
            expected_machine.chassis_serial_number,
            expected_machine.bmc_mac_address,
            machine_interface
                .map(|x| x.address.join("\n"))
                .unwrap_or("Undiscovered".to_string()),
            expected_machine.fallback_dpu_serial_numbers.join("\n"),
            machine_id.unwrap_or("Unlinked"),
            expected_machine
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            expected_machine
                .metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
            labels.join(", "),
            expected_machine.sku_id.as_deref().unwrap_or_default(),
            default_pause_ingestion_and_poweron,
            // is_dpf_enabled will be None only for the servers where old proto file is used.
            #[allow(deprecated)]
            expected_machine
                .is_dpf_enabled
                .unwrap_or(expected_machine.dpf_enabled)
                .to_string(),
            expected_machine
                .host_lifecycle_profile
                .and_then(|hlp| hlp.disable_lockdown)
                .unwrap_or_default()
                .to_string(),
            dpu_policy_display,
            format_dpu_loopback_reservations(&expected_machine.dpu_loopback_reservations),
        ]);
    }

    async_write!(output, "{}", table)?;

    Ok(())
}

/// Formats the DPU loopback reservations for the table, one reservation per
/// line as `serial: v4=<addr> v6=<addr>`. A missing address in a family renders
/// as `-`; no reservations renders as an empty cell.
fn format_dpu_loopback_reservations(
    reservations: &Option<::rpc::forge::DpuLoopbackReservationList>,
) -> String {
    let Some(list) = reservations else {
        return String::new();
    };
    list.reservations
        .iter()
        .map(|reservation| {
            format!(
                "{}: v4={} v6={}",
                reservation.dpu_serial_number,
                reservation.loopback_ipv4.as_deref().unwrap_or("-"),
                reservation.loopback_ipv6.as_deref().unwrap_or("-"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Formats the stable Forge compatibility field with policy vocabulary.
fn dpu_policy_display(dpu_mode: Option<i32>) -> String {
    dpu_mode
        .and_then(|value| ::rpc::forge::DpuMode::try_from(value).ok())
        .map(HostDpuPolicy::from)
        .filter(|policy| *policy != HostDpuPolicy::Unspecified)
        .unwrap_or(HostDpuPolicy::Manage)
        .to_possible_value()
        .map(|value| value.get_name().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use rpc::forge::DpuMode;

    use super::dpu_policy_display;

    #[test]
    fn dpu_policy_display_uses_canonical_policy_vocabulary() {
        value_scenarios!(
            run = dpu_policy_display;
            "missing value defaults to manage" {
                None => "manage".to_string(),
            }

            "protobuf sentinel defaults to manage" {
                Some(DpuMode::Unspecified as i32) => "manage".to_string(),
            }

            "DPU mode displays as manage" {
                Some(DpuMode::DpuMode as i32) => "manage".to_string(),
            }

            "NIC mode displays as use as NIC" {
                Some(DpuMode::NicMode as i32) => "nic".to_string(),
            }

            "no DPU displays as ignore" {
                Some(DpuMode::NoDpu as i32) => "ignore".to_string(),
            }

            "unknown value preserves historical manage default" {
                Some(i32::MAX) => "manage".to_string(),
            }
        );
    }
}
