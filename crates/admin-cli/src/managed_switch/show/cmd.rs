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
use std::fmt::Write;

use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::switch::SwitchId;
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{LinkedExpectedSwitch, MachineInterface, Switch};
use serde::Serialize;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

const UNKNOWN: &str = "Unknown";

#[derive(Default, Serialize)]
struct ManagedSwitchOutputWrapper {
    options: ManagedSwitchOutputOptions,
    managed_switch_output: ManagedSwitchOutput,
}

#[derive(Serialize)]
struct ManagedSwitchList<'a> {
    managed_switches: &'a [ManagedSwitchOutput],
}

#[derive(Default, Clone, Copy, Serialize)]
struct ManagedSwitchOutputOptions {
    show_ips: bool,
    more_details: bool,
    single_switch_detail_view: bool,
}

#[derive(Default, Serialize)]
struct ManagedSwitchOutput {
    switch_id: Option<String>,
    name: String,
    serial_number: String,
    bmc_mac: String,
    bmc_ip: Option<String>,
    bmc_version: Option<String>,
    bmc_firmware_version: Option<String>,
    nvos_mac_addresses: Vec<String>,
    controller_state: String,
    power_state: Option<String>,
    health_status: Option<String>,
    expected_switch_id: Option<String>,
    explored_endpoint: Option<String>,
    rack_id: Option<String>,
    slot_number: Option<i32>,
    tray_index: Option<i32>,
    state_reason: Option<String>,
}

/// Build a map from SwitchId -> list of NVOS MAC addresses by filtering
/// machine interfaces that have a switch_id foreign key set.
fn build_nvos_mac_map(interfaces: &[MachineInterface]) -> HashMap<SwitchId, Vec<String>> {
    let mut map: HashMap<SwitchId, Vec<String>> = HashMap::new();
    for mi in interfaces {
        if let Some(switch_id) = mi.switch_id {
            map.entry(switch_id)
                .or_default()
                .push(mi.mac_address.clone());
        }
    }
    map
}

fn build_managed_switch_outputs(
    switches: Vec<Switch>,
    linked: Vec<LinkedExpectedSwitch>,
    nvos_mac_map: &HashMap<SwitchId, Vec<String>>,
) -> Vec<ManagedSwitchOutput> {
    let switch_map: HashMap<String, &Switch> = switches
        .iter()
        .filter_map(|s| s.id.as_ref().map(|id| (id.to_string(), s)))
        .collect();

    let mut outputs: Vec<ManagedSwitchOutput> = Vec::new();
    let mut seen_switch_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for linked_switch in &linked {
        let switch = linked_switch
            .switch_id
            .as_ref()
            .and_then(|id| switch_map.get(&id.to_string()));

        if switch.is_none() {
            continue;
        }

        let switch_id_str = linked_switch.switch_id.as_ref().map(|id| id.to_string());

        if let Some(ref id) = switch_id_str {
            seen_switch_ids.insert(id.clone());
        }

        let nvos_macs = linked_switch
            .switch_id
            .as_ref()
            .and_then(|id| nvos_mac_map.get(id).cloned())
            .unwrap_or_default();

        outputs.push(ManagedSwitchOutput {
            switch_id: switch_id_str,
            name: switch
                .and_then(|s| s.config.as_ref().map(|c| c.name.clone()))
                .unwrap_or_else(|| linked_switch.switch_serial_number.clone()),
            serial_number: linked_switch.switch_serial_number.clone(),
            bmc_mac: linked_switch.bmc_mac_address.clone(),
            bmc_ip: linked_switch.explored_endpoint_address.clone(),
            bmc_version: switch
                .and_then(|s| s.bmc_info.as_ref())
                .and_then(|b| b.version.clone()),
            bmc_firmware_version: switch
                .and_then(|s| s.bmc_info.as_ref())
                .and_then(|b| b.firmware_version.clone()),
            nvos_mac_addresses: nvos_macs,
            controller_state: switch
                .map(|s| s.controller_state.clone())
                .unwrap_or_else(|| "NotCreated".to_string()),
            power_state: switch.and_then(|s| {
                s.status
                    .as_ref()
                    .and_then(|st| st.power_state.as_ref().cloned())
            }),
            health_status: switch.and_then(|s| {
                s.status
                    .as_ref()
                    .and_then(|st| st.health_status.as_ref().cloned())
            }),
            expected_switch_id: linked_switch
                .expected_switch_id
                .as_ref()
                .map(|id| id.value.clone()),
            explored_endpoint: linked_switch.explored_endpoint_address.clone(),
            rack_id: linked_switch.rack_id.as_ref().map(|id| id.to_string()),
            slot_number: switch
                .and_then(|s| s.placement_in_rack.as_ref().and_then(|p| p.slot_number)),
            tray_index: switch
                .and_then(|s| s.placement_in_rack.as_ref().and_then(|p| p.tray_index)),
            state_reason: switch.and_then(|s| {
                s.status
                    .as_ref()
                    .and_then(|st| st.state_reason.as_ref().and_then(|r| r.outcome_msg.clone()))
            }),
        });
    }

    for switch in &switches {
        let id_str = switch.id.as_ref().map(|id| id.to_string());
        if let Some(ref id) = id_str
            && seen_switch_ids.contains(id)
        {
            continue;
        }

        let nvos_macs = switch
            .id
            .as_ref()
            .and_then(|id| nvos_mac_map.get(id).cloned())
            .unwrap_or_default();

        outputs.push(ManagedSwitchOutput {
            switch_id: id_str,
            name: switch
                .config
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_default(),
            serial_number: String::new(),
            bmc_mac: switch
                .bmc_info
                .as_ref()
                .and_then(|b| b.mac.clone())
                .unwrap_or_default(),
            bmc_ip: switch.bmc_info.as_ref().and_then(|b| b.ip.clone()),
            bmc_version: switch.bmc_info.as_ref().and_then(|b| b.version.clone()),
            bmc_firmware_version: switch
                .bmc_info
                .as_ref()
                .and_then(|b| b.firmware_version.clone()),
            nvos_mac_addresses: nvos_macs,
            controller_state: switch.controller_state.clone(),
            power_state: switch.status.as_ref().and_then(|st| st.power_state.clone()),
            health_status: switch
                .status
                .as_ref()
                .and_then(|st| st.health_status.clone()),
            expected_switch_id: None,
            explored_endpoint: None,
            rack_id: None,
            slot_number: switch
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.slot_number),
            tray_index: switch.placement_in_rack.as_ref().and_then(|p| p.tray_index),
            state_reason: switch
                .status
                .as_ref()
                .and_then(|st| st.state_reason.as_ref().and_then(|r| r.outcome_msg.clone())),
        });
    }

    outputs
}

impl From<ManagedSwitchOutputWrapper> for Row {
    fn from(src: ManagedSwitchOutputWrapper) -> Self {
        let value = src.managed_switch_output;

        let is_unhealthy = value
            .health_status
            .as_deref()
            .map(|s| !s.eq_ignore_ascii_case("OK") && !s.eq_ignore_ascii_case("Healthy"))
            .unwrap_or(false);

        let bmc_mac = if value.bmc_mac.is_empty() {
            UNKNOWN.to_string()
        } else {
            value.bmc_mac
        };

        let nvos_mac = if value.nvos_mac_addresses.is_empty() {
            UNKNOWN.to_string()
        } else {
            value.nvos_mac_addresses.join("\n")
        };

        let serial = if value.serial_number.is_empty() {
            UNKNOWN.to_string()
        } else {
            value.serial_number
        };

        let mut row_data = vec![
            String::from(if is_unhealthy { "U" } else { "H" }),
            value.switch_id.unwrap_or_else(|| UNKNOWN.to_string()),
            value.name,
            value.controller_state,
        ];

        if src.options.show_ips {
            row_data.extend_from_slice(&[bmc_mac, nvos_mac]);
        }

        if src.options.more_details {
            row_data.extend_from_slice(&[
                serial,
                value.power_state.unwrap_or_else(|| UNKNOWN.to_string()),
                value.health_status.unwrap_or_else(|| UNKNOWN.to_string()),
            ]);
        }

        Row::new(row_data.into_iter().map(|x| Cell::new(&x)).collect())
    }
}

fn convert_managed_switches_to_nice_output(
    managed_switches: Vec<ManagedSwitchOutput>,
    options: ManagedSwitchOutputOptions,
) -> Box<Table> {
    let managed_switches_wrapper = managed_switches
        .into_iter()
        .map(|x| ManagedSwitchOutputWrapper {
            options,
            managed_switch_output: x,
        })
        .collect::<Vec<ManagedSwitchOutputWrapper>>();

    let mut table = Table::new();

    let mut headers = vec!["", "Switch ID", "Name", "State"];

    if options.show_ips {
        headers.extend_from_slice(&["BMC MAC", "NVOS MAC"]);
    }

    if options.more_details {
        headers.extend_from_slice(&["Serial", "Power", "Health"]);
    }

    // TODO additional discovery work needed for remaining information
    table.set_titles(Row::new(
        headers.into_iter().map(Cell::new).collect::<Vec<Cell>>(),
    ));

    for managed_switch in managed_switches_wrapper {
        table.add_row(managed_switch.into());
    }

    table.into()
}

async fn show_managed_switches(
    managed_switches: Vec<ManagedSwitchOutput>,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: OutputFormat,
    output_options: ManagedSwitchOutputOptions,
) -> CarbideCliResult<()> {
    match output_format {
        OutputFormat::Json => {
            if output_options.single_switch_detail_view {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        managed_switches.first().ok_or(CarbideCliError::Empty)?
                    )?
                )
            } else {
                let wrapped = ManagedSwitchList {
                    managed_switches: &managed_switches,
                };
                println!("{}", serde_json::to_string_pretty(&wrapped)?)
            }
        }
        OutputFormat::Yaml => {
            if output_options.single_switch_detail_view {
                println!(
                    "{}",
                    serde_yaml::to_string(managed_switches.first().ok_or(CarbideCliError::Empty)?)?
                )
            } else {
                let wrapped = ManagedSwitchList {
                    managed_switches: &managed_switches,
                };
                println!("{}", serde_yaml::to_string(&wrapped)?)
            }
        }
        OutputFormat::Csv => {
            let result = convert_managed_switches_to_nice_output(managed_switches, output_options);
            async_write_table_as_csv!(output_file, result)?;
        }
        _ => {
            if output_options.single_switch_detail_view {
                show_managed_switch_details_view(
                    managed_switches
                        .into_iter()
                        .next()
                        .ok_or(CarbideCliError::Empty)?,
                )?;
            } else {
                let result =
                    convert_managed_switches_to_nice_output(managed_switches, output_options);
                async_write!(output_file, "{}", result)?;
            }
        }
    }
    Ok(())
}

fn show_managed_switch_details_view(m: ManagedSwitchOutput) -> CarbideCliResult<()> {
    let width = 27;
    let mut lines = String::new();

    writeln!(&mut lines, "Name        : {}", m.name)?;
    writeln!(&mut lines, "State       : {}", m.controller_state)?;
    if let Some(ref reason) = m.state_reason {
        writeln!(&mut lines, "    Reason  : {}", reason)?;
    }

    writeln!(
        &mut lines,
        "\nSwitch:\n----------------------------------------"
    )?;

    let data = vec![
        ("  ID", m.switch_id),
        ("  Slot Number", m.slot_number.map(|n| n.to_string())),
        ("  Tray Index", m.tray_index.map(|n| n.to_string())),
        ("  Serial Number", m.serial_number.none_if_empty()),
        ("  Rack ID", m.rack_id),
        ("  Power State", m.power_state),
        ("  Health", m.health_status),
        (
            "  NVOS MAC Addresses",
            m.nvos_mac_addresses.join(", ").none_if_empty(),
        ),
        ("  BMC", Some(String::new())),
        ("    Version", m.bmc_version),
        ("    Firmware Version", m.bmc_firmware_version),
        ("    IP", m.bmc_ip),
        ("    MAC", m.bmc_mac.none_if_empty()),
    ];

    for (key, value) in data {
        if matches!(&value, Some(x) if x.is_empty()) {
            writeln!(&mut lines, "{key:<width$}")?;
        } else {
            writeln!(
                &mut lines,
                "{:<width$}: {}",
                key,
                value.unwrap_or(UNKNOWN.to_string())
            )?;
        }
    }

    println!("{lines}");
    Ok(())
}

pub(super) async fn handle_show(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let (switch_id, name) = args.parse_identifier();
    let is_single = switch_id.is_some() || name.is_some();

    let query = rpc::forge::SwitchQuery { name, switch_id };
    let switches = api_client.0.find_switches(query).await?.switches;
    let linked = api_client
        .0
        .get_all_expected_switches_linked()
        .await?
        .expected_switches;
    let all_interfaces = api_client.get_all_machines_interfaces(None).await?;
    let nvos_mac_map = build_nvos_mac_map(&all_interfaces.interfaces);

    let outputs = build_managed_switch_outputs(switches, linked, &nvos_mac_map);

    let output_options = ManagedSwitchOutputOptions {
        show_ips: args.ips,
        more_details: args.more,
        single_switch_detail_view: is_single,
    };

    show_managed_switches(outputs, output_file, output_format, output_options).await
}
