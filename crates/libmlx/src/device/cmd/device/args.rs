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

use clap::{Args, Subcommand, ValueEnum};

use crate::device::filters::{DeviceField, DeviceFilter, DeviceFilterSet, MatchMode};

// DeviceArgs represents the arguments for device-related commands.
#[derive(Args)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub action: DeviceAction,
}

// DeviceAction defines the available device subcommands.
#[derive(Subcommand, Clone)]
pub enum DeviceAction {
    // List all discovered Mellanox devices.
    #[command(about = "List all discovered Mellanox devices on this machine.")]
    List {
        // format specifies the output format for device information.
        #[arg(long, default_value = "ascii-table")]
        format: OutputFormat,

        // detailed shows detailed device information.
        #[arg(long)]
        detailed: bool,
    },

    // Filter devices using advanced filter expressions.
    #[command(about = "Filter devices based on DeviceFilter options.")]
    Filter {
        // format specifies the output format for device information.
        #[arg(long, default_value = "ascii-table")]
        format: OutputFormat,

        // filter specifies filter expression in the format field:value:match_mode.
        // Examples:
        //   --filter device_type:ConnectX-6:prefix
        //   --filter part_number:MCX.*:regex
        //   --filter firmware_version:22.32.1010:exact
        #[arg(long)]
        filter: Vec<DeviceFilter>,

        // detailed shows detailed device information.
        #[arg(long)]
        detailed: bool,
    },

    // Describe detailed information about a specific device.
    #[command(about = "Show everything known about a device by its ID.")]
    Describe {
        // device specifies the PCI address or identifier of the target device.
        device: String,
        // format specifies the output format for device information.
        #[arg(long, default_value = "ascii-table")]
        format: OutputFormat,
    },

    // Generate a complete device discovery report.
    #[command(
        about = "Generate an MlxDeviceReport in a given --format and optional --filter args."
    )]
    Report {
        // format specifies the output format for the report.
        #[arg(long, default_value = "ascii-table")]
        format: OutputFormat,

        // filter specifies filter expression in the format field:value:match_mode.
        // Examples:
        //   --filter device_type:ConnectX-6:prefix
        //   --filter part_number:MCX.*:regex
        //   --filter firmware_version:22.32.1010:exact
        #[arg(long)]
        filter: Vec<DeviceFilter>,

        // detailed shows detailed device information.
        #[arg(long)]
        detailed: bool,
    },
}

// OutputFormat defines the available output formats for device information.
#[derive(Clone, Debug, ValueEnum)]
pub enum OutputFormat {
    // ascii-table outputs device information in a formatted ASCII table.
    #[value(name = "ascii-table")]
    AsciiTable,
    // json outputs device information in JSON format.
    #[value(name = "json")]
    Json,
    // yaml outputs device information in YAML format.
    #[value(name = "yaml")]
    Yaml,
}

// parse_filter_expression parses a filter expression in the format field:value:match_mode.
// Values can be comma-separated for OR logic: field:value1,value2,value3:match_mode
pub fn parse_filter_expression(expression: &str) -> Result<DeviceFilter, String> {
    let parts: Vec<&str> = expression.split(':').collect();

    if parts.len() < 2 || parts.len() > 3 {
        return Err(format!(
            "Invalid filter expression '{expression}'. Expected format: field:value[,value2,value3] or field:value[,value2,value3]:match_mode"
        ));
    }

    let field = parse_device_field(parts[0])?;

    // Parse comma-separated values for OR logic.
    let values: Vec<String> = parts[1]
        .split(',')
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .collect();

    if values.is_empty() {
        return Err(format!(
            "No valid values found in filter expression '{expression}'"
        ));
    }

    let match_mode = if parts.len() == 3 {
        parse_match_mode(parts[2])?
    } else {
        // Use regex as default for all fields.
        MatchMode::Regex
    };

    Ok(DeviceFilter {
        field,
        values,
        match_mode,
    })
}

// parse_device_field converts a string to a DeviceField enum.
fn parse_device_field(field_str: &str) -> Result<DeviceField, String> {
    match field_str.to_lowercase().as_str() {
        "device_type" | "type" => Ok(DeviceField::DeviceType),
        "part_number" | "part" => Ok(DeviceField::PartNumber),
        "firmware_version" | "firmware" | "fw" => Ok(DeviceField::FirmwareVersion),
        "mac_address" | "mac" => Ok(DeviceField::MacAddress),
        "description" | "desc" => Ok(DeviceField::Description),
        "pci_name" | "pci" => Ok(DeviceField::PciName),
        "status" => Ok(DeviceField::Status),
        _ => Err(format!(
            "Unknown field '{field_str}'. Valid fields: device_type, part_number, firmware_version, mac_address, description, pci_name, status"
        )),
    }
}

// parse_match_mode converts a string to a MatchMode enum.
fn parse_match_mode(mode_str: &str) -> Result<MatchMode, String> {
    match mode_str.to_lowercase().as_str() {
        "regex" => Ok(MatchMode::Regex),
        "exact" => Ok(MatchMode::Exact),
        "prefix" => Ok(MatchMode::Prefix),
        _ => Err(format!(
            "Unknown match mode '{mode_str}'. Valid modes: regex, exact, prefix"
        )),
    }
}

// build_filter_set_from_filter_args creates a DeviceFilterSet from filter command arguments.
pub fn build_filter_set_from_filter_args(
    filter_expressions: Vec<String>,
) -> Result<DeviceFilterSet, String> {
    let mut filter_set = DeviceFilterSet::new();

    for expression in filter_expressions {
        let filter = parse_filter_expression(&expression)?;
        filter_set.add_filter(filter);
    }

    Ok(filter_set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_filter_from_str_integration() {
        use std::str::FromStr;

        let filter = DeviceFilter::from_str("device_type:ConnectX-6:prefix").unwrap();
        assert_eq!(
            filter.field,
            crate::device::filters::DeviceField::DeviceType
        );
        assert_eq!(filter.values, vec!["ConnectX-6"]);
        assert!(matches!(
            filter.match_mode,
            crate::device::filters::MatchMode::Prefix
        ));
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;

    // Projection of a DeviceFilter into PartialEq pieces, since DeviceFilter
    // itself is not PartialEq. (field, values, match_mode) captures every
    // observable output of the parsers under test.
    type FilterParts = (DeviceField, Vec<String>, MatchMode);

    fn parts(f: &DeviceFilter) -> FilterParts {
        (f.field.clone(), f.values.clone(), f.match_mode.clone())
    }

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    // parse_device_field: every accepted alias maps to its DeviceField arm,
    // case-insensitively, and any unknown token is rejected.
    #[test]
    fn parse_device_field_cases() {
        scenarios!(
            run = parse_device_field;
            "device_type canonical" {
                "device_type" => Yields(DeviceField::DeviceType),
            }

            "type alias" {
                "type" => Yields(DeviceField::DeviceType),
            }

            "device_type uppercased (lowercased internally)" {
                "DEVICE_TYPE" => Yields(DeviceField::DeviceType),
            }

            "part_number canonical" {
                "part_number" => Yields(DeviceField::PartNumber),
            }

            "part alias" {
                "part" => Yields(DeviceField::PartNumber),
            }

            "firmware_version canonical" {
                "firmware_version" => Yields(DeviceField::FirmwareVersion),
            }

            "firmware alias" {
                "firmware" => Yields(DeviceField::FirmwareVersion),
            }

            "fw alias" {
                "fw" => Yields(DeviceField::FirmwareVersion),
            }

            "mac_address canonical" {
                "mac_address" => Yields(DeviceField::MacAddress),
            }

            "mac alias" {
                "mac" => Yields(DeviceField::MacAddress),
            }

            "description canonical" {
                "description" => Yields(DeviceField::Description),
            }

            "desc alias" {
                "desc" => Yields(DeviceField::Description),
            }

            "pci_name canonical" {
                "pci_name" => Yields(DeviceField::PciName),
            }

            "pci alias" {
                "pci" => Yields(DeviceField::PciName),
            }

            "status canonical" {
                "status" => Yields(DeviceField::Status),
            }

            "unknown field rejected" {
                "bogus" => FailsWith(
                    "Unknown field 'bogus'. Valid fields: device_type, part_number, \
                     firmware_version, mac_address, description, pci_name, status"
                        .to_string(),
                ),
            }

            "empty field rejected" {
                "" => FailsWith(
                    "Unknown field ''. Valid fields: device_type, part_number, \
                     firmware_version, mac_address, description, pci_name, status"
                        .to_string(),
                ),
            }
        );
    }

    // parse_match_mode: each accepted mode (case-insensitive) maps to its
    // MatchMode arm; anything else is rejected with the canonical message.
    #[test]
    fn parse_match_mode_cases() {
        scenarios!(
            run = parse_match_mode;
            "regex" {
                "regex" => Yields(MatchMode::Regex),
            }

            "exact" {
                "exact" => Yields(MatchMode::Exact),
            }

            "prefix" {
                "prefix" => Yields(MatchMode::Prefix),
            }

            "uppercased prefix (lowercased internally)" {
                "PREFIX" => Yields(MatchMode::Prefix),
            }

            "unknown mode rejected" {
                "fuzzy" => FailsWith(
                    "Unknown match mode 'fuzzy'. Valid modes: regex, exact, prefix".to_string(),
                ),
            }

            "empty mode rejected" {
                "" => FailsWith(
                    "Unknown match mode ''. Valid modes: regex, exact, prefix".to_string(),
                ),
            }
        );
    }

    // parse_filter_expression success paths: 2-part defaults to Regex,
    // 3-part honors the explicit mode, comma-separated values become an OR
    // list, and surrounding whitespace on values is trimmed while empties drop.
    #[test]
    fn parse_filter_expression_ok_cases() {
        check_cases(
            [
                Case {
                    scenario: "two parts default to regex",
                    input: "device_type:ConnectX-6",
                    expect: Yields((
                        DeviceField::DeviceType,
                        owned(&["ConnectX-6"]),
                        MatchMode::Regex,
                    )),
                },
                Case {
                    scenario: "three parts with explicit prefix mode",
                    input: "part_number:MCX:prefix",
                    expect: Yields((DeviceField::PartNumber, owned(&["MCX"]), MatchMode::Prefix)),
                },
                Case {
                    scenario: "comma-separated OR values",
                    input: "status:ok,fail,warn:exact",
                    expect: Yields((
                        DeviceField::Status,
                        owned(&["ok", "fail", "warn"]),
                        MatchMode::Exact,
                    )),
                },
                Case {
                    scenario: "values are trimmed and empties dropped",
                    input: "fw: 22.32 , ,1010 :exact",
                    expect: Yields((
                        DeviceField::FirmwareVersion,
                        owned(&["22.32", "1010"]),
                        MatchMode::Exact,
                    )),
                },
                Case {
                    scenario: "alias field with default mode",
                    input: "mac:00:11",
                    // Note: three colon-parts here -> third part parsed as mode.
                    // "00:11" splits to ["mac","00","11"]; "11" is not a mode.
                    expect: Fails,
                },
            ],
            |expr| parse_filter_expression(expr).map(|f| parts(&f)),
        );
    }

    // parse_filter_expression rejection paths: too few / too many colon parts,
    // unknown field propagated, unknown mode propagated, and a values list
    // that collapses to empty after trimming.
    #[test]
    fn parse_filter_expression_err_cases() {
        scenarios!(
            run = |expr| parse_filter_expression(expr).map(|f| parts(&f));
            "single part (no colon) rejected" {
                "device_type" => FailsWith(
                    "Invalid filter expression 'device_type'. Expected format: \
                     field:value[,value2,value3] or \
                     field:value[,value2,value3]:match_mode"
                        .to_string(),
                ),
            }

            "four parts rejected" {
                "a:b:c:d" => FailsWith(
                    "Invalid filter expression 'a:b:c:d'. Expected format: \
                     field:value[,value2,value3] or \
                     field:value[,value2,value3]:match_mode"
                        .to_string(),
                ),
            }

            "unknown field propagated" {
                "nope:value" => FailsWith(
                    "Unknown field 'nope'. Valid fields: device_type, part_number, \
                     firmware_version, mac_address, description, pci_name, status"
                        .to_string(),
                ),
            }

            "unknown mode propagated" {
                "device_type:val:bogus" => FailsWith(
                    "Unknown match mode 'bogus'. Valid modes: regex, exact, prefix".to_string(),
                ),
            }

            "values empty after trim/drop" {
                "device_type: , :exact" => FailsWith(
                    "No valid values found in filter expression 'device_type: , :exact'"
                        .to_string(),
                ),
            }
        );
    }

    // build_filter_set_from_filter_args: empty input yields an empty set;
    // multiple valid expressions accumulate in order; any single invalid
    // expression aborts the whole build.
    #[test]
    fn build_filter_set_ok_cases() {
        scenarios!(
            run = |exprs| {
                build_filter_set_from_filter_args(exprs)
                    .map(|set| set.filters.iter().map(parts).collect::<Vec<_>>())
            };
            "empty input -> empty set" {
                vec![] => Yields(vec![]),
            }

            "single valid expression" {
                vec!["device_type:ConnectX:exact".to_string()] => Yields(vec![(
                    DeviceField::DeviceType,
                    owned(&["ConnectX"]),
                    MatchMode::Exact,
                )]),
            }

            "multiple expressions accumulate in order" {
                vec!["part:MCX:prefix".to_string(), "status:ok".to_string()] => Yields(vec![
                    (DeviceField::PartNumber, owned(&["MCX"]), MatchMode::Prefix),
                    (DeviceField::Status, owned(&["ok"]), MatchMode::Regex),
                ]),
            }
        );
    }

    // build_filter_set_from_filter_args aborts on the first bad expression and
    // surfaces that expression's error verbatim.
    #[test]
    fn build_filter_set_propagates_first_error() {
        scenarios!(
            run = |exprs| {
                build_filter_set_from_filter_args(exprs)
                    .map(|set| set.filters.iter().map(parts).collect::<Vec<_>>())
            };
            "invalid field aborts the build" {
                vec!["device_type:ok".to_string(), "nope:value".to_string()] => FailsWith(
                    "Unknown field 'nope'. Valid fields: device_type, part_number, \
                     firmware_version, mac_address, description, pci_name, status"
                        .to_string(),
                ),
            }

            "malformed expression aborts the build" {
                vec!["justonepart".to_string()] => FailsWith(
                    "Invalid filter expression 'justonepart'. Expected format: \
                     field:value[,value2,value3] or \
                     field:value[,value2,value3]:match_mode"
                        .to_string(),
                ),
            }
        );
    }

    // OutputFormat round-trips through clap's ValueEnum string forms, covering
    // every variant's kebab-cased name.
    #[test]
    fn output_format_value_enum_round_trip() {
        value_scenarios!(
            run = |name| OutputFormat::from_str(name, true).is_ok();
            "ascii-table" {
                "ascii-table" => true,
            }

            "json" {
                "json" => true,
            }

            "yaml" {
                "yaml" => true,
            }

            "unknown format not accepted" {
                "xml" => false,
            }
        );
    }
}
