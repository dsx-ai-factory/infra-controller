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

use mac_address::MacAddress;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{ExpectedPowerShelf, ExpectedPowerShelfList, ExpectedPowerShelfRequest};

use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

enum ShowResult {
    Single(ExpectedPowerShelf),
    List(ExpectedPowerShelfList),
}

enum RenderOutcome {
    Complete,
    TableRequired(ExpectedPowerShelfList),
}

async fn render_show_result(
    result: ShowResult,
    output_format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<RenderOutcome> {
    match result {
        ShowResult::Single(expected_power_shelf) => {
            if output_format == OutputFormat::Json {
                async_writeln!(
                    output,
                    "{}",
                    serde_json::to_string_pretty(&expected_power_shelf)?
                )?;
            } else {
                async_writeln!(output, "{:#?}", expected_power_shelf)?;
            }
            Ok(RenderOutcome::Complete)
        }
        ShowResult::List(expected_power_shelves) if output_format == OutputFormat::Json => {
            async_writeln!(
                output,
                "{}",
                serde_json::to_string_pretty(&expected_power_shelves)?
            )?;
            Ok(RenderOutcome::Complete)
        }
        ShowResult::List(expected_power_shelves) => {
            Ok(RenderOutcome::TableRequired(expected_power_shelves))
        }
    }
}

pub(super) async fn show(
    request: Option<ExpectedPowerShelfRequest>,
    api_client: &ApiClient,
    output_format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let result = if let Some(req) = request {
        ShowResult::Single(api_client.0.get_expected_power_shelf(req).await?)
    } else {
        ShowResult::List(api_client.0.get_all_expected_power_shelves().await?)
    };

    let expected_power_shelves = match render_show_result(result, output_format, output).await? {
        RenderOutcome::Complete => return Ok(()),
        RenderOutcome::TableRequired(expected_power_shelves) => expected_power_shelves,
    };

    // TODO: This should be optimised. `find_interfaces` should accept a list of macs also and
    // return related interfaces details.
    let all_mi = api_client.get_all_machines_interfaces(None).await?;
    let expected_macs = expected_power_shelves
        .expected_power_shelves
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
        .filter_map(|iface| iface.address.first())
        .cloned()
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
        &expected_power_shelves,
        &expected_bmc_ip_vs_ids,
        &expected_mi,
    )
    .await?;

    Ok(())
}

async fn convert_and_print_into_nice_table(
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    expected_power_shelves: &::rpc::forge::ExpectedPowerShelfList,
    expected_discovered_machine_ids: &HashMap<String, String>,
    expected_discovered_machine_interfaces: &HashMap<MacAddress, ::rpc::forge::MachineInterface>,
) -> CarbideCliResult<()> {
    let mut table = Box::new(Table::new());

    table.set_titles(row![
        "Serial Number",
        "BMC Mac",
        "Interface IP",
        "Associated Machine",
        "Name",
        "Description",
        "Labels"
    ]);

    for expected_power_shelf in &expected_power_shelves.expected_power_shelves {
        let Ok(bmc_mac_address) = expected_power_shelf.bmc_mac_address.parse() else {
            continue;
        };
        let machine_interface = expected_discovered_machine_interfaces.get(&bmc_mac_address);
        let machine_id = expected_discovered_machine_ids
            .get(
                machine_interface
                    .and_then(|x| x.address.first().map(String::as_str))
                    .unwrap_or("unknown"),
            )
            .map(String::as_str);

        let labels =
            crate::metadata::fmt_labels_as_kv_pairs(expected_power_shelf.metadata.as_ref());

        table.add_row(row![
            expected_power_shelf.shelf_serial_number,
            expected_power_shelf.bmc_mac_address,
            machine_interface
                .map(|x| Cow::Owned(x.address.join("\n")))
                .unwrap_or(Cow::Borrowed("Undiscovered"))
                .as_ref(),
            machine_id.unwrap_or("Unlinked"),
            expected_power_shelf
                .metadata
                .as_ref()
                .map(|m| m.name.as_str())
                .unwrap_or_default(),
            expected_power_shelf
                .metadata
                .as_ref()
                .map(|m| m.description.as_str())
                .unwrap_or_default(),
            labels.join(", ")
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

    fn expected_power_shelf() -> ExpectedPowerShelf {
        ExpectedPowerShelf {
            bmc_mac_address: "00:11:22:33:44:55".to_string(),
            shelf_serial_number: "shelf-1".to_string(),
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
        let (outcome, json) = render_json(ShowResult::Single(expected_power_shelf())).await;

        assert!(matches!(outcome, RenderOutcome::Complete));
        assert_eq!(json["shelf_serial_number"], "shelf-1");
    }

    #[tokio::test]
    async fn list_json_output_is_complete_document() {
        let list = ExpectedPowerShelfList {
            expected_power_shelves: vec![expected_power_shelf()],
        };
        let (outcome, json) = render_json(ShowResult::List(list)).await;

        assert!(matches!(outcome, RenderOutcome::Complete));
        assert_eq!(
            json["expected_power_shelves"][0]["shelf_serial_number"],
            "shelf-1"
        );
    }
}
