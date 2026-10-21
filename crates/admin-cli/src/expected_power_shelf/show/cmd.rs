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
use crate::{async_write, async_write_table_as_csv, async_writeln};

enum ShowResult {
    Single(ExpectedPowerShelf),
    List(ExpectedPowerShelfList),
}

enum RenderOutcome {
    Complete,
    TableRequired(ExpectedPowerShelfList),
}

// Keep the existing JSON and human-readable outputs unchanged. YAML uses the
// same protobuf-shaped objects as JSON, but must not expose BMC credentials.
fn redact_credentials(record: &mut ExpectedPowerShelf) {
    if !record.bmc_password.is_empty() {
        record.bmc_password = "***".to_string();
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
            Ok(RenderOutcome::TableRequired(ExpectedPowerShelfList {
                expected_power_shelves: vec![record],
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
            for record in &mut records.expected_power_shelves {
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
        output_format,
    )
    .await?;

    Ok(())
}

async fn convert_and_print_into_nice_table(
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    expected_power_shelves: &::rpc::forge::ExpectedPowerShelfList,
    expected_discovered_machine_ids: &HashMap<String, String>,
    expected_discovered_machine_interfaces: &HashMap<MacAddress, ::rpc::forge::MachineInterface>,
    output_format: OutputFormat,
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

    #[tokio::test]
    async fn yaml_output_is_one_redacted_document_for_single_list_and_empty() {
        let mut record = expected_power_shelf();
        record.bmc_password = "SYNTHETIC_BMC_SECRET".into();
        record.metadata = Some(rpc::forge::Metadata {
            name: "quoted \"name\"".into(),
            description: "line one\nline two".into(),
            ..Default::default()
        });
        let mut without_metadata = expected_power_shelf();
        without_metadata.bmc_mac_address = "00:11:22:33:44:66".into();
        for (result, is_single, is_empty) in [
            (ShowResult::Single(record.clone()), true, false),
            (
                ShowResult::List(ExpectedPowerShelfList {
                    expected_power_shelves: vec![record, without_metadata],
                }),
                false,
                false,
            ),
            (
                ShowResult::List(ExpectedPowerShelfList::default()),
                false,
                true,
            ),
        ] {
            let mut captured = CapturedOutput::new();
            let outcome = render_show_result(result, OutputFormat::Yaml, captured.writer())
                .await
                .unwrap();
            assert!(matches!(outcome, RenderOutcome::Complete));
            let output = captured.into_bytes().await;
            let text = std::str::from_utf8(&output).unwrap();
            assert!(!text.contains("SYNTHETIC_BMC_SECRET"));
            let yaml: Value = serde_yaml::from_str(text).unwrap();
            let item = if is_single {
                &yaml
            } else {
                &yaml["expected_power_shelves"][0]
            };
            if !is_empty {
                assert_eq!(item["shelf_serial_number"], "shelf-1");
                assert_eq!(item["bmc_password"], "***");
                assert_eq!(item["metadata"]["description"], "line one\nline two");
                if !is_single {
                    assert!(yaml["expected_power_shelves"][1]["metadata"].is_null());
                    assert_eq!(yaml["expected_power_shelves"][1]["bmc_password"], "");
                }
            } else {
                assert_eq!(yaml["expected_power_shelves"].as_array().unwrap().len(), 0);
            }
        }
    }

    #[tokio::test]
    async fn csv_output_has_one_header_and_escaped_rows() {
        let mut record = expected_power_shelf();
        record.bmc_password = "SYNTHETIC_BMC_SECRET".into();
        record.metadata = Some(rpc::forge::Metadata {
            name: "quoted \"name\"".into(),
            description: "line one,\nline two".into(),
            ..Default::default()
        });
        let mut without_metadata = expected_power_shelf();
        without_metadata.bmc_mac_address = "00:11:22:33:44:66".into();
        for (result, row_count) in [
            (ShowResult::Single(record.clone()), 1),
            (
                ShowResult::List(ExpectedPowerShelfList {
                    expected_power_shelves: vec![record, without_metadata],
                }),
                2,
            ),
            (ShowResult::List(ExpectedPowerShelfList::default()), 0),
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
            }
        }
    }
}
