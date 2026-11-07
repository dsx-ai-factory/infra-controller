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

use libmlx::device::filters::{DeviceField, DeviceFilter, MatchMode};
use libmlx::variables::registry::MlxVariableRegistry;
use libmlx::variables::spec::MlxVariableSpec;
use libmlx::variables::variable::MlxConfigVariable;

// create_test_variables creates a set of test variables for registry testing.
fn create_test_variables() -> Vec<MlxConfigVariable> {
    vec![
        MlxConfigVariable::builder()
            .name("BOOL_VAR".to_string())
            .description("A boolean variable".to_string())
            .read_only(false)
            .spec(MlxVariableSpec::Boolean)
            .build(),
        MlxConfigVariable::builder()
            .name("INT_VAR".to_string())
            .description("An integer variable".to_string())
            .read_only(false)
            .spec(MlxVariableSpec::Integer)
            .build(),
        MlxConfigVariable::builder()
            .name("ENUM_VAR".to_string())
            .description("An enum variable".to_string())
            .read_only(false)
            .spec(MlxVariableSpec::Enum {
                options: vec!["low".to_string(), "medium".to_string(), "high".to_string()],
            })
            .build(),
        MlxConfigVariable::builder()
            .name("ARRAY_VAR".to_string())
            .description("An integer array variable".to_string())
            .read_only(false)
            .spec(MlxVariableSpec::IntegerArray { size: 4 })
            .build(),
    ]
}

#[test]
fn test_registry_serde_serialization_no_filters() {
    let variables = create_test_variables();
    let registry = MlxVariableRegistry::new("serde_test").variables(variables);

    // Test JSON serialization
    let json = serde_json::to_string(&registry).expect("JSON serialization failed");
    let deserialized: MlxVariableRegistry =
        serde_json::from_str(&json).expect("JSON deserialization failed");

    assert_eq!(registry.name, deserialized.name);
    assert_eq!(registry.variables.len(), deserialized.variables.len());
    assert!(!deserialized.has_filters());

    // Test YAML serialization
    let yaml = serde_yaml::to_string(&registry).expect("YAML serialization failed");
    let yaml_deserialized: MlxVariableRegistry =
        serde_yaml::from_str(&yaml).expect("YAML deserialization failed");

    assert_eq!(registry.name, yaml_deserialized.name);
    assert_eq!(registry.variables.len(), yaml_deserialized.variables.len());
    assert!(!yaml_deserialized.has_filters());
}

#[test]
fn test_registry_serde_serialization_with_filters() {
    let variables = create_test_variables();
    let registry = MlxVariableRegistry::new("serde_filter_test")
        .variables(variables)
        .with_filter(DeviceFilter {
            field: DeviceField::DeviceType,
            values: vec!["BlueField3".to_string()],
            match_mode: MatchMode::Exact,
        });

    // Test JSON serialization with filters
    let json = serde_json::to_string(&registry).expect("JSON serialization failed");
    let deserialized: MlxVariableRegistry =
        serde_json::from_str(&json).expect("JSON deserialization failed");

    assert_eq!(registry.name, deserialized.name);
    assert_eq!(registry.variables.len(), deserialized.variables.len());
    assert!(deserialized.has_filters());
    assert_eq!(deserialized.filters.as_ref().unwrap().filters.len(), 1);

    // Test YAML serialization with filters
    let yaml = serde_yaml::to_string(&registry).expect("YAML serialization failed");
    let yaml_deserialized: MlxVariableRegistry =
        serde_yaml::from_str(&yaml).expect("YAML deserialization failed");

    assert_eq!(registry.name, yaml_deserialized.name);
    assert_eq!(registry.variables.len(), yaml_deserialized.variables.len());
    assert!(yaml_deserialized.has_filters());
    assert_eq!(yaml_deserialized.filters.as_ref().unwrap().filters.len(), 1);
}

#[test]
fn test_registry_yaml_format_matches_expected() {
    // Test that we can deserialize the expected YAML format
    let yaml = r#"
name: "test_registry"
filters:
  - field: device_type
    values: ["BlueField3", "ConnectX-6"]
    match_mode: exact
  - field: part_number
    values: ["MCX.*"]
    match_mode: regex
variables:
  - name: "TEST_VAR"
    description: "Test variable"
    read_only: false
    spec:
      type: "boolean"
"#;

    let registry: MlxVariableRegistry =
        serde_yaml::from_str(yaml).expect("Should deserialize expected YAML format");

    assert_eq!(registry.name, "test_registry");
    assert!(registry.has_filters());
    assert_eq!(registry.filters.as_ref().unwrap().filters.len(), 2);
    assert_eq!(registry.variables.len(), 1);
    assert_eq!(registry.variables[0].name, "TEST_VAR");
}
