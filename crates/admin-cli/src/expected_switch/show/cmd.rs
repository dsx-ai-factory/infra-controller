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

use mac_address::MacAddress;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{ExpectedSwitch, ExpectedSwitchList, ExpectedSwitchRequest, LinkedExpectedSwitch};

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

enum ShowResult {
    Single(ExpectedSwitch),
    List(ExpectedSwitchList),
}

enum RenderOutcome {
    Complete,
    TableRequired(ExpectedSwitchList),
}

async fn render_show_result(
    result: ShowResult,
    output_format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<RenderOutcome> {
    match result {
        ShowResult::Single(expected_switch) => {
            if output_format == OutputFormat::Json {
                async_writeln!(
                    output,
                    "{}",
                    serde_json::to_string_pretty(&expected_switch)?
                )?;
            } else {
                async_writeln!(output, "{:#?}", expected_switch)?;
            }
            Ok(RenderOutcome::Complete)
        }
        ShowResult::List(expected_switches) if output_format == OutputFormat::Json => {
            async_writeln!(
                output,
                "{}",
                serde_json::to_string_pretty(&expected_switches)?
            )?;
            Ok(RenderOutcome::Complete)
        }
        ShowResult::List(expected_switches) => Ok(RenderOutcome::TableRequired(expected_switches)),
    }
}

pub(super) async fn show(
    request: Option<ExpectedSwitchRequest>,
    api_client: &ApiClient,
    output_format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let result = if let Some(req) = request {
        ShowResult::Single(api_client.0.get_expected_switch(req).await?)
    } else {
        ShowResult::List(api_client.0.get_all_expected_switches().await?)
    };

    let expected_switches = match render_show_result(result, output_format, output).await? {
        RenderOutcome::Complete => return Ok(()),
        RenderOutcome::TableRequired(expected_switches) => expected_switches,
    };

    let linked_switches = api_client.0.get_all_expected_switches_linked().await?;
    let linked_by_bmc_mac: HashMap<String, LinkedExpectedSwitch> = linked_switches
        .expected_switches
        .into_iter()
        .map(|linked| (linked.bmc_mac_address.clone(), linked))
        .collect();

    let all_mi = api_client.get_all_machines_interfaces(None).await?;
    let expected_macs = expected_switches
        .expected_switches
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

    convert_and_print_into_nice_table(output, &expected_switches, &linked_by_bmc_mac, &expected_mi)
        .await?;

    Ok(())
}

fn format_interface_ip(
    machine_interface: Option<&::rpc::forge::MachineInterface>,
    linked: Option<&LinkedExpectedSwitch>,
) -> String {
    if let Some(mi) = machine_interface
        && !mi.address.is_empty()
    {
        return mi.address.join("\n");
    }

    if let Some(addr) = linked.and_then(|l| l.explored_endpoint_address.as_deref())
        && !addr.is_empty()
    {
        return addr.to_string();
    }

    "Undiscovered".to_string()
}

async fn convert_and_print_into_nice_table(
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    expected_switches: &::rpc::forge::ExpectedSwitchList,
    linked_by_bmc_mac: &HashMap<String, LinkedExpectedSwitch>,
    expected_discovered_machine_interfaces: &HashMap<MacAddress, ::rpc::forge::MachineInterface>,
) -> CarbideCliResult<()> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "Serial Number",
        "BMC Mac",
        "MAC addresses",
        "Interface IP",
        "Associated Switch",
        "Name",
        "Description",
        "Labels",
        "NVOS Username",
        "NVOS Password"
    ]);

    for expected_switch in &expected_switches.expected_switches {
        let linked = linked_by_bmc_mac.get(&expected_switch.bmc_mac_address);
        let machine_interface = expected_switch
            .bmc_mac_address
            .parse()
            .ok()
            .and_then(|mac| expected_discovered_machine_interfaces.get(&mac));

        let labels = crate::metadata::fmt_labels_as_kv_pairs(expected_switch.metadata.as_ref());
        let associated_switch = linked
            .and_then(|l| l.switch_id.as_ref())
            .map(|id| id.to_string())
            .unwrap_or_else(|| "Unlinked".to_string());

        table.add_row(row![
            expected_switch.switch_serial_number,
            expected_switch.bmc_mac_address,
            expected_switch.nvos_mac_addresses.join(", "),
            format_interface_ip(machine_interface, linked),
            associated_switch,
            expected_switch
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            expected_switch
                .metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
            labels.join(", "),
            expected_switch.nvos_username.as_deref().unwrap_or_default(),
            expected_switch
                .nvos_password
                .as_ref()
                .map(|_| "***")
                .unwrap_or_default()
        ]);
    }

    async_write!(output, "{}", table)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::async_write::CapturedOutput;

    fn expected_switch() -> ExpectedSwitch {
        ExpectedSwitch {
            bmc_mac_address: "00:11:22:33:44:55".to_string(),
            switch_serial_number: "switch-1".to_string(),
            ..Default::default()
        }
    }

    async fn render_json(result: ShowResult) -> (RenderOutcome, Value) {
        let mut captured = CapturedOutput::new();
        let outcome = render_show_result(result, OutputFormat::Json, captured.writer())
            .await
            .expect("JSON output should render");
        let output = captured.into_bytes().await;
        let json = serde_json::from_slice(&output)
            .expect("the complete captured output should be one JSON document");
        (outcome, json)
    }

    #[tokio::test]
    async fn single_json_output_is_complete_document() {
        let (outcome, json) = render_json(ShowResult::Single(expected_switch())).await;

        assert!(matches!(outcome, RenderOutcome::Complete));
        assert_eq!(json["switch_serial_number"], "switch-1");
    }

    #[tokio::test]
    async fn list_json_output_is_complete_document() {
        let list = ExpectedSwitchList {
            expected_switches: vec![expected_switch()],
        };
        let (outcome, json) = render_json(ShowResult::List(list)).await;

        assert!(matches!(outcome, RenderOutcome::Complete));
        assert_eq!(
            json["expected_switches"][0]["switch_serial_number"],
            "switch-1"
        );
    }
}
