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

// tests/json_parser_tests.rs
// Tests for JsonResponseParser functionality

use std::fs;

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use libmlx::runner::error::MlxRunnerError;
use libmlx::runner::exec_options::ExecOptions;
use libmlx::runner::json_parser::JsonResponseParser;
use libmlx::runner::result_types::QueryResult;
use libmlx::variables::registry::MlxVariableRegistry;
use libmlx::variables::value::MlxValueType;
use serde_json::json;

use super::common;

// Build a parser over `registry`, write `json` to a temp file, and parse it for
// `device`. The temp-file/parser dance was copy-pasted into every test below.
fn parse(
    registry: &MlxVariableRegistry,
    json: serde_json::Value,
    device: &str,
) -> Result<QueryResult, MlxRunnerError> {
    let options = ExecOptions::default();
    let parser = JsonResponseParser {
        registry,
        options: &options,
    };
    let temp_file = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        temp_file.path(),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .unwrap();
    parser.parse_json_response(temp_file.path(), device)
}

#[test]
fn test_parse_basic_json_response() {
    let registry = common::create_test_registry();
    let result = parse(
        &registry,
        common::create_sample_json_response("01:00.0"),
        "01:00.0",
    )
    .unwrap();

    // Verify device info
    assert_eq!(
        result.device_info.device_type,
        Some("BlueField3".to_string())
    );
    assert_eq!(
        result.device_info.part_number,
        Some("900-9D3D4-00EN-HA0_Ax".to_string())
    );

    // Verify variables were parsed
    assert_eq!(result.variables.len(), 5); // SRIOV_EN, NUM_OF_VFS, POWER_MODE, DEVICE_NAME, PERFORMANCE_PRESET

    // Check SRIOV_EN boolean parsing
    let sriov_var = result
        .variables
        .iter()
        .find(|v| v.name() == "SRIOV_EN")
        .unwrap();

    assert_eq!(sriov_var.current_value.value, MlxValueType::Boolean(true));
    assert_eq!(sriov_var.default_value.value, MlxValueType::Boolean(false));
    assert_eq!(sriov_var.next_value.value, MlxValueType::Boolean(true));
    assert!(sriov_var.modified);
    assert!(!sriov_var.read_only);
}

#[test]
fn test_device_mismatch_error() {
    let registry = common::create_test_registry();
    // Try to parse with a different expected device.
    let result = parse(
        &registry,
        common::create_sample_json_response("01:00.0"),
        "02:00.0",
    );

    assert!(result.is_err());
    if let Err(libmlx::runner::error::MlxRunnerError::DeviceMismatch { expected, actual }) = result
    {
        assert_eq!(expected, "02:00.0");
        assert_eq!(actual, "01:00.0");
    } else {
        panic!("Expected DeviceMismatch error");
    }
}

#[test]
fn test_malformed_json() {
    let registry = common::create_test_registry();
    let options = ExecOptions::default();
    let parser = JsonResponseParser {
        registry: &registry,
        options: &options,
    };

    let malformed_json = r#"{"Device #1": {"invalid": "structure"#;

    let temp_file = tempfile::NamedTempFile::new().unwrap();
    fs::write(temp_file.path(), malformed_json).unwrap();

    let result = parser.parse_json_response(temp_file.path(), "01:00.0");

    assert!(result.is_err());
}

// The boolean spellings mlxconfig might hand back, parsed for TEST_BOOL.
#[test]
fn test_boolean_parsing_variations() {
    let registry = common::create_minimal_test_registry();
    scenarios!(
        run = |raw| {
            let json = json!({
                "Device #1": {
                    "description": "Test Device",
                    "device": "01:00.0",
                    "device_type": "Test",
                    "name": "Test",
                    "tlv_configuration": {
                        "TEST_BOOL": {
                            "current_value": raw,
                            "default_value": "False(0)",
                            "modified": false,
                            "next_value": raw,
                            "read_only": false
                        }
                    }
                }
            });
            parse(&registry, json, "01:00.0")
                .map(|r| {
                    r.variables
                        .iter()
                        .find(|v| v.name() == "TEST_BOOL")
                        .unwrap()
                        .current_value
                        .value
                        .clone()
                })
                .map_err(drop)
        };
        "True(1)" {
            "True(1)" => Yields(MlxValueType::Boolean(true)),
        }

        "False(0)" {
            "False(0)" => Yields(MlxValueType::Boolean(false)),
        }

        "TRUE" {
            "TRUE" => Yields(MlxValueType::Boolean(true)),
        }

        "FALSE" {
            "FALSE" => Yields(MlxValueType::Boolean(false)),
        }
    );
}

#[test]
fn test_log_json_output_option() {
    let registry = common::create_test_registry();
    let options = ExecOptions::new().with_log_json_output(true);
    let parser = JsonResponseParser {
        registry: &registry,
        options: &options,
    };

    let json_data = common::create_sample_json_response("01:00.0");

    let temp_file = tempfile::NamedTempFile::new().unwrap();
    let json_string = serde_json::to_string_pretty(&json_data).unwrap();
    fs::write(temp_file.path(), json_string).unwrap();

    // Should not fail even with logging enabled
    let result = parser.parse_json_response(temp_file.path(), "01:00.0");
    assert!(result.is_ok());
}
