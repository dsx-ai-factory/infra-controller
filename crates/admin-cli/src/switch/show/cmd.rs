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

use carbide_uuid::switch::SwitchId;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{Switch, SwitchList, SwitchSearchFilter};

use super::args::Args;
use crate::cfg::cli_options::SortField;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::{async_write, async_write_table_as_csv};

/// Converts a SwitchList to a Table object.
fn to_table(switches: &SwitchList) -> Table {
    let mut table = Table::new();

    table.set_titles(row![
        "ID",
        "Name",
        "Metadata Name",
        "Slot",
        "Tray",
        "Primary",
        "Power State",
        "Health",
        "FabricManager(nmxc)",
        "State"
    ]);

    for switch in switches.switches.iter() {
        let id = switch
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "N/A".to_string());

        let name = switch
            .config
            .as_ref()
            .map(|config| config.name.as_str())
            .unwrap_or("N/A");

        let metadata_name = switch
            .metadata
            .as_ref()
            .map(|m| m.name.as_str())
            .unwrap_or("N/A");

        let slot_number = switch
            .placement_in_rack
            .as_ref()
            .and_then(|p| p.slot_number)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "N/A".to_string());

        let tray_index = switch
            .placement_in_rack
            .as_ref()
            .and_then(|p| p.tray_index)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "N/A".to_string());

        let power_state = switch
            .status
            .as_ref()
            .and_then(|status| status.power_state.as_deref())
            .unwrap_or("N/A");

        let health = switch
            .status
            .as_ref()
            .and_then(|status| status.health_status.as_deref())
            .unwrap_or("N/A");
        let is_primary = if switch.is_primary { "Yes" } else { "No" };
        let fabric_manager_status = switch
            .status
            .as_ref()
            .and_then(|status| status.fabric_manager_status.as_deref())
            .unwrap_or("N/A");

        table.add_row(row![
            id,
            name,
            metadata_name,
            slot_number,
            tray_index,
            is_primary,
            power_state,
            health,
            fabric_manager_status,
            switch.controller_state,
        ]);
    }

    table
}

/// Displays a list of switches in a specified output format, optionally sorted by a given
/// field. Output destination is controlled by the output_file parameter.
async fn show_switches(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
    api_client: &ApiClient,
    page_size: usize,
    sort_by: &SortField,
) -> CarbideCliResult<()> {
    let filter: SwitchSearchFilter = SwitchSearchFilter::default();
    let mut switch_list = api_client.get_all_switches(filter, page_size).await?;

    // Sort the switches by the specified field.
    match sort_by {
        SortField::PrimaryId => switch_list.switches.sort_by_key(|switch| switch.id),
        SortField::State => switch_list.switches.sort_by(|s1, s2| {
            let default_state = "N/A".to_string();
            let state1 = s1
                .status
                .as_ref()
                .and_then(|status| status.controller_state.as_ref())
                .unwrap_or(&default_state);
            let state2 = s2
                .status
                .as_ref()
                .and_then(|status| status.controller_state.as_ref())
                .unwrap_or(&default_state);
            state1.cmp(state2)
        }),
    }

    match output_format {
        OutputFormat::Json | OutputFormat::Yaml => {
            return Err(CarbideCliError::NotImplemented(output_format.to_string()));
        }
        OutputFormat::Csv | OutputFormat::AsciiTable => {
            let table = to_table(&switch_list);

            // Print table as either as either CSV or ASCII
            if let OutputFormat::Csv = output_format {
                async_write_table_as_csv!(output_file, table)?;
            } else {
                // ASCII
                async_write!(output_file, "{}", table)?;
            }
        }
    }
    Ok(())
}

/// Displays detailed information about a specific switch in a specified output format
/// to a specified output destination.
async fn show_switch_information(
    switch_id: SwitchId,
    output_format: &OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let switches = api_client.get_one_switch(switch_id).await?.switches;
    if switches.is_empty() {
        return Err(CarbideCliError::SwitchNotFound(switch_id));
    } else if switches.len() > 1 {
        // This really shouldn't happen in practice.
        return Err(CarbideCliError::GenericError(format!(
            "Expected 1 switch, but got {}.",
            switches.len()
        )));
    }

    let switch = &switches[0];
    match output_format {
        OutputFormat::Json => {
            return Err(CarbideCliError::NotImplemented(output_format.to_string()));
        }
        OutputFormat::AsciiTable => async_write!(
            output_file,
            "{}",
            switch_details_text(switch).unwrap_or_else(|x| x.to_string())
        )?,
        OutputFormat::Csv => {
            return Err(CarbideCliError::NotImplemented(output_format.to_string()));
        }
        OutputFormat::Yaml => {
            return Err(CarbideCliError::NotImplemented(output_format.to_string()));
        }
    }

    Ok(())
}

/// Builds and returns a detailed text representation of the Switch for CLI output,
/// roughly following the structure of the Switch RPC message.
fn switch_details_text(switch: &Switch) -> CarbideCliResult<String> {
    let mut lines = String::new();

    let data: Vec<(&str, String)> = vec![
        (
            "ID",
            switch
                .id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        (
            "Rack ID",
            switch
                .rack_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
        ("State Version", switch.state_version.clone()),
        ("Version", switch.version.clone()),
        (
            "Primary",
            if switch.is_primary {
                "Yes".to_string()
            } else {
                "No".to_string()
            },
        ),
        (
            "Slot Number",
            switch
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.slot_number)
                .map(|s| s.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
        ),
        (
            "Tray Index",
            switch
                .placement_in_rack
                .as_ref()
                .and_then(|p| p.tray_index)
                .map(|t| t.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
        ),
        (
            "Deleted At",
            switch
                .deleted
                .as_ref()
                .map(|ts| ts.to_string())
                .unwrap_or_else(|| "N/A".to_string()),
        ),
    ];

    let width = 1 + data
        .iter()
        .fold(0, |accum, (key, _)| std::cmp::max(accum, key.len()));

    for (key, value) in data {
        writeln!(&mut lines, "{key:<width$}: {value}")?;
    }

    // Config
    writeln!(&mut lines, "\nConfig:")?;
    if let Some(config) = &switch.config {
        writeln!(&mut lines, "\tName       : {}", config.name)?;
        writeln!(&mut lines, "\tNMX-C Configured : {}", config.enable_nmxc)?;
        writeln!(
            &mut lines,
            "\tNMX-C Effective  : {}",
            config.enable_nmxc || switch.is_primary
        )?;
        if let Some(fm_config) = &config.fabric_manager_config
            && !fm_config.config_map.is_empty()
        {
            writeln!(&mut lines, "\tFabric Manager Config:")?;
            let mut sorted_keys: Vec<&String> = fm_config.config_map.keys().collect();
            sorted_keys.sort();
            for k in sorted_keys {
                writeln!(&mut lines, "\t\t{k}: {}", fm_config.config_map[k])?;
            }
        }
    } else {
        writeln!(&mut lines, "\tNone")?;
    }

    // Status
    writeln!(&mut lines, "\nStatus:")?;
    if let Some(status) = &switch.status {
        let status_data: Vec<(&str, String)> = vec![
            (
                "Switch Name",
                status
                    .switch_name
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
            (
                "Power State",
                status
                    .power_state
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
            (
                "Health Status",
                status
                    .health_status
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
            (
                "Controller State",
                status
                    .controller_state
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
            (
                "Fabric Manager",
                status
                    .fabric_manager_status
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
        ];
        let sw = 1 + status_data
            .iter()
            .fold(0, |acc, (k, _)| std::cmp::max(acc, k.len()));
        for (key, value) in status_data {
            writeln!(&mut lines, "\t{key:<sw$}: {value}")?;
        }

        // Lifecycle
        if let Some(lifecycle) = &status.lifecycle {
            writeln!(&mut lines, "\tLifecycle:")?;
            writeln!(
                &mut lines,
                "\t\tState   : {}",
                lifecycle.state.to_uppercase()
            )?;
            writeln!(&mut lines, "\t\tVersion : {}", lifecycle.version)?;
            if let Some(reason) = &lifecycle.state_reason {
                writeln!(
                    &mut lines,
                    "\t\tReason  : {}",
                    reason.outcome_msg.as_deref().unwrap_or("N/A")
                )?;
            }
            if let Some(sla) = &lifecycle.sla {
                writeln!(
                    &mut lines,
                    "\t\tSLA Breached: {}",
                    sla.time_in_state_above_sla
                )?;
            }
        }

        if let Some(fm_details) = &status.fabric_manager_status_details {
            writeln!(&mut lines, "\tFabric Manager Status:")?;
            writeln!(
                &mut lines,
                "\t\tState  : {}",
                fm_details.fabric_manager_state
            )?;
            if let Some(info) = &fm_details.addition_info {
                writeln!(&mut lines, "\t\tInfo   : {info}")?;
            }
            if let Some(reason) = &fm_details.reason {
                writeln!(&mut lines, "\t\tReason : {reason}")?;
            }
            if let Some(err) = &fm_details.error_message {
                writeln!(&mut lines, "\t\tError  : {err}")?;
            }
        }

        if !status.health_sources.is_empty() {
            writeln!(&mut lines, "\tHealth Sources:")?;
            for hs in &status.health_sources {
                writeln!(&mut lines, "\t\tmode={} source={}", hs.mode, hs.source)?;
            }
        }
    } else {
        writeln!(&mut lines, "\tNone")?;
    }

    // BMC Info
    writeln!(&mut lines, "\nBMC:")?;
    if let Some(bmc) = &switch.bmc_info {
        let bmc_data: Vec<(&str, String)> = vec![
            (
                "Machine Interface ID",
                bmc.machine_interface_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
            ("IP", bmc.ip.clone().unwrap_or_else(|| "N/A".to_string())),
            ("MAC", bmc.mac.clone().unwrap_or_else(|| "N/A".to_string())),
            (
                "Version",
                bmc.version.clone().unwrap_or_else(|| "N/A".to_string()),
            ),
            (
                "Firmware Version",
                bmc.firmware_version
                    .clone()
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
            (
                "Port",
                bmc.port
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "N/A".to_string()),
            ),
        ];
        let bw = 1 + bmc_data
            .iter()
            .fold(0, |acc, (k, _)| std::cmp::max(acc, k.len()));
        for (key, value) in bmc_data {
            writeln!(&mut lines, "\t{key:<bw$}: {value}")?;
        }
    } else {
        writeln!(&mut lines, "\tNone")?;
    }

    crate::metadata::write_metadata_in_nice_format(&mut lines, width, switch.metadata.as_ref())?;

    Ok(lines)
}

pub(super) async fn handle_show(
    args: Args,
    output_format: &OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
    page_size: usize,
    sort_by: &SortField,
) -> CarbideCliResult<()> {
    if let Some(switch_id) = args.switch_id {
        show_switch_information(switch_id, output_format, output_file, api_client).await
    } else {
        show_switches(output_file, output_format, api_client, page_size, sort_by).await
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::SwitchId;
    use rpc::forge::{BmcInfo, Metadata, PlacementInRack, Switch, SwitchConfig, SwitchStatus};

    use super::*;

    // switch_details_text_formats_representative_switch calls switch_details_text
    // with a representative Switch and verifies fields from each output section.
    #[test]
    fn switch_details_text_formats_representative_switch() {
        let switch = Switch {
            id: Some(
                SwitchId::from_str("sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60")
                    .unwrap(),
            ),
            rack_id: Some(RackId::from_str("ipp6-gb200-36x1").unwrap()),
            state_version: "V3-T1774905143273055".to_string(),
            version: "V1-T1778792629596284".to_string(),
            config: Some(SwitchConfig {
                name: "MT2519600UD6".to_string(),
                enable_nmxc: false,
                fabric_manager_config: None,
            }),
            status: Some(SwitchStatus {
                switch_name: Some(
                    "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60".to_string(),
                ),
                power_state: Some("on".to_string()),
                health_status: Some("ok".to_string()),
                controller_state: Some("ready".to_string()),
                fabric_manager_status: Some("not_running".to_string()),
                ..Default::default()
            }),
            placement_in_rack: Some(PlacementInRack {
                slot_number: Some(13),
                tray_index: Some(8),
            }),
            is_primary: true,
            controller_state:
                r#"{"state":"reprovisioning","reprovisioning_state":"WaitingForNVOSUpgrade"}"#
                    .to_string(),
            metadata: Some(Metadata {
                name: "sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60".to_string(),
                ..Default::default()
            }),
            bmc_info: Some(BmcInfo {
                ip: Some("10.85.14.106".to_string()),
                mac: Some("E0:9D:73:F0:45:96".to_string()),
                version: Some("V1-T1778792629596284".to_string()),
                firmware_version: Some("1.3.5-GA".to_string()),
                machine_interface_id: None,
                port: Some(443),
            }),
            ..Default::default()
        };

        let output = switch_details_text(&switch).expect("switch_details_text should succeed");

        for expected in [
            "ID            : sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60",
            "Primary       : Yes",
            "\tName       : MT2519600UD6",
            "\tNMX-C Configured : false",
            "\tNMX-C Effective  : true",
            "\tPower State      : on",
            "\tFirmware Version     : 1.3.5-GA",
            "\tNAME: sw100nsner0op5osl6n85t7772j010jmhafm934n7oej4mlome3okrn9b60",
        ] {
            assert!(output.contains(expected), "missing {expected:?}");
        }
    }
}
