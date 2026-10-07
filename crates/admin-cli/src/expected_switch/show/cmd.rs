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
use crate::{async_write, async_write_table_as_csv, async_writeln};

enum ShowResult {
    Single(ExpectedSwitch),
    List(ExpectedSwitchList),
}

enum RenderOutcome {
    Complete,
    TableRequired(ExpectedSwitchList),
}

// Keep the existing JSON and human-readable outputs unchanged. YAML uses the
// same protobuf-shaped objects as JSON, but must not expose BMC credentials.
fn redact_credentials(record: &mut ExpectedSwitch) {
    if !record.bmc_password.is_empty() {
        record.bmc_password = "***".to_string();
    }
    if let Some(password) = &mut record.nvos_password {
        *password = "***".to_string();
    }
}

async fn render_show_result(
    result: ShowResult,
    output_format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<RenderOutcome> {
    match (result, output_format) {
        (ShowResult::Single(record), OutputFormat::Json) => {
            async_writeln!(output, "{}", serde_json::to_string_pretty(&record)?)?;
            Ok(RenderOutcome::Complete)
        }
        (ShowResult::Single(mut record), OutputFormat::Yaml) => {
            redact_credentials(&mut record);
            async_write!(output, "{}", serde_yaml::to_string(&record)?)?;
            Ok(RenderOutcome::Complete)
        }
        (ShowResult::Single(record), OutputFormat::Csv) => {
            // Reuse the list's table projection (and its field escaping) for a
            // one-row CSV document. The caller fetches linked columns as usual.
            Ok(RenderOutcome::TableRequired(ExpectedSwitchList {
                expected_switches: vec![record],
            }))
        }
        (ShowResult::Single(record), OutputFormat::AsciiTable) => {
            async_writeln!(output, "{:#?}", record)?;
            Ok(RenderOutcome::Complete)
        }
        (ShowResult::List(records), OutputFormat::Json) => {
            async_writeln!(output, "{}", serde_json::to_string_pretty(&records)?)?;
            Ok(RenderOutcome::Complete)
        }
        (ShowResult::List(mut records), OutputFormat::Yaml) => {
            for record in &mut records.expected_switches {
                redact_credentials(record);
            }
            async_write!(output, "{}", serde_yaml::to_string(&records)?)?;
            Ok(RenderOutcome::Complete)
        }
        (ShowResult::List(records), OutputFormat::Csv | OutputFormat::AsciiTable) => {
            Ok(RenderOutcome::TableRequired(records))
        }
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

    convert_and_print_into_nice_table(
        output,
        &expected_switches,
        &linked_by_bmc_mac,
        &expected_mi,
        output_format,
    )
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
    output_format: OutputFormat,
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

    if output_format == OutputFormat::Csv {
        async_write_table_as_csv!(output, table)?;
    } else {
        async_write!(output, "{}", table)?;
    }

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

    #[tokio::test]
    async fn yaml_output_is_one_redacted_document_for_single_list_and_empty() {
        let mut record = expected_switch();
        record.bmc_password = "SYNTHETIC_BMC_SECRET".into();
        record.nvos_password = Some("SYNTHETIC_NVOS_SECRET".into());
        record.metadata = Some(rpc::forge::Metadata {
            name: "quoted \"name\"".into(),
            description: "line one\nline two".into(),
            ..Default::default()
        });
        let mut without_metadata = expected_switch();
        without_metadata.bmc_mac_address = "00:11:22:33:44:66".into();
        for (result, is_single, is_empty) in [
            (ShowResult::Single(record.clone()), true, false),
            (
                ShowResult::List(ExpectedSwitchList {
                    expected_switches: vec![record, without_metadata],
                }),
                false,
                false,
            ),
            (ShowResult::List(ExpectedSwitchList::default()), false, true),
        ] {
            let mut captured = CapturedOutput::new();
            let outcome = render_show_result(result, OutputFormat::Yaml, captured.writer())
                .await
                .unwrap();
            assert!(matches!(outcome, RenderOutcome::Complete));
            let output = captured.into_bytes().await;
            let text = std::str::from_utf8(&output).unwrap();
            assert!(!text.contains("SYNTHETIC_BMC_SECRET"));
            assert!(!text.contains("SYNTHETIC_NVOS_SECRET"));
            let yaml: Value = serde_yaml::from_str(text).unwrap();
            let item = if is_single {
                &yaml
            } else {
                &yaml["expected_switches"][0]
            };
            if !is_empty {
                assert_eq!(item["switch_serial_number"], "switch-1");
                assert_eq!(item["bmc_password"], "***");
                assert_eq!(item["metadata"]["description"], "line one\nline two");
                if !is_single {
                    assert!(yaml["expected_switches"][1]["metadata"].is_null());
                    assert_eq!(yaml["expected_switches"][1]["bmc_password"], "");
                    assert!(yaml["expected_switches"][1]["nvos_password"].is_null());
                }
                assert_eq!(item["nvos_password"], "***");
            } else {
                assert_eq!(yaml["expected_switches"].as_array().unwrap().len(), 0);
            }
        }
    }

    #[tokio::test]
    async fn csv_output_has_one_header_and_escaped_rows() {
        let mut record = expected_switch();
        record.bmc_password = "SYNTHETIC_BMC_SECRET".into();
        record.nvos_password = Some("SYNTHETIC_NVOS_SECRET".into());
        record.metadata = Some(rpc::forge::Metadata {
            name: "quoted \"name\"".into(),
            description: "line one,\nline two".into(),
            ..Default::default()
        });
        let mut without_metadata = expected_switch();
        without_metadata.bmc_mac_address = "00:11:22:33:44:66".into();
        for (result, row_count) in [
            (ShowResult::Single(record.clone()), 1),
            (
                ShowResult::List(ExpectedSwitchList {
                    expected_switches: vec![record, without_metadata],
                }),
                2,
            ),
            (ShowResult::List(ExpectedSwitchList::default()), 0),
        ] {
            let mut captured = CapturedOutput::new();
            let outcome = render_show_result(result, OutputFormat::Csv, captured.writer())
                .await
                .unwrap();
            let RenderOutcome::TableRequired(records) = outcome else {
                panic!("CSV must use the table projection");
            };
            convert_and_print_into_nice_table(
                captured.writer(),
                &records,
                &HashMap::new(),
                &HashMap::new(),
                OutputFormat::Csv,
            )
            .await
            .unwrap();
            let output = captured.into_bytes().await;
            let text = std::str::from_utf8(&output).unwrap();
            assert!(!text.contains("SYNTHETIC_BMC_SECRET"));
            assert!(!text.contains("SYNTHETIC_NVOS_SECRET"));
            let mut reader = csv::Reader::from_reader(output.as_slice());
            let headers = reader.headers().unwrap().clone();
            assert!(headers.iter().any(|header| header == "Description"));
            let rows = reader.records().collect::<Result<Vec<_>, _>>().unwrap();
            assert_eq!(rows.len(), row_count);
            if let Some(row) = rows.first() {
                assert_eq!(
                    row.get(
                        headers
                            .iter()
                            .position(|header| header == "Description")
                            .unwrap()
                    ),
                    Some("line one,\nline two")
                );
                assert_eq!(
                    row.get(headers.iter().position(|header| header == "Name").unwrap()),
                    Some("quoted \"name\"")
                );
                assert_eq!(
                    row.get(
                        headers
                            .iter()
                            .position(|header| header == "NVOS Password")
                            .unwrap()
                    ),
                    Some("***")
                );
            }
            if row_count == 2 {
                assert_eq!(
                    rows[1].get(headers.iter().position(|header| header == "Name").unwrap()),
                    Some("")
                );
                assert_eq!(
                    rows[1].get(
                        headers
                            .iter()
                            .position(|header| header == "Labels")
                            .unwrap()
                    ),
                    Some("")
                );
                assert_eq!(
                    rows[1].get(
                        headers
                            .iter()
                            .position(|header| header == "NVOS Password")
                            .unwrap()
                    ),
                    Some("")
                );
            }
        }
    }
}
