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

// src/json_parser.rs
// JSON response parser for converting mlxconfig JSON output into typed
// structures with all of our super fancy proper variable/value validation
// and array (and/or sparse array) handling.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::runner::error::MlxRunnerError;
use crate::runner::exec_options::ExecOptions;
use crate::runner::result_types::{QueriedDeviceInfo, QueriedVariable, QueryResult};
use crate::variables::registry::MlxVariableRegistry;
use crate::variables::spec::MlxVariableSpec;
use crate::variables::value::MlxConfigValue;

// JsonResponseParser handles parsing of mlxconfig JSON responses
// and conversion to strongly-typed QueryResult structures.
pub struct JsonResponseParser<'a> {
    // registry is the registry containing variable definitions
    // for validation.
    pub registry: &'a MlxVariableRegistry,
    // options are the execution options provided by the
    // parent runner, which in this case is primarily used
    // by the JSON parser for logging control.
    pub options: &'a ExecOptions,
}

// JsonResponse represents the top-level structure of the
// mlxconfig JSON output, where there is a "device", and
// within that is everything else (including info about
// the device, as well as the actual variable settings
// themselves).
#[derive(Debug, Deserialize, Serialize)]
struct JsonResponse {
    #[serde(rename = "Device #1")]
    device: JsonDevice,
}

// JsonDevice is the one and only entry at the top level
// of a JsonResponse, containing the device information
// and all variable configuration (which lives in the
// tlv_configuration parameter).
#[derive(Debug, Deserialize, Serialize)]
struct JsonDevice {
    description: String,
    device: String,
    device_type: String,
    name: String,
    tlv_configuration: HashMap<String, JsonVariable>,
}

// JsonVariable represents a single variable's state in
// the mlxconfig JSON response. The tlv_configuration
// hashmap is a map of VAR_NAME -> JsonVariable, with
// all of these fields populated.
#[derive(Debug, Deserialize, Serialize)]
pub struct JsonVariable {
    // current_value is current value actually applied
    // and being used by the device.
    current_value: serde_json::Value,
    // default_value is the factory default value the
    // device comes with.
    default_value: serde_json::Value,
    // modified is if the next_value is different than
    // the default value.
    modified: bool,
    // next_value is the value that *will* be applied
    // to the card on next reboot. If we have applied
    // changes, but *haven't* rebooted, then we will
    // see that next_value != current_value.
    next_value: serde_json::Value,
    // read_only is if this is a read-only variable
    // supplied by the card, and can't actually be
    // modified by mlxconfig.
    read_only: bool,
}

// JsonValueField is used to specify which field to
// extract from a JsonVariable and convert into an
// MlxConfigValue.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum JsonValueField {
    Default,
    Current,
    Next,
}

impl<'a> JsonResponseParser<'a> {
    // parse_json_response parses a complete JSON response file from mlxconfig
    // into a QueryResult. It also validates that the device in the response
    // matches the expected device we provided, because if those don't match,
    // that's kinda sus, and we don't want to be modifying values on the wrong
    // card.
    pub fn parse_json_response(
        &self,
        json_path: &Path,
        expected_device: &str,
    ) -> Result<QueryResult, MlxRunnerError> {
        let content = fs::read_to_string(json_path)
            .map_err(|e| MlxRunnerError::temp_file_error(json_path.to_path_buf(), e))?;

        if self.options.log_json_output {
            println!("[JSON] Response content:\n{content}");
        }

        let json_response: JsonResponse = serde_json::from_str(&content)
            .map_err(|e| MlxRunnerError::json_parsing(content.clone(), e))?;

        // And now verify the device from the response matches
        // what we [thought we] queried -- if it doesn't match,
        // something is wonky.
        if json_response.device.device != expected_device {
            return Err(MlxRunnerError::DeviceMismatch {
                expected: expected_device.to_string(),
                actual: json_response.device.device,
            });
        }

        // Finally, parse variables from the tlv_configuration
        // field and pass back a Vec<QueriedVariable> with everything
        // converted into properly typed values and such.
        let variables = self.parse_variables(&json_response.device.tlv_configuration)?;

        Ok(QueryResult {
            device_info: QueriedDeviceInfo {
                device_id: Some(json_response.device.device),
                device_type: Some(json_response.device.device_type),
                part_number: Some(json_response.device.name),
                description: Some(json_response.device.description),
            },
            variables,
        })
    }

    // parse_variables is the thing that actually parses all variables
    // from the tlv_configuration HashMap, handling both scalar variables
    // and array variables (technically with sparse array support, even
    // though mlxconfig really shouldn't be giving us incomplete array
    // sets back).
    pub fn parse_variables(
        &self,
        json_vars: &HashMap<String, JsonVariable>,
    ) -> Result<Vec<QueriedVariable>, MlxRunnerError> {
        let mut variables = Vec::new();
        let mut processed_arrays = std::collections::HashSet::new();

        for (json_name, json_var) in json_vars {
            // Check if this is an array element like "ARRAY[0]".
            if let Some((base_name, _)) = crate::runner::traits::parse_array_index(json_name)? {
                // Skip if we already processed this array.
                if processed_arrays.contains(&base_name) {
                    continue;
                }
                // Otherwise insert it as being processed...
                processed_arrays.insert(base_name.clone());

                // ...and then fire off the array parser, which will
                // just go and grab each index to build a fully-populated
                // array variable (instead of incrementally building
                // arrays and then assembling at the end).
                if let Some(registry_var) = self.registry.get_variable(&base_name) {
                    let queried_var =
                        self.parse_array_variable(registry_var, json_vars, &base_name)?;
                    variables.push(queried_var);
                }
            } else {
                // Or just do a basic processing of a scalar variable.
                if let Some(registry_var) = self.registry.get_variable(json_name) {
                    let queried_var = self.parse_single_variable(registry_var, json_var)?;
                    variables.push(queried_var);
                }
            }
        }

        Ok(variables)
    }

    // parse_single_variable parses a single scalar (non-array) variable
    // from mlxconfig JSON output. Returns a fully populated QueriedVariable
    // with all value states.
    pub fn parse_single_variable(
        &self,
        registry_var: &crate::variables::variable::MlxConfigVariable,
        json_var: &JsonVariable,
    ) -> Result<QueriedVariable, MlxRunnerError> {
        let current_value =
            self.json_value_to_config_value(registry_var, &json_var.current_value)?;
        let default_value =
            self.json_value_to_config_value(registry_var, &json_var.default_value)?;
        let next_value = self.json_value_to_config_value(registry_var, &json_var.next_value)?;

        Ok(QueriedVariable {
            variable: registry_var.clone(),
            current_value,
            default_value,
            next_value,
            modified: json_var.modified,
            read_only: json_var.read_only,
        })
    }

    // parse_array_variable parses an array variable by collecting
    // all indices from the mlxconfig JSON output, and reconstructing
    // the array from all individual [index] entries. Technically
    // this builds a "sparse" array, but really, mlxconfig *should*
    // be giving us each index anyway.
    pub fn parse_array_variable(
        &self,
        registry_var: &crate::variables::variable::MlxConfigVariable,
        json_vars: &HashMap<String, JsonVariable>,
        base_name: &str,
    ) -> Result<QueriedVariable, MlxRunnerError> {
        // Get array size from registry spec.
        let array_size = self.get_array_size(&registry_var.spec)?;

        // Build sparse arrays for current, default, and next values.
        let current_value = self.build_sparse_array_from_json(
            registry_var,
            json_vars,
            base_name,
            array_size,
            JsonValueField::Current,
        )?;
        let default_value = self.build_sparse_array_from_json(
            registry_var,
            json_vars,
            base_name,
            array_size,
            JsonValueField::Default,
        )?;
        let next_value = self.build_sparse_array_from_json(
            registry_var,
            json_vars,
            base_name,
            array_size,
            JsonValueField::Next,
        )?;

        // Collect modified/read_only status from any index.
        let mut modified = false;
        let mut read_only = false;

        for index in 0..array_size {
            let indexed_name = format!("{base_name}[{index}]");
            if let Some(json_var) = json_vars.get(&indexed_name) {
                if json_var.modified {
                    modified = true;
                }
                if json_var.read_only {
                    read_only = true;
                }
            }
        }

        Ok(QueriedVariable {
            variable: registry_var.clone(),
            current_value,
            default_value,
            next_value,
            modified,
            read_only,
        })
    }

    // build_sparse_array_from_json builds a sparse array MlxConfigValue
    // from mlxconfig JSON data for a given field within a JsonVariable,
    // i.e. the default, current, or next value; handles all of the array
    // types, does proper type conversion, etc.
    pub fn build_sparse_array_from_json(
        &self,
        registry_var: &crate::variables::variable::MlxConfigVariable,
        json_vars: &HashMap<String, JsonVariable>,
        base_name: &str,
        array_size: usize,
        value_field: JsonValueField,
    ) -> Result<MlxConfigValue, MlxRunnerError> {
        match &registry_var.spec {
            MlxVariableSpec::BooleanArray { .. } => {
                let sparse_values = self.build_typed_sparse_array(
                    json_vars,
                    base_name,
                    array_size,
                    value_field,
                    |json_value| self.parse_bool_from_json(json_value),
                )?;
                registry_var.with(sparse_values).map_err(|e| {
                    MlxRunnerError::value_conversion(
                        base_name.to_string(),
                        "boolean array".to_string(),
                        e,
                    )
                })
            }
            MlxVariableSpec::IntegerArray { .. } => {
                let sparse_values = self.build_typed_sparse_array(
                    json_vars,
                    base_name,
                    array_size,
                    value_field,
                    |json_value| self.parse_int_from_json(json_value),
                )?;
                registry_var.with(sparse_values).map_err(|e| {
                    MlxRunnerError::value_conversion(
                        base_name.to_string(),
                        "integer array".to_string(),
                        e,
                    )
                })
            }
            MlxVariableSpec::EnumArray { .. } => {
                let sparse_values = self.build_typed_sparse_array(
                    json_vars,
                    base_name,
                    array_size,
                    value_field,
                    |json_value| self.parse_string_from_json(json_value),
                )?;
                registry_var.with(sparse_values).map_err(|e| {
                    MlxRunnerError::value_conversion(
                        base_name.to_string(),
                        "enum array".to_string(),
                        e,
                    )
                })
            }
            MlxVariableSpec::BinaryArray { .. } => {
                let sparse_values = self.build_typed_sparse_array(
                    json_vars,
                    base_name,
                    array_size,
                    value_field,
                    |json_value| self.parse_hex_from_json(json_value),
                )?;
                registry_var.with(sparse_values).map_err(|e| {
                    MlxRunnerError::value_conversion(
                        base_name.to_string(),
                        "binary array".to_string(),
                        e,
                    )
                })
            }
            _ => Err(MlxRunnerError::ValueConversion {
                variable_name: base_name.to_string(),
                value: "array".to_string(),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "array type".to_string(),
                    got: format!("{:?}", registry_var.spec),
                },
            }),
        }
    }

    // build_typed_sparse_array is a generic helper for building typed
    // sparse arrays from mlxconfig JSON data. It takes a parsing function
    // from the caller to convert the input JSON values to the target
    // output type.
    pub fn build_typed_sparse_array<T, F>(
        &self,
        json_vars: &HashMap<String, JsonVariable>,
        base_name: &str,
        array_size: usize,
        value_field: JsonValueField,
        parse_fn: F,
    ) -> Result<Vec<Option<T>>, MlxRunnerError>
    where
        F: Fn(&serde_json::Value) -> Result<T, MlxRunnerError>,
    {
        let mut sparse_values: Vec<Option<T>> = Vec::with_capacity(array_size);

        for index in 0..array_size {
            let indexed_name = format!("{base_name}[{index}]");

            if let Some(json_var) = json_vars.get(&indexed_name) {
                let json_value = self.get_json_field_value(json_var, value_field)?;
                let parsed_value = parse_fn(json_value)?;
                sparse_values.push(Some(parsed_value));
            } else {
                // Missing indices become None in sparse arrays, so this
                // handles cases where device doesn't return all array elements,
                // which like I mentioned above, *shouldn't* happen, but if it
                // does, we just handle it gracefully.
                sparse_values.push(None);
            }
        }

        Ok(sparse_values)
    }

    // json_value_to_config_value converts a JSON value to an
    // MlxConfigValue using the variable's spec, handling automatic
    // type conversion and validation.
    pub fn json_value_to_config_value(
        &self,
        registry_var: &crate::variables::variable::MlxConfigVariable,
        json_value: &serde_json::Value,
    ) -> Result<MlxConfigValue, MlxRunnerError> {
        match json_value {
            serde_json::Value::String(_) => {
                let cleaned_string = self.parse_string_from_json(json_value)?;
                registry_var.with(cleaned_string).map_err(|e| {
                    MlxRunnerError::value_conversion(
                        registry_var.name.clone(),
                        "string".to_string(),
                        e,
                    )
                })
            }
            serde_json::Value::Number(_) => {
                let int_val = self.parse_int_from_json(json_value)?;
                registry_var.with(int_val).map_err(|e| {
                    MlxRunnerError::value_conversion(
                        registry_var.name.clone(),
                        "number".to_string(),
                        e,
                    )
                })
            }
            _ => Err(MlxRunnerError::ValueConversion {
                variable_name: registry_var.name.clone(),
                value: format!("{json_value:?}"),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "string or number".to_string(),
                    got: format!("{json_value:?}"),
                },
            }),
        }
    }

    // get_json_field_value extracts the appropriate field value
    // from a JsonVariable (default, current, or next), based on
    // the JsonVariable and JsonValueField provided.
    fn get_json_field_value<'b>(
        &self,
        json_var: &'b JsonVariable,
        value_field: JsonValueField,
    ) -> Result<&'b serde_json::Value, MlxRunnerError> {
        match value_field {
            JsonValueField::Current => Ok(&json_var.current_value),
            JsonValueField::Default => Ok(&json_var.default_value),
            JsonValueField::Next => Ok(&json_var.next_value),
        }
    }

    // parse_bool_from_json parses boolean values from mlxconfig JSON
    // responses. Handles format like "TRUE(1)" or "FALSE(0)" by stripping
    // parentheticals.
    fn parse_bool_from_json(&self, json_value: &serde_json::Value) -> Result<bool, MlxRunnerError> {
        match json_value {
            serde_json::Value::String(s) => {
                let cleaned = if s.contains('(') {
                    s.split('(').next().unwrap_or(s)
                } else {
                    s
                };

                match cleaned.to_lowercase().as_str() {
                    "true" => Ok(true),
                    "false" => Ok(false),
                    _ => Err(MlxRunnerError::ValueConversion {
                        variable_name: "boolean".to_string(),
                        value: s.clone(),
                        error: crate::variables::value::MlxValueError::TypeMismatch {
                            expected: "boolean".to_string(),
                            got: s.clone(),
                        },
                    }),
                }
            }
            _ => Err(MlxRunnerError::ValueConversion {
                variable_name: "boolean".to_string(),
                value: format!("{json_value:?}"),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "string".to_string(),
                    got: format!("{json_value:?}"),
                },
            }),
        }
    }

    // parse_int_from_json parses integer values from mlxconfig
    // JSON responses.
    fn parse_int_from_json(&self, json_value: &serde_json::Value) -> Result<i64, MlxRunnerError> {
        match json_value {
            serde_json::Value::Number(n) => {
                n.as_i64().ok_or_else(|| MlxRunnerError::ValueConversion {
                    variable_name: "integer".to_string(),
                    value: n.to_string(),
                    error: crate::variables::value::MlxValueError::TypeMismatch {
                        expected: "integer".to_string(),
                        got: "float".to_string(),
                    },
                })
            }
            _ => Err(MlxRunnerError::ValueConversion {
                variable_name: "integer".to_string(),
                value: format!("{json_value:?}"),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "number".to_string(),
                    got: format!("{json_value:?}"),
                },
            }),
        }
    }

    // parse_string_from_json parses string values from mlxconfig
    // JSON responses. Handles format like "ENUM_VALUE(1)" by stripping
    // parentheticals.
    fn parse_string_from_json(
        &self,
        json_value: &serde_json::Value,
    ) -> Result<String, MlxRunnerError> {
        match json_value {
            serde_json::Value::String(s) => {
                let cleaned = if s.contains('(') {
                    s.split('(').next().unwrap_or(s).to_string()
                } else {
                    s.clone()
                };
                Ok(cleaned)
            }
            _ => Err(MlxRunnerError::ValueConversion {
                variable_name: "string".to_string(),
                value: format!("{json_value:?}"),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "string".to_string(),
                    got: format!("{json_value:?}"),
                },
            }),
        }
    }

    // parse_hex_from_json parses hex string values from mlxconfig
    // JSON responses. Handles both "0x1a2b3c" and "1a2b3c" formats.
    fn parse_hex_from_json(
        &self,
        json_value: &serde_json::Value,
    ) -> Result<Vec<u8>, MlxRunnerError> {
        match json_value {
            serde_json::Value::String(s) => {
                let hex_str = if s.starts_with("0x") || s.starts_with("0X") {
                    &s[2..]
                } else {
                    s
                };

                hex::decode(hex_str).map_err(|_| MlxRunnerError::ValueConversion {
                    variable_name: "binary".to_string(),
                    value: s.clone(),
                    error: crate::variables::value::MlxValueError::TypeMismatch {
                        expected: "hex string".to_string(),
                        got: s.clone(),
                    },
                })
            }
            _ => Err(MlxRunnerError::ValueConversion {
                variable_name: "binary".to_string(),
                value: format!("{json_value:?}"),
                error: crate::variables::value::MlxValueError::TypeMismatch {
                    expected: "string".to_string(),
                    got: format!("{json_value:?}"),
                },
            }),
        }
    }

    // get_array_size gets the array size from a
    // variable spec for doing array processing.
    fn get_array_size(&self, spec: &MlxVariableSpec) -> Result<usize, MlxRunnerError> {
        crate::runner::traits::get_array_size_from_spec(spec)
    }
}

#[cfg(test)]
mod coverage_tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, scenarios, value_scenarios};
    use serde_json::json;

    use super::*;
    use crate::variables::value::MlxValueType;
    use crate::variables::variable::MlxConfigVariable;

    // Builds a JsonVariable where every field (current/default/next) holds the
    // same JSON value -- enough for the per-field parsing helpers, which only
    // read one field at a time.
    fn json_var(value: serde_json::Value) -> JsonVariable {
        JsonVariable {
            current_value: value.clone(),
            default_value: value.clone(),
            modified: false,
            next_value: value,
            read_only: false,
        }
    }

    // Builds a JsonVariable with distinct current/default/next values plus the
    // modified/read_only flags, for exercising field selection and flag rollup.
    fn json_var_full(
        current: serde_json::Value,
        default: serde_json::Value,
        next: serde_json::Value,
        modified: bool,
        read_only: bool,
    ) -> JsonVariable {
        JsonVariable {
            current_value: current,
            default_value: default,
            modified,
            next_value: next,
            read_only,
        }
    }

    fn var(name: &str, spec: MlxVariableSpec) -> MlxConfigVariable {
        MlxConfigVariable {
            name: name.to_string(),
            description: format!("desc {name}"),
            read_only: false,
            spec,
        }
    }

    fn registry_with(vars: Vec<MlxConfigVariable>) -> MlxVariableRegistry {
        MlxVariableRegistry {
            name: "test".to_string(),
            variables: vars,
            filters: None,
        }
    }

    // parse_bool_from_json: TRUE(n)/FALSE(n) get their parenthetical stripped and
    // are matched case-insensitively; bare true/false work; anything else (or a
    // non-string JSON value) is rejected. MlxRunnerError isn't PartialEq, so error
    // rows drop the error and assert only that it fails.
    #[test]
    fn parse_bool_from_json_covers_every_branch() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        scenarios!(
            run = |value| parser.parse_bool_from_json(&value).map_err(drop);
            "TRUE(1) strips the parenthetical" {
                json!("TRUE(1)") => Yields(true),
            }

            "FALSE(0) strips the parenthetical" {
                json!("FALSE(0)") => Yields(false),
            }

            "bare true" {
                json!("true") => Yields(true),
            }

            "bare false" {
                json!("false") => Yields(false),
            }

            "mixed case True" {
                json!("True") => Yields(true),
            }

            "unrecognized string is rejected" {
                json!("maybe") => Fails,
            }

            "a number is not a boolean string" {
                json!(1) => Fails,
            }

            "a JSON bool is still rejected (must be a string)" {
                json!(true) => Fails,
            }
        );
    }

    // parse_int_from_json: only a JSON integer number yields an i64; a float, a
    // string, or any non-number fails.
    #[test]
    fn parse_int_from_json_covers_every_branch() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        scenarios!(
            run = |value| parser.parse_int_from_json(&value).map_err(drop);
            "positive integer" {
                json!(42) => Yields(42),
            }

            "negative integer" {
                json!(-7) => Yields(-7),
            }

            "zero" {
                json!(0) => Yields(0),
            }

            "a float has no i64 representation" {
                json!(1.5) => Fails,
            }

            "a numeric string is not a JSON number" {
                json!("42") => Fails,
            }
        );
    }

    // parse_string_from_json: keeps everything before the first '(' when a
    // parenthetical is present; otherwise returns the string unchanged (no
    // trimming here). Non-string JSON is rejected.
    #[test]
    fn parse_string_from_json_covers_every_branch() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        scenarios!(
            run = |value| parser.parse_string_from_json(&value).map_err(drop);
            "parenthetical is stripped" {
                json!("ENUM_VALUE(1)") => Yields("ENUM_VALUE".to_string()),
            }

            "plain string is unchanged" {
                json!("plain") => Yields("plain".to_string()),
            }

            "only the first '(' splits" {
                json!("a(b(c") => Yields("a".to_string()),
            }

            "surrounding whitespace is NOT trimmed here" {
                json!("  spaced  ") => Yields("  spaced  ".to_string()),
            }

            "a number is rejected" {
                json!(5) => Fails,
            }
        );
    }

    // parse_hex_from_json: decodes hex with or without an 0x/0X prefix; invalid
    // hex (odd length, non-hex chars) and non-string JSON fail.
    #[test]
    fn parse_hex_from_json_covers_every_branch() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        scenarios!(
            run = |value| parser.parse_hex_from_json(&value).map_err(drop);
            "0x prefix" {
                json!("0x1a2b3c") => Yields(vec![0x1a, 0x2b, 0x3c]),
            }

            "0X prefix" {
                json!("0X1A2B") => Yields(vec![0x1a, 0x2b]),
            }

            "bare hex" {
                json!("1a2b") => Yields(vec![0x1a, 0x2b]),
            }

            "empty string decodes to empty bytes" {
                json!("") => Yields(vec![]),
            }

            "non-hex characters are rejected" {
                json!("not_hex") => Fails,
            }

            "odd length is rejected" {
                json!("1a2") => Fails,
            }

            "a number is rejected" {
                json!(123) => Fails,
            }
        );
    }

    // get_json_field_value selects the right field of a JsonVariable per the
    // JsonValueField; it never fails. The JsonVariable carries distinct values so
    // each arm is distinguishable.
    #[test]
    fn get_json_field_value_selects_the_right_field() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };
        let jv = json_var_full(json!("cur"), json!("def"), json!("nxt"), false, false);

        scenarios!(
            run = |field| {
                parser
                    .get_json_field_value(&jv, field)
                    .cloned()
                    .map_err(drop)
            };
            "Current" {
                JsonValueField::Current => Yields(json!("cur")),
            }

            "Default" {
                JsonValueField::Default => Yields(json!("def")),
            }

            "Next" {
                JsonValueField::Next => Yields(json!("nxt")),
            }
        );
    }

    // get_array_size delegates to the spec helper: array specs yield their size;
    // every scalar/untyped spec is rejected.
    #[test]
    fn get_array_size_covers_array_and_non_array_specs() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        scenarios!(
            run = |spec| parser.get_array_size(&spec).map_err(drop);
            "boolean array" {
                MlxVariableSpec::BooleanArray { size: 4 } => Yields(4),
            }

            "integer array" {
                MlxVariableSpec::IntegerArray { size: 2 } => Yields(2),
            }

            "binary array" {
                MlxVariableSpec::BinaryArray { size: 7 } => Yields(7),
            }

            "enum array" {
                MlxVariableSpec::EnumArray {
                    options: vec!["a".to_string()],
                    size: 3,
                } => Yields(3),
            }

            "scalar boolean has no size" {
                MlxVariableSpec::Boolean => Fails,
            }

            "untyped array has no size" {
                MlxVariableSpec::Array => Fails,
            }
        );
    }

    // json_value_to_config_value: a JSON string routes through the string spec
    // conversion (note `with` trims and validates); a JSON number routes through
    // integer conversion; bool/null/array are rejected outright as neither string
    // nor number.
    #[test]
    fn json_value_to_config_value_covers_string_number_and_rejects() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };
        let str_var = var("S", MlxVariableSpec::String);
        let int_var = var("I", MlxVariableSpec::Integer);
        let enum_var = var(
            "E",
            MlxVariableSpec::Enum {
                options: vec!["medium".to_string()],
            },
        );

        // String spec: parse_string keeps the raw text, then `with` trims it.
        scenarios!(
            run = |value| {
                parser
                    .json_value_to_config_value(&str_var, &value)
                    .map(|v| v.value)
                    .map_err(drop)
            };
            "plain string stored (trimmed by `with`)" {
                json!("  hi  ") => Yields(MlxValueType::String("hi".to_string())),
            }

            "parenthetical stripped before `with`" {
                json!("FOO(2)") => Yields(MlxValueType::String("FOO".to_string())),
            }

            "a JSON bool is neither string nor number" {
                json!(true) => Fails,
            }

            "JSON null is rejected" {
                json!(null) => Fails,
            }

            "a JSON array is rejected" {
                json!(["a", "b"]) => Fails,
            }
        );

        // Number spec: routes through parse_int + with(i64).
        Case {
            scenario: "number into an integer variable",
            input: json!(99),
            expect: Yields(MlxValueType::Integer(99)),
        }
        .check(|value| {
            parser
                .json_value_to_config_value(&int_var, &value)
                .map(|v| v.value)
                .map_err(drop)
        });

        // Enum spec with a parenthetical string: stripped, then validated.
        Case {
            scenario: "enum string validates after stripping parenthetical",
            input: json!("medium(3)"),
            expect: Yields(MlxValueType::Enum("medium".to_string())),
        }
        .check(|value| {
            parser
                .json_value_to_config_value(&enum_var, &value)
                .map(|v| v.value)
                .map_err(drop)
        });

        // Enum spec where the stripped value isn't an allowed option -> `with` fails.
        Case {
            scenario: "enum string not in options is rejected",
            input: json!("bogus"),
            expect: Fails,
        }
        .check(|value| {
            parser
                .json_value_to_config_value(&enum_var, &value)
                .map(|v| v.value)
                .map_err(drop)
        });
    }

    // build_typed_sparse_array fills present indices with Some(parsed) and missing
    // indices with None, in ascending order. Uses parse_int_from_json so the value
    // type is i64.
    #[test]
    fn build_typed_sparse_array_fills_present_and_missing() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        // ARR[0] and ARR[2] present, ARR[1] missing, size 3.
        let mut json_vars = HashMap::new();
        json_vars.insert("ARR[0]".to_string(), json_var(json!(10)));
        json_vars.insert("ARR[2]".to_string(), json_var(json!(30)));

        let result = parser
            .build_typed_sparse_array(&json_vars, "ARR", 3, JsonValueField::Current, |jv| {
                parser.parse_int_from_json(jv)
            })
            .expect("ints parse cleanly");
        assert_eq!(result, vec![Some(10), None, Some(30)]);

        // size 0 yields an empty sparse array regardless of present keys.
        let empty = parser
            .build_typed_sparse_array(&json_vars, "ARR", 0, JsonValueField::Current, |jv| {
                parser.parse_int_from_json(jv)
            })
            .expect("size 0 is fine");
        assert_eq!(empty, Vec::<Option<i64>>::new());

        // A present index whose value can't be parsed propagates the error.
        let mut bad = HashMap::new();
        bad.insert("ARR[0]".to_string(), json_var(json!("not_an_int")));
        let err = parser.build_typed_sparse_array(&bad, "ARR", 1, JsonValueField::Current, |jv| {
            parser.parse_int_from_json(jv)
        });
        assert!(err.is_err());
    }

    // build_sparse_array_from_json dispatches on the array spec variant and builds
    // a correctly-sized MlxValueType; a non-array spec is rejected. Each row gives
    // a fully-populated set of indices so size validation in `with` passes.
    #[test]
    fn build_sparse_array_from_json_covers_each_array_variant() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };

        // Boolean array, size 2, both indices present.
        let bool_var = var("BA", MlxVariableSpec::BooleanArray { size: 2 });
        let mut bool_vars = HashMap::new();
        bool_vars.insert("BA[0]".to_string(), json_var(json!("TRUE(1)")));
        bool_vars.insert("BA[1]".to_string(), json_var(json!("FALSE(0)")));
        let bool_result = parser
            .build_sparse_array_from_json(&bool_var, &bool_vars, "BA", 2, JsonValueField::Current)
            .expect("boolean array builds");
        assert_eq!(
            bool_result.value,
            MlxValueType::BooleanArray(vec![Some(true), Some(false)])
        );

        // Integer array, size 3, one gap -> None in the middle.
        let int_var = var("IA", MlxVariableSpec::IntegerArray { size: 3 });
        let mut int_vars = HashMap::new();
        int_vars.insert("IA[0]".to_string(), json_var(json!(1)));
        int_vars.insert("IA[2]".to_string(), json_var(json!(3)));
        let int_result = parser
            .build_sparse_array_from_json(&int_var, &int_vars, "IA", 3, JsonValueField::Current)
            .expect("integer array builds");
        assert_eq!(
            int_result.value,
            MlxValueType::IntegerArray(vec![Some(1), None, Some(3)])
        );

        // Enum array, size 2, parenthetical stripped then validated.
        let enum_var = var(
            "EA",
            MlxVariableSpec::EnumArray {
                options: vec!["in".to_string(), "out".to_string()],
                size: 2,
            },
        );
        let mut enum_vars = HashMap::new();
        enum_vars.insert("EA[0]".to_string(), json_var(json!("in(0)")));
        enum_vars.insert("EA[1]".to_string(), json_var(json!("out(1)")));
        let enum_result = parser
            .build_sparse_array_from_json(&enum_var, &enum_vars, "EA", 2, JsonValueField::Current)
            .expect("enum array builds");
        assert_eq!(
            enum_result.value,
            MlxValueType::EnumArray(vec![Some("in".to_string()), Some("out".to_string())])
        );

        // Binary array, size 2, hex decoded.
        let bin_var = var("BIN", MlxVariableSpec::BinaryArray { size: 2 });
        let mut bin_vars = HashMap::new();
        bin_vars.insert("BIN[0]".to_string(), json_var(json!("0x1a2b")));
        bin_vars.insert("BIN[1]".to_string(), json_var(json!("3c4d")));
        let bin_result = parser
            .build_sparse_array_from_json(&bin_var, &bin_vars, "BIN", 2, JsonValueField::Current)
            .expect("binary array builds");
        assert_eq!(
            bin_result.value,
            MlxValueType::BinaryArray(vec![Some(vec![0x1a, 0x2b]), Some(vec![0x3c, 0x4d])])
        );

        // A non-array spec hits the catch-all error arm.
        let scalar_var = var("SC", MlxVariableSpec::Boolean);
        let scalar_err = parser.build_sparse_array_from_json(
            &scalar_var,
            &HashMap::new(),
            "SC",
            1,
            JsonValueField::Current,
        );
        assert!(scalar_err.is_err());

        // An enum array element that isn't an allowed option -> `with` rejects.
        let mut bad_enum_vars = HashMap::new();
        bad_enum_vars.insert("EA[0]".to_string(), json_var(json!("nope")));
        bad_enum_vars.insert("EA[1]".to_string(), json_var(json!("out")));
        let bad_enum = parser.build_sparse_array_from_json(
            &enum_var,
            &bad_enum_vars,
            "EA",
            2,
            JsonValueField::Current,
        );
        assert!(bad_enum.is_err());
    }

    // parse_single_variable converts current/default/next and carries the
    // modified/read_only flags straight through.
    #[test]
    fn parse_single_variable_populates_all_fields() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };
        let int_var = var("I", MlxVariableSpec::Integer);
        let jv = json_var_full(json!(1), json!(2), json!(3), true, true);

        let result = parser
            .parse_single_variable(&int_var, &jv)
            .expect("single var parses");
        assert_eq!(result.current_value.value, MlxValueType::Integer(1));
        assert_eq!(result.default_value.value, MlxValueType::Integer(2));
        assert_eq!(result.next_value.value, MlxValueType::Integer(3));
        assert!(result.modified);
        assert!(result.read_only);

        // A field that can't convert (bool JSON into an integer spec) fails.
        let bad = json_var_full(json!(true), json!(2), json!(3), false, false);
        assert!(parser.parse_single_variable(&int_var, &bad).is_err());
    }

    // parse_array_variable reconstructs an array across indices and rolls up the
    // modified/read_only flags if ANY index has them set.
    #[test]
    fn parse_array_variable_reconstructs_and_rolls_up_flags() {
        let parser = JsonResponseParser {
            registry: &registry_with(vec![]),
            options: &ExecOptions::default(),
        };
        let int_array = var("IA", MlxVariableSpec::IntegerArray { size: 3 });

        let mut json_vars = HashMap::new();
        // index 0: not modified, not read-only
        json_vars.insert(
            "IA[0]".to_string(),
            json_var_full(json!(10), json!(0), json!(10), false, false),
        );
        // index 2: modified set here -> should roll up to true; index 1 missing.
        json_vars.insert(
            "IA[2]".to_string(),
            json_var_full(json!(30), json!(0), json!(30), true, false),
        );

        let result = parser
            .parse_array_variable(&int_array, &json_vars, "IA")
            .expect("array var parses");
        assert_eq!(
            result.current_value.value,
            MlxValueType::IntegerArray(vec![Some(10), None, Some(30)])
        );
        assert!(result.modified, "modified rolls up from any index");
        assert!(!result.read_only, "no index was read-only");

        // A non-array variable spec makes get_array_size fail.
        let scalar = var("SC", MlxVariableSpec::Boolean);
        assert!(
            parser
                .parse_array_variable(&scalar, &HashMap::new(), "SC")
                .is_err()
        );
    }

    // parse_variables walks the tlv map: scalar vars resolve by name; array
    // indices resolve once per base name (dedup); unknown names are skipped.
    #[test]
    fn parse_variables_handles_scalars_arrays_and_unknowns() {
        let scalar = var("FOO", MlxVariableSpec::Integer);
        let array = var("ARR", MlxVariableSpec::IntegerArray { size: 2 });
        let registry = registry_with(vec![scalar, array]);
        let parser = JsonResponseParser {
            registry: &registry,
            options: &ExecOptions::default(),
        };

        let mut json_vars = HashMap::new();
        json_vars.insert("FOO".to_string(), json_var(json!(7)));
        json_vars.insert("ARR[0]".to_string(), json_var(json!(1)));
        json_vars.insert("ARR[1]".to_string(), json_var(json!(2)));
        // Unknown scalar -> skipped (registry has no UNKNOWN).
        json_vars.insert("UNKNOWN".to_string(), json_var(json!(99)));
        // Unknown array base -> skipped.
        json_vars.insert("MYSTERY[0]".to_string(), json_var(json!(5)));

        let result = parser.parse_variables(&json_vars).expect("parses");

        // Exactly FOO and ARR are produced (order is map-dependent, so sort names).
        let mut names: Vec<&str> = result.iter().map(|v| v.name()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["ARR", "FOO"]);

        // The ARR entry was reconstructed from both indices, just once.
        let arr_entry = result.iter().find(|v| v.name() == "ARR").unwrap();
        assert_eq!(
            arr_entry.current_value.value,
            MlxValueType::IntegerArray(vec![Some(1), Some(2)])
        );

        // An invalid array index syntax (non-numeric) propagates an error.
        let mut bad = HashMap::new();
        bad.insert("ARR[x]".to_string(), json_var(json!(1)));
        assert!(parser.parse_variables(&bad).is_err());
    }

    // parse_variables yields nothing when none of the tlv names are registered.
    #[test]
    fn parse_variables_skips_when_nothing_registered() {
        let registry = registry_with(vec![]);
        let parser = JsonResponseParser {
            registry: &registry,
            options: &ExecOptions::default(),
        };
        let mut json_vars = HashMap::new();
        json_vars.insert("FOO".to_string(), json_var(json!(7)));

        value_scenarios!(
            run = |jv| parser.parse_variables(&jv).expect("parses").len();
            "no registered variables -> empty result" {
                json_vars => 0usize,
            }
        );
    }

    // parse_json_response: a well-formed file with a matching device parses; a
    // device mismatch, malformed JSON, and a missing file all fail.
    #[test]
    fn parse_json_response_end_to_end_and_error_paths() {
        use std::io::Write;

        let foo = var("FOO", MlxVariableSpec::Integer);
        let registry = registry_with(vec![foo]);
        let parser = JsonResponseParser {
            registry: &registry,
            options: &ExecOptions::default(),
        };

        let good_json = r#"{
            "Device #1": {
                "description": "Test Card",
                "device": "/dev/mst/mt4129_pciconf0",
                "device_type": "ConnectX7",
                "name": "MCX755106AS",
                "tlv_configuration": {
                    "FOO": {
                        "current_value": 7,
                        "default_value": 0,
                        "modified": true,
                        "next_value": 7,
                        "read_only": false
                    }
                }
            }
        }"#;

        // Happy path: device matches, FOO is parsed.
        let mut good = tempfile::NamedTempFile::new().unwrap();
        good.write_all(good_json.as_bytes()).unwrap();
        let result = parser
            .parse_json_response(good.path(), "/dev/mst/mt4129_pciconf0")
            .expect("parses");
        assert_eq!(
            result.device_info.device_id.as_deref(),
            Some("/dev/mst/mt4129_pciconf0")
        );
        assert_eq!(
            result.device_info.part_number.as_deref(),
            Some("MCX755106AS")
        );
        assert_eq!(result.variables.len(), 1);
        assert_eq!(
            result.variables[0].current_value.value,
            MlxValueType::Integer(7)
        );

        // Device mismatch path.
        let mut mismatch = tempfile::NamedTempFile::new().unwrap();
        mismatch.write_all(good_json.as_bytes()).unwrap();
        assert!(
            parser
                .parse_json_response(mismatch.path(), "/dev/some/other/device")
                .is_err()
        );

        // Malformed JSON path.
        let mut bad = tempfile::NamedTempFile::new().unwrap();
        bad.write_all(b"{ not valid json").unwrap();
        assert!(
            parser
                .parse_json_response(bad.path(), "/dev/mst/mt4129_pciconf0")
                .is_err()
        );

        // Missing file path (temp_file_error).
        assert!(
            parser
                .parse_json_response(
                    std::path::Path::new("/nonexistent/path/to/file.json"),
                    "/dev/mst/mt4129_pciconf0"
                )
                .is_err()
        );
    }
}
