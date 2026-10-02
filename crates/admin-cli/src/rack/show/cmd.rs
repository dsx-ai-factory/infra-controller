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

use std::io::Write;

use carbide_uuid::rack::RackId;
use color_eyre::Result;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::errors::RpcDataConversionError;
use rpc::forge::{MachineSearchConfig, PowerShelfSearchFilter, Rack, SwitchSearchFilter};
use serde::Serialize;

use super::args::Args;
use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

#[derive(Serialize)]
struct RackOutput {
    id: String,
    name: String,
    state: String,
    version: String,
    current_compute_trays: Vec<String>,
    current_power_shelves: Vec<String>,
    current_nvlink_switches: Vec<String>,
}

impl From<&Rack> for RackOutput {
    fn from(r: &Rack) -> Self {
        Self {
            id: r.id.as_ref().map(|id| id.to_string()).unwrap_or_default(),
            name: r
                .metadata
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_default(),
            state: r.rack_state.clone(),
            version: r.version.clone(),
            current_compute_trays: vec![],
            current_power_shelves: vec![],
            current_nvlink_switches: vec![],
        }
    }
}

/// Gets the compute trays associated with a rack.
async fn get_compute_trays(api_client: &ApiClient, rack_id: &RackId) -> Result<Vec<String>> {
    // Use a MachineSearchConfig with the RackId to get a Vec<MachineId>.
    let request = MachineSearchConfig {
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    };
    let machine_ids = api_client.0.find_machine_ids(request).await?.machine_ids;

    // Convert these to a vector of Strings and return them in a Result.
    let compute_trays = machine_ids.iter().map(|id| id.to_string()).collect();
    Ok(compute_trays)
}

/// Gets the power shelves associated with a rack.
async fn get_power_shelves(api_client: &ApiClient, rack_id: &RackId) -> Result<Vec<String>> {
    // Use a PowerShelfSearchFilter with the RackId to get a Vec<PowerShelfId>.
    let request = PowerShelfSearchFilter {
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    };
    let power_shelf_ids = api_client.0.find_power_shelf_ids(request).await?.ids;

    // Convert these to a vector of Strings and return them in a Result.
    let power_shelves = power_shelf_ids.iter().map(|id| id.to_string()).collect();
    Ok(power_shelves)
}

/// Gets the switches associated with a rack.
async fn get_nvlink_switches(api_client: &ApiClient, rack_id: &RackId) -> Result<Vec<String>> {
    // Use a SwitchSearchFilter with the RackId to get a Vec<SwitchId>.
    let request = SwitchSearchFilter {
        rack_id: Some(rack_id.clone()),
        ..Default::default()
    };
    let switch_ids = api_client.0.find_switch_ids(request).await?.ids;

    // Convert these to a vector of Strings and return them in a Result.
    let switches = switch_ids.iter().map(|id| id.to_string()).collect();
    Ok(switches)
}

/// Takes a list of Racks and returns a list of RackOutputs.
/// Since limited information is available from the Rack object, we need additional API calls
/// to get full details like compute trays, power shelves, and nvlink switches.
async fn get_rack_outputs(api_client: &ApiClient, racks: &Vec<Rack>) -> Result<Vec<RackOutput>> {
    let mut outputs: Vec<RackOutput> = Vec::new();
    for rack in racks {
        let rack_id = rack
            .id
            .as_ref()
            .ok_or(RpcDataConversionError::MissingArgument("rack.id"))?
            .clone();
        let compute_trays = get_compute_trays(api_client, &rack_id).await?;
        let power_shelves = get_power_shelves(api_client, &rack_id).await?;
        let nvlink_switches = get_nvlink_switches(api_client, &rack_id).await?;
        let mut output = RackOutput::from(rack);
        output.current_compute_trays = compute_trays;
        output.current_power_shelves = power_shelves;
        output.current_nvlink_switches = nvlink_switches;
        outputs.push(output);
    }
    Ok(outputs)
}

pub(super) async fn show_rack(
    api_client: &ApiClient,
    args: Args,
    config: &RuntimeConfig,
) -> Result<()> {
    let format = config.format;
    match args.rack {
        Some(rack_id) => {
            let racks = api_client.get_one_rack(rack_id).await?.racks;
            let outputs = get_rack_outputs(api_client, &racks).await?;
            match outputs.first() {
                Some(output) => show_single(output, format, &mut std::io::stdout())?,
                None => println!("No rack found"),
            }
        }
        None => {
            let racks = api_client.get_all_racks(config.page_size).await?.racks;
            if racks.is_empty() {
                println!("No racks found");
            } else {
                let outputs = get_rack_outputs(api_client, &racks).await?;
                show_list(&outputs, format, &mut std::io::stdout())?;
            }
        }
    }

    Ok(())
}

fn show_single(output: &RackOutput, format: OutputFormat, writer: &mut impl Write) -> Result<()> {
    match format {
        OutputFormat::Json => writeln!(writer, "{}", serde_json::to_string_pretty(output)?)?,
        OutputFormat::Yaml => writeln!(writer, "{}", serde_yaml::to_string(output)?)?,
        _ => show_detail(output, writer)?,
    }
    Ok(())
}

fn show_list(outputs: &[RackOutput], format: OutputFormat, writer: &mut impl Write) -> Result<()> {
    match format {
        OutputFormat::Json => writeln!(writer, "{}", serde_json::to_string_pretty(outputs)?)?,
        OutputFormat::Yaml => writeln!(writer, "{}", serde_yaml::to_string(outputs)?)?,
        OutputFormat::Csv => show_table_csv(outputs, writer)?,
        _ => show_table(outputs, writer)?,
    }
    Ok(())
}

fn show_detail(output: &RackOutput, writer: &mut impl Write) -> Result<()> {
    let mut table = Table::new();
    table.add_row(row!["ID", output.id]);
    table.add_row(row!["Name", output.name]);
    table.add_row(row!["State", output.state]);
    table.add_row(row!["Version", output.version]);
    table.add_row(row![
        "Current Compute Trays",
        if output.current_compute_trays.is_empty() {
            "N/A".to_string()
        } else {
            output
                .current_compute_trays
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        }
    ]);
    table.add_row(row![
        "Current Power Shelves",
        if output.current_power_shelves.is_empty() {
            "N/A".to_string()
        } else {
            output
                .current_power_shelves
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        }
    ]);
    table.add_row(row![
        "Current NVLink Switches",
        if output.current_nvlink_switches.is_empty() {
            "N/A".to_string()
        } else {
            output
                .current_nvlink_switches
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        }
    ]);
    table.print(writer)?;
    Ok(())
}

fn show_table(outputs: &[RackOutput], writer: &mut impl Write) -> Result<()> {
    let mut table = Table::new();
    table.set_titles(row![
        "ID",
        "Name",
        "State",
        "Compute Trays",
        "Power Shelves",
        "Switches",
    ]);

    for output in outputs {
        table.add_row(row![
            output.id,
            output.name,
            output.state,
            format!("{}", output.current_compute_trays.len(),),
            format!("{}", output.current_power_shelves.len(),),
            format!("{}", output.current_nvlink_switches.len(),),
        ]);
    }

    table.print(writer)?;
    Ok(())
}

fn show_table_csv(outputs: &[RackOutput], writer: &mut impl Write) -> Result<()> {
    let mut table = Table::new();
    table.set_titles(row![
        "ID",
        "Name",
        "State",
        "Compute Trays",
        "Power Shelves",
        "Switches",
    ]);

    for output in outputs {
        table.add_row(row![
            output.id,
            output.name,
            output.state,
            format!("{}", output.current_compute_trays.len(),),
            format!("{}", output.current_power_shelves.len(),),
            format!("{}", output.current_nvlink_switches.len(),),
        ]);
    }

    table.to_csv(writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};
    use rpc::admin_cli::OutputFormat;
    use rpc::forge::{Metadata, Rack};

    use super::*;

    fn make_rack(id: &str, state: &str, name: &str, version: &str) -> Rack {
        Rack {
            id: Some(id.parse().unwrap()),
            rack_state: state.to_string(),
            version: version.to_string(),
            metadata: Some(Metadata {
                name: name.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn make_output(
        id: &str,
        name: &str,
        state: &str,
        version: &str,
        compute_trays: Vec<&str>,
        power_shelves: Vec<&str>,
        nvlink_switches: Vec<&str>,
    ) -> RackOutput {
        RackOutput {
            id: id.to_string(),
            name: name.to_string(),
            state: state.to_string(),
            version: version.to_string(),
            current_compute_trays: compute_trays.into_iter().map(str::to_string).collect(),
            current_power_shelves: power_shelves.into_iter().map(str::to_string).collect(),
            current_nvlink_switches: nvlink_switches.into_iter().map(str::to_string).collect(),
        }
    }

    /////////////////////////////////////////////////////////////////////////////
    // From<&Rack> for RackOutput

    /// RackOutput maps basic fields from Rack; component lists start empty.
    #[test]
    fn rack_output_maps_fields_from_rack() {
        let rack = make_rack("Rack1", "Created", "NVL72", "V1-T1777407111818648");
        let output = RackOutput::from(&rack);
        assert_eq!(output.id, "Rack1");
        assert_eq!(output.name, "NVL72");
        assert_eq!(output.state, "Created");
        assert_eq!(output.version, "V1-T1777407111818648");
        assert!(output.current_compute_trays.is_empty());
        assert!(output.current_power_shelves.is_empty());
        assert!(output.current_nvlink_switches.is_empty());
    }

    // A missing Rack component falls back to an empty string in RackOutput:
    // no ID yields an empty id, no metadata yields an empty name. The run
    // closure converts the Rack and reads back the field named by the case.
    #[test]
    fn rack_output_defaults_when_fields_missing() {
        check_cases(
            [
                Case {
                    scenario: "missing id falls back to empty string",
                    input: Rack {
                        id: None,
                        rack_state: "Created".to_string(),
                        ..Default::default()
                    },
                    expect: Yields(String::new()),
                },
                Case {
                    scenario: "missing metadata falls back to empty name",
                    input: Rack {
                        id: Some("Rack1".parse().unwrap()),
                        metadata: None,
                        ..Default::default()
                    },
                    expect: Yields(String::new()),
                },
            ],
            // Read the field that the missing component would have populated:
            // id for the no-id case, name for the no-metadata case.
            |rack: Rack| -> Result<String, ()> {
                let output = RackOutput::from(&rack);
                Ok(if rack.id.is_none() {
                    output.id
                } else {
                    output.name
                })
            },
        );
    }

    /////////////////////////////////////////////////////////////////////////////
    // JSON / YAML serialization shape

    /// RackOutput serializes to JSON with the expected field names and no
    /// 'expected_*' fields (regression guard against re-introducing removed fields).
    #[test]
    fn rack_output_json_serializes_expected_fields() {
        let output = make_output(
            "Rack1",
            "NVL72",
            "Created",
            "V1-T1777407111818648",
            vec![],
            vec![],
            vec![],
        );
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&output).unwrap()).unwrap();

        assert_eq!(json["id"], "Rack1");
        assert_eq!(json["name"], "NVL72");
        assert_eq!(json["state"], "Created");
        assert_eq!(json["version"], "V1-T1777407111818648");
        assert_eq!(json["current_compute_trays"], serde_json::json!([]));
        assert_eq!(json["current_power_shelves"], serde_json::json!([]));
        assert_eq!(json["current_nvlink_switches"], serde_json::json!([]));

        assert!(json.get("expected_compute_tray_bmcs").is_none());
        assert!(json.get("expected_power_shelf_bmcs").is_none());
        assert!(json.get("expected_nvlink_switch_bmcs").is_none());
    }

    /// RackOutput serializes to YAML with the expected field names and no
    /// 'expected_*' fields (regression guard against re-introducing removed fields).
    #[test]
    fn rack_output_yaml_serializes_expected_fields() {
        let output = make_output("Rack1", "NVL72", "Created", "V1", vec![], vec![], vec![]);
        let yaml = serde_yaml::to_string(&output).unwrap();

        assert!(yaml.contains("id:"));
        assert!(yaml.contains("name:"));
        assert!(yaml.contains("state:"));
        assert!(yaml.contains("version:"));
        assert!(yaml.contains("current_compute_trays:"));
        assert!(yaml.contains("current_power_shelves:"));
        assert!(yaml.contains("current_nvlink_switches:"));

        assert!(!yaml.contains("expected_compute_tray_bmcs"));
        assert!(!yaml.contains("expected_power_shelf_bmcs"));
        assert!(!yaml.contains("expected_nvlink_switch_bmcs"));
    }

    /////////////////////////////////////////////////////////////////////////////
    // Rendering functions

    // The structured renderers return Ok for both formats: show_single and
    // show_list each serialize cleanly as JSON and as YAML. Each row's input
    // is a thunk that builds the fixture and runs one renderer at one format.
    #[test]
    fn structured_renderers_return_ok() {
        type RenderCase = Case<fn() -> Result<()>, (), ()>;
        let cases: [RenderCase; 4] = [
            Case {
                scenario: "show_single renders JSON",
                input: || {
                    let output =
                        make_output("Rack1", "NVL72", "Created", "V1", vec![], vec![], vec![]);
                    show_single(&output, OutputFormat::Json, &mut Vec::new())
                },
                expect: Yields(()),
            },
            Case {
                scenario: "show_single renders YAML",
                input: || {
                    let output =
                        make_output("Rack1", "NVL72", "Created", "V1", vec![], vec![], vec![]);
                    show_single(&output, OutputFormat::Yaml, &mut Vec::new())
                },
                expect: Yields(()),
            },
            Case {
                scenario: "show_list renders JSON",
                input: || {
                    let outputs = vec![
                        make_output("Rack1", "NVL72", "Created", "V1", vec![], vec![], vec![]),
                        make_output(
                            "Rack2",
                            "NVL36",
                            "Provisioned",
                            "V2",
                            vec!["t-1"],
                            vec![],
                            vec![],
                        ),
                    ];
                    show_list(&outputs, OutputFormat::Json, &mut Vec::new())
                },
                expect: Yields(()),
            },
            Case {
                scenario: "show_list renders YAML",
                input: || {
                    let outputs = vec![make_output(
                        "Rack1",
                        "NVL72",
                        "Created",
                        "V1",
                        vec![],
                        vec![],
                        vec![],
                    )];
                    show_list(&outputs, OutputFormat::Yaml, &mut Vec::new())
                },
                expect: Yields(()),
            },
        ];
        check_cases(cases, |run| run().map_err(drop));
    }

    /// show_detail with all-empty component lists renders "N/A" paths.
    #[test]
    fn show_detail_with_empty_components_renders_na() {
        let output = make_output("Rack1", "NVL72", "Created", "V1", vec![], vec![], vec![]);
        let mut rendered = Vec::new();
        show_detail(&output, &mut rendered).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();

        assert_eq!(rendered.matches("N/A").count(), 3);
    }

    /// show_detail with populated component lists renders every component.
    #[test]
    fn show_detail_with_populated_components_renders_components() {
        let output = make_output(
            "Rack1",
            "NVL72",
            "Created",
            "V1",
            vec!["tray-a", "tray-b"],
            vec!["shelf-1"],
            vec!["switch-x"],
        );
        let mut rendered = Vec::new();
        show_detail(&output, &mut rendered).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();

        for component in ["tray-a", "tray-b", "shelf-1", "switch-x"] {
            assert!(rendered.contains(component), "missing {component}");
        }
    }

    /// show_table renders component counts for multiple outputs.
    #[test]
    fn show_table_renders_component_counts() {
        let outputs = vec![
            make_output(
                "Rack1",
                "NVL72",
                "Created",
                "V1",
                vec!["t-1", "t-2"],
                vec!["s-1"],
                vec![],
            ),
            make_output(
                "Rack2",
                "NVL36",
                "Provisioned",
                "V2",
                vec![],
                vec![],
                vec!["sw-1"],
            ),
        ];
        let mut rendered = Vec::new();
        show_table(&outputs, &mut rendered).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();

        assert!(rendered.contains("| Rack1 | NVL72 | Created     | 2"));
        assert!(rendered.contains("| Rack2 | NVL36 | Provisioned | 0"));
    }

    /// show_table_csv renders headers and component counts.
    #[test]
    fn show_table_csv_renders_component_counts() {
        let outputs = vec![make_output(
            "Rack1",
            "NVL72",
            "Created",
            "V1",
            vec!["t-1"],
            vec![],
            vec![],
        )];
        let mut rendered = Vec::new();
        show_table_csv(&outputs, &mut rendered).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();

        assert!(rendered.contains("ID,Name,State,Compute Trays,Power Shelves,Switches"));
        assert!(rendered.contains("Rack1,NVL72,Created,1,0,0"));
    }
}
