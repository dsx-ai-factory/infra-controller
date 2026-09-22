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

use carbide_test_support::value_scenarios;
use libmlx::profile::error::MlxProfileError;
use libmlx::profile::profile::MlxConfigProfile;
use libmlx::profile::serialization::{
    SerializableProfile, deserialize_option_profile_map, serialize_option_profile_map,
};
use libmlx::registry::registries;
use rpc::protos::mlx_device::SerializableMlxConfigProfile as SerializableMlxConfigProfilePb;
use serde::{Deserialize, Serialize};

// assert_same_profile checks the four identity fields (name, registry, description,
// and the config size) line up between two profiles. Every roundtrip test below --
// YAML/JSON/TOML, file or string, protobuf -- ends with this same comparison, so it
// lives here once.
fn assert_same_profile(got: &SerializableProfile, want: &SerializableProfile) {
    assert_eq!(got.name, want.name);
    assert_eq!(got.registry_name, want.registry_name);
    assert_eq!(got.description, want.description);
    assert_eq!(got.config.len(), want.config.len());
}

// to_proto hands the profile to the crate's own conversion, `impl
// TryFrom<&SerializableProfile> for SerializableMlxConfigProfilePb`. These helpers used to
// re-create that conversion here so the tests could assert the wire shape independently --
// but re-creating it meant the real impl was never exercised by anything, which is the
// opposite of what these tests are for.
fn to_proto(profile: &SerializableProfile) -> SerializableMlxConfigProfilePb {
    profile
        .try_into()
        .expect("a profile built in these tests should convert to its protobuf form")
}

// from_proto is the same idea in the other direction, through `impl
// TryFrom<SerializableMlxConfigProfilePb> for SerializableProfile`.
fn from_proto(proto: SerializableMlxConfigProfilePb) -> SerializableProfile {
    proto
        .try_into()
        .expect("a protobuf built in these tests should convert back to a profile")
}

#[test]
fn test_serializable_profile_creation() {
    let profile = SerializableProfile::new("test_profile", "test_registry")
        .with_description("Test profile for unit tests")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 42)
        .with_config("STRING_VAR", "test_value");

    assert_eq!(profile.name, "test_profile");
    assert_eq!(profile.registry_name, "test_registry");
    assert_eq!(
        profile.description,
        Some("Test profile for unit tests".to_string())
    );
    assert_eq!(profile.config.len(), 3);
}

#[test]
fn test_yaml_serialization_roundtrip() {
    let original = SerializableProfile::new("yaml_test", "test_registry")
        .with_description("YAML test profile")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 123)
        .with_config("STRING_VAR", "hello");

    // Serialize to YAML
    let yaml = original.to_yaml().expect("Should serialize to YAML");

    // Verify YAML contains expected content
    assert!(yaml.contains("name: yaml_test"));
    assert!(yaml.contains("registry_name: test_registry"));
    assert!(yaml.contains("description: YAML test profile"));
    assert!(yaml.contains("BOOL_VAR: true"));
    assert!(yaml.contains("INT_VAR: 123"));
    assert!(yaml.contains("STRING_VAR: hello"));

    // Deserialize back from YAML and verify round-trip integrity
    let deserialized = SerializableProfile::from_yaml(&yaml).expect("Should deserialize from YAML");
    assert_same_profile(&deserialized, &original);
}

#[test]
fn test_json_serialization_roundtrip() {
    let original = SerializableProfile::new("json_test", "test_registry")
        .with_config("BOOL_VAR", false)
        .with_config("INT_VAR", 456);

    // Serialize to JSON
    let json = original.to_json().expect("Should serialize to JSON");

    // Verify JSON contains expected content
    assert!(json.contains(r#""name": "json_test""#));
    assert!(json.contains(r#""registry_name": "test_registry""#));
    assert!(json.contains(r#""BOOL_VAR": false"#));
    assert!(json.contains(r#""INT_VAR": 456"#));

    // Deserialize back from JSON and verify round-trip integrity
    let deserialized = SerializableProfile::from_json(&json).expect("Should deserialize from JSON");
    assert_same_profile(&deserialized, &original);
}

#[test]
fn test_yaml_with_arrays() {
    let yaml = r#"
name: array_test
registry_name: test_registry
description: Testing array serialization
config:
  SIMPLE_ARRAY:
    - value1
    - value2
    - value3
  SPARSE_ARRAY:
    - first
    - "-"
    - third
    - "-"
  BOOL_ARRAY:
    - true
    - false
    - true
  INT_ARRAY:
    - 10
    - 20
    - 30
"#;

    let profile = SerializableProfile::from_yaml(yaml).expect("Should parse YAML with arrays");

    assert_eq!(profile.name, "array_test");
    assert_eq!(profile.config.len(), 4);

    // Verify array parsing
    assert!(profile.config.contains_key("SIMPLE_ARRAY"));
    assert!(profile.config.contains_key("SPARSE_ARRAY"));
    assert!(profile.config.contains_key("BOOL_ARRAY"));
    assert!(profile.config.contains_key("INT_ARRAY"));
}

#[test]
fn test_minimal_profile() {
    let yaml = r#"
name: minimal
registry_name: basic_registry
config:
  SINGLE_VAR: true
"#;

    let profile = SerializableProfile::from_yaml(yaml).expect("Should parse minimal YAML");

    assert_eq!(profile.name, "minimal");
    assert_eq!(profile.registry_name, "basic_registry");
    assert!(profile.description.is_none());
    assert_eq!(profile.config.len(), 1);
}

#[test]
fn test_invalid_yaml() {
    let invalid_yaml = r#"
name: broken
this is not valid yaml: [
"#;

    let result = SerializableProfile::from_yaml(invalid_yaml);
    assert!(matches!(result, Err(MlxProfileError::YamlParsing { .. })));
}

#[test]
fn test_invalid_json() {
    let invalid_json = r#"{"name": "broken", "missing_comma" "value"}"#;

    let result = SerializableProfile::from_json(invalid_json);
    assert!(matches!(result, Err(MlxProfileError::JsonParsing { .. })));
}

#[test]
fn test_try_from_protobuf_basic() {
    let proto_config: HashMap<String, String> = [
        ("BOOL_VAR".to_string(), "true".to_string()),
        ("INT_VAR".to_string(), "789".to_string()),
        ("STRING_VAR".to_string(), "proto_value".to_string()),
    ]
    .into();

    let proto = SerializableMlxConfigProfilePb {
        name: "proto_test".to_string(),
        registry_name: "test_registry".to_string(),
        description: Some("Testing protobuf conversion".to_string()),
        config: proto_config,
    };

    let profile = from_proto(proto);

    assert_eq!(profile.name, "proto_test");
    assert_eq!(profile.registry_name, "test_registry");
    assert_eq!(
        profile.description,
        Some("Testing protobuf conversion".to_string())
    );
    assert_eq!(profile.config.len(), 3);

    // Each config string parses into the YAML value variant its content implies.
    fn variant(value: Option<&serde_yaml::Value>) -> &'static str {
        match value {
            Some(serde_yaml::Value::Bool(_)) => "bool",
            Some(serde_yaml::Value::Number(_)) => "number",
            Some(serde_yaml::Value::String(_)) => "string",
            _ => "other",
        }
    }
    value_scenarios!(
        run = |key| variant(profile.config.get(key));
        "BOOL_VAR parses to a bool" {
            "BOOL_VAR" => "bool",
        }

        "INT_VAR parses to a number" {
            "INT_VAR" => "number",
        }

        "STRING_VAR parses to a string" {
            "STRING_VAR" => "string",
        }
    );
}

#[test]
fn test_try_into_protobuf_basic() {
    let original = SerializableProfile::new("proto_test", "test_registry")
        .with_description("Testing protobuf conversion")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 789)
        .with_config("STRING_VAR", "proto_value");

    let proto = to_proto(&original);

    assert_eq!(proto.name, "proto_test");
    assert_eq!(proto.registry_name, "test_registry");
    assert_eq!(
        proto.description,
        Some("Testing protobuf conversion".to_string())
    );
    assert_eq!(proto.config.len(), 3);

    // Verify the config values were serialized correctly
    assert_eq!(proto.config.get("BOOL_VAR"), Some(&"true".to_string()));
    assert_eq!(proto.config.get("INT_VAR"), Some(&"789".to_string()));
    assert_eq!(
        proto.config.get("STRING_VAR"),
        Some(&"proto_value".to_string())
    );
}

#[test]
fn test_protobuf_roundtrip() {
    let original = SerializableProfile::new("roundtrip_test", "test_registry")
        .with_description("Testing full roundtrip")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 42)
        .with_config("STRING_VAR", "test_value");

    // Convert to protobuf and back, then verify roundtrip integrity.
    let roundtrip = from_proto(to_proto(&original));
    assert_same_profile(&roundtrip, &original);
}

#[test]
fn test_protobuf_empty_description() {
    let proto = SerializableMlxConfigProfilePb {
        name: "no_desc_test".to_string(),
        registry_name: "test_registry".to_string(),
        description: None,      // No description set, so it should be None
        config: HashMap::new(), // Simplified for this test
    };

    assert_eq!(proto.description, None); // None in protobuf

    // Convert back from protobuf -- None should survive the round trip.
    let converted_back = from_proto(proto);
    assert_eq!(converted_back.description, None);
}

#[test]
fn test_edge_cases() {
    // Test empty profile
    let empty = SerializableProfile::new("empty", "registry");
    let yaml = empty.to_yaml().expect("Should serialize empty profile");
    let deserialized =
        SerializableProfile::from_yaml(&yaml).expect("Should deserialize empty profile");
    assert_eq!(deserialized.config.len(), 0);

    // Test profile with no description
    let no_desc = SerializableProfile::new("no_desc", "registry").with_config("VAR", "value");
    let yaml = no_desc.to_yaml().expect("Should serialize");
    assert!(!yaml.contains("description:"));

    // An explicitly-empty description is retained as Some("") on the profile.
    let empty_desc = SerializableProfile::new("empty_desc", "registry")
        .with_description("")
        .with_config("VAR", "value");
    assert_eq!(empty_desc.description, Some("".to_string()));
}

#[test]
fn test_toml_serialization_roundtrip() {
    let original = SerializableProfile::new("toml_test", "test_registry")
        .with_description("TOML test profile")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 123)
        .with_config("STRING_VAR", "hello");

    // Serialize to TOML
    let toml = original.to_toml().expect("Should serialize to TOML");

    // Verify TOML contains expected content
    assert!(toml.contains(r#"name = "toml_test""#));
    assert!(toml.contains(r#"registry_name = "test_registry""#));
    assert!(toml.contains(r#"description = "TOML test profile""#));

    // TOML config section
    assert!(toml.contains("[config]"));
    assert!(toml.contains("BOOL_VAR = true"));
    assert!(toml.contains("INT_VAR = 123"));
    assert!(toml.contains(r#"STRING_VAR = "hello""#));

    // Deserialize back from TOML and verify round-trip integrity
    let deserialized = SerializableProfile::from_toml(&toml).expect("Should deserialize from TOML");
    assert_same_profile(&deserialized, &original);
}

#[test]
fn test_toml_with_nested_config() {
    let toml = r#"
name = "nested_test"
registry_name = "test_registry"
description = "Testing nested TOML config"

[config]
SRIOV_EN = true
NUM_OF_VFS = 16
POWER_MODE = "high"
ROCE_RTT_RESP_DSCP_P1 = 46
"#;

    let profile =
        SerializableProfile::from_toml(toml).expect("Should parse TOML with nested config");

    assert_eq!(profile.name, "nested_test");
    assert_eq!(profile.registry_name, "test_registry");
    assert_eq!(
        profile.description,
        Some("Testing nested TOML config".to_string())
    );
    assert_eq!(profile.config.len(), 4);

    // Verify nested config parsing
    assert!(profile.config.contains_key("SRIOV_EN"));
    assert!(profile.config.contains_key("NUM_OF_VFS"));
    assert!(profile.config.contains_key("POWER_MODE"));
    assert!(profile.config.contains_key("ROCE_RTT_RESP_DSCP_P1"));
}

#[test]
fn test_toml_with_arrays() {
    let toml = r#"
name = "array_test"
registry_name = "test_registry"
description = "Testing TOML arrays"

[config]
SIMPLE_ARRAY = ["first", "second", "third"]
SPARSE_ARRAY = ["val1", "-", "val3", "-"]
BOOL_ARRAY = [true, false, true]
INT_ARRAY = [10, 20, 30]
PCI_DOWNSTREAM_PORT_OWNER = ["EMBEDDED_CPU", "HOST_0", "HOST_1", "DEVICE_DEFAULT"]
"#;

    let profile = SerializableProfile::from_toml(toml).expect("Should parse TOML with arrays");

    assert_eq!(profile.name, "array_test");
    assert_eq!(profile.config.len(), 5);

    // Verify array parsing
    assert!(profile.config.contains_key("SIMPLE_ARRAY"));
    assert!(profile.config.contains_key("SPARSE_ARRAY"));
    assert!(profile.config.contains_key("BOOL_ARRAY"));
    assert!(profile.config.contains_key("INT_ARRAY"));
    assert!(profile.config.contains_key("PCI_DOWNSTREAM_PORT_OWNER"));
}

#[test]
fn test_toml_minimal_profile() {
    let toml = r#"
name = "minimal"
registry_name = "basic_registry"

[config]
SINGLE_VAR = true
"#;

    let profile = SerializableProfile::from_toml(toml).expect("Should parse minimal TOML");

    assert_eq!(profile.name, "minimal");
    assert_eq!(profile.registry_name, "basic_registry");
    assert!(profile.description.is_none());
    assert_eq!(profile.config.len(), 1);
}

#[test]
fn test_toml_production_like_config() {
    let toml = r#"
name = "bluefield-production"
registry_name = "mlx_generic"
description = "Production configuration for BlueField3 DPUs in datacenter"

[config]
SRIOV_EN = true
NUM_OF_VFS = 16
NUM_OF_PF = 2
INTERNAL_CPU_OFFLOAD_ENGINE = "ENABLED"
ROCE_ADAPTIVE_ROUTING_EN = false
TX_SCHEDULER_LOCALITY_MODE = "STATIC_MODE"
MULTIPATH_DSCP = "DSCP_1"
ROCE_RTT_RESP_DSCP_P1 = 46
ROCE_RTT_RESP_DSCP_MODE_P1 = "FIXED_VALUE"

# Array configuration for PCI downstream ports
PCI_DOWNSTREAM_PORT_OWNER = [
    "EMBEDDED_CPU",  # Port 0
    "HOST_0",        # Port 1
    "HOST_1",        # Port 2
    "HOST_0",        # Port 3
    "HOST_1",        # Port 4
    "DEVICE_DEFAULT", # Port 5
    "DEVICE_DEFAULT", # Port 6-15 use defaults
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT"
]
"#;

    let profile =
        SerializableProfile::from_toml(toml).expect("Should parse production-like TOML config");

    assert_eq!(profile.name, "bluefield-production");
    assert_eq!(profile.registry_name, "mlx_generic");
    assert!(profile.description.is_some());
    assert_eq!(profile.config.len(), 10);

    // Verify specific values
    if let Some(serde_yaml::Value::Bool(sriov)) = profile.config.get("SRIOV_EN") {
        assert!(*sriov);
    } else {
        panic!("SRIOV_EN should be a boolean true");
    }

    if let Some(serde_yaml::Value::Number(vfs)) = profile.config.get("NUM_OF_VFS") {
        assert_eq!(vfs.as_i64(), Some(16));
    } else {
        panic!("NUM_OF_VFS should be number 16");
    }

    if let Some(serde_yaml::Value::String(mode)) = profile.config.get("INTERNAL_CPU_OFFLOAD_ENGINE")
    {
        assert_eq!(mode, "ENABLED");
    } else {
        panic!("INTERNAL_CPU_OFFLOAD_ENGINE should be string 'ENABLED'");
    }
}

#[test]
fn test_toml_serialization_preserves_structure() {
    let original = SerializableProfile::new("structure_test", "test_registry")
        .with_description("Test structure preservation")
        .with_config("VAR1", "value1")
        .with_config("VAR2", 42);

    let toml = original.to_toml().expect("Should serialize");

    // Should have proper TOML structure with [config] section
    assert!(toml.contains("name ="));
    assert!(toml.contains("registry_name ="));
    assert!(toml.contains("description ="));
    assert!(toml.contains("[config]"));

    // Variables should be in config section
    let config_section_start = toml.find("[config]").expect("Should have config section");
    let config_part = &toml[config_section_start..];
    assert!(config_part.contains("VAR1"));
    assert!(config_part.contains("VAR2"));
}

#[test]
fn test_invalid_toml() {
    let invalid_toml = r#"
name = "broken"
this is not valid toml syntax [
missing quotes and brackets
"#;

    // TOML errors get wrapped in MlxProfileError::Serialization
    let result = SerializableProfile::from_toml(invalid_toml);
    assert!(matches!(result, Err(MlxProfileError::Serialization { .. })));
}

#[test]
fn test_yaml_file_operations() {
    use tempfile::tempdir;

    let temp_dir = tempdir().expect("Should create temp dir");

    let profile = SerializableProfile::new("yaml_file_test", "test_registry")
        .with_description("Testing YAML file operations")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 789)
        .with_config("STRING_VAR", "yaml_file_value");

    // Test YAML file operations
    let yaml_path = temp_dir.path().join("test_profile.yaml");
    profile
        .to_yaml_file(&yaml_path)
        .expect("Should write YAML file");

    // Verify file was created and has content
    assert!(yaml_path.exists());
    let file_content = std::fs::read_to_string(&yaml_path).expect("Should read file");
    assert!(file_content.contains("name: yaml_file_test"));
    assert!(file_content.contains("BOOL_VAR: true"));

    // Load back from file and verify round-trip integrity
    let loaded_profile =
        SerializableProfile::from_yaml_file(&yaml_path).expect("Should read YAML file");
    assert_same_profile(&loaded_profile, &profile);

    // Verify specific config values
    assert!(loaded_profile.config.contains_key("BOOL_VAR"));
    assert!(loaded_profile.config.contains_key("INT_VAR"));
    assert!(loaded_profile.config.contains_key("STRING_VAR"));
}

#[test]
fn test_json_file_operations() {
    use tempfile::tempdir;

    let temp_dir = tempdir().expect("Should create temp dir");

    let profile = SerializableProfile::new("json_file_test", "test_registry")
        .with_description("Testing JSON file operations")
        .with_config("BOOL_VAR", false)
        .with_config("INT_VAR", 456)
        .with_config("STRING_VAR", "json_file_value");

    // Test JSON file operations
    let json_path = temp_dir.path().join("test_profile.json");
    profile
        .to_json_file(&json_path)
        .expect("Should write JSON file");

    // Verify file was created and has content
    assert!(json_path.exists());
    let file_content = std::fs::read_to_string(&json_path).expect("Should read file");
    assert!(file_content.contains(r#""name": "json_file_test""#));
    assert!(file_content.contains(r#""BOOL_VAR": false"#));

    // Load back from file and verify round-trip integrity
    let loaded_profile =
        SerializableProfile::from_json_file(&json_path).expect("Should read JSON file");
    assert_same_profile(&loaded_profile, &profile);

    // Verify specific config values
    assert!(loaded_profile.config.contains_key("BOOL_VAR"));
    assert!(loaded_profile.config.contains_key("INT_VAR"));
    assert!(loaded_profile.config.contains_key("STRING_VAR"));
}

#[test]
fn test_all_file_formats_compatibility() {
    use tempfile::tempdir;

    let temp_dir = tempdir().expect("Should create temp dir");

    // Create a comprehensive profile
    let original = SerializableProfile::new("multi_format_test", "test_registry")
        .with_description("Testing all file format compatibility")
        .with_config("BOOL_VAR", true)
        .with_config("INT_VAR", 123)
        .with_config("STRING_VAR", "multi_format")
        .with_config("ARRAY_VAR", vec!["item1", "item2", "item3"]);

    // Write to all three formats
    let yaml_path = temp_dir.path().join("profile.yaml");
    let json_path = temp_dir.path().join("profile.json");
    let toml_path = temp_dir.path().join("profile.toml");

    original
        .to_yaml_file(&yaml_path)
        .expect("Should write YAML");
    original
        .to_json_file(&json_path)
        .expect("Should write JSON");
    original
        .to_toml_file(&toml_path)
        .expect("Should write TOML");

    // Verify all files exist
    assert!(yaml_path.exists());
    assert!(json_path.exists());
    assert!(toml_path.exists());

    // Load from all three formats
    let from_yaml = SerializableProfile::from_yaml_file(&yaml_path).expect("Should load from YAML");
    let from_json = SerializableProfile::from_json_file(&json_path).expect("Should load from JSON");
    let from_toml = SerializableProfile::from_toml_file(&toml_path).expect("Should load from TOML");

    // All should be equivalent to original
    let profiles = [&from_yaml, &from_json, &from_toml];
    for profile in &profiles {
        assert_same_profile(profile, &original);
    }

    // All should be equivalent to each other
    for (i, profile1) in profiles.iter().enumerate() {
        for profile2 in profiles.iter().skip(i + 1) {
            assert_same_profile(profile1, profile2);
        }
    }
}

#[test]
fn test_file_with_complex_arrays() {
    use tempfile::tempdir;

    let temp_dir = tempdir().expect("Should create temp dir");

    // Create profile with arrays similar to real MLX configs
    let profile = SerializableProfile::new("complex_arrays_test", "mlx_generic")
        .with_description("Testing complex array configurations in files")
        .with_config("SRIOV_EN", true)
        .with_config("NUM_OF_VFS", 16);

    // Add complex array manually
    let mut config = profile.config.clone();
    let pci_ports = vec![
        serde_yaml::Value::String("EMBEDDED_CPU".to_string()),
        serde_yaml::Value::String("HOST_0".to_string()),
        serde_yaml::Value::String("HOST_1".to_string()),
        serde_yaml::Value::String("DEVICE_DEFAULT".to_string()),
    ];
    config.insert(
        "PCI_DOWNSTREAM_PORT_OWNER".to_string(),
        serde_yaml::Value::Sequence(pci_ports),
    );

    let profile_with_arrays = SerializableProfile {
        name: profile.name,
        registry_name: profile.registry_name,
        description: profile.description,
        config,
    };

    // Test with each format
    let yaml_path = temp_dir.path().join("complex.yaml");
    let json_path = temp_dir.path().join("complex.json");
    let toml_path = temp_dir.path().join("complex.toml");

    profile_with_arrays
        .to_yaml_file(&yaml_path)
        .expect("Should write YAML with arrays");
    profile_with_arrays
        .to_json_file(&json_path)
        .expect("Should write JSON with arrays");
    profile_with_arrays
        .to_toml_file(&toml_path)
        .expect("Should write TOML with arrays");

    // Load back and verify array handling
    let from_yaml =
        SerializableProfile::from_yaml_file(&yaml_path).expect("Should load YAML with arrays");
    let from_json =
        SerializableProfile::from_json_file(&json_path).expect("Should load JSON with arrays");
    let from_toml =
        SerializableProfile::from_toml_file(&toml_path).expect("Should load TOML with arrays");

    // Verify arrays are preserved in all formats
    for profile in [&from_yaml, &from_json, &from_toml] {
        assert!(profile.config.contains_key("PCI_DOWNSTREAM_PORT_OWNER"));
        if let Some(serde_yaml::Value::Sequence(ports)) =
            profile.config.get("PCI_DOWNSTREAM_PORT_OWNER")
        {
            assert_eq!(ports.len(), 4);
            if let serde_yaml::Value::String(first_port) = &ports[0] {
                assert_eq!(first_port, "EMBEDDED_CPU");
            }
        } else {
            panic!("PCI_DOWNSTREAM_PORT_OWNER should be an array");
        }
    }
}

#[test]
fn test_file_error_handling() {
    use tempfile::tempdir;

    let temp_dir = tempdir().expect("Should create temp dir");

    // Test reading non-existent files
    let missing_yaml = temp_dir.path().join("missing.yaml");
    let missing_json = temp_dir.path().join("missing.json");
    let missing_toml = temp_dir.path().join("missing.toml");

    assert!(SerializableProfile::from_yaml_file(&missing_yaml).is_err());
    assert!(SerializableProfile::from_json_file(&missing_json).is_err());
    assert!(SerializableProfile::from_toml_file(&missing_toml).is_err());

    // Create files with invalid content and confirm parsing fails.
    let invalid_yaml = temp_dir.path().join("invalid.yaml");
    let invalid_json = temp_dir.path().join("invalid.json");
    let invalid_toml = temp_dir.path().join("invalid.toml");

    std::fs::write(&invalid_yaml, "invalid: yaml: content: [[[")
        .expect("Should write invalid YAML");
    std::fs::write(&invalid_json, r#"{"invalid": json: content}"#)
        .expect("Should write invalid JSON");
    std::fs::write(&invalid_toml, "invalid = toml content [[[").expect("Should write invalid TOML");

    assert!(SerializableProfile::from_yaml_file(&invalid_yaml).is_err());
    assert!(SerializableProfile::from_json_file(&invalid_json).is_err());
    assert!(SerializableProfile::from_toml_file(&invalid_toml).is_err());
}

#[test]
fn test_file_paths_with_extensions() {
    use tempfile::tempdir;

    let temp_dir = tempdir().expect("Should create temp dir");

    let profile = SerializableProfile::new("extension_test", "test_registry")
        .with_description("Testing various file extensions")
        .with_config("TEST_VAR", "extension_value");

    // Test various extensions work correctly
    let paths_and_methods = [
        (temp_dir.path().join("profile.yaml"), "yaml"),
        (temp_dir.path().join("profile.yml"), "yaml"),
        (temp_dir.path().join("profile.json"), "json"),
        (temp_dir.path().join("profile.toml"), "toml"),
        (temp_dir.path().join("profile.tml"), "toml"),
        // Test without extensions too
        (temp_dir.path().join("profile_yaml"), "yaml"),
        (temp_dir.path().join("profile_json"), "json"),
        (temp_dir.path().join("profile_toml"), "toml"),
    ];

    for (path, format) in &paths_and_methods {
        match *format {
            "yaml" => {
                profile.to_yaml_file(path).expect("Should write YAML");
                let loaded = SerializableProfile::from_yaml_file(path).expect("Should read YAML");
                assert_eq!(loaded.name, profile.name);
            }
            "json" => {
                profile.to_json_file(path).expect("Should write JSON");
                let loaded = SerializableProfile::from_json_file(path).expect("Should read JSON");
                assert_eq!(loaded.name, profile.name);
            }
            "toml" => {
                profile.to_toml_file(path).expect("Should write TOML");
                let loaded = SerializableProfile::from_toml_file(path).expect("Should read TOML");
                assert_eq!(loaded.name, profile.name);
            }
            _ => unreachable!(),
        }

        assert!(path.exists(), "File should exist: {}", path.display());
    }
}

#[test]
fn test_option_hashmap_deserialize_with_profiles() {
    #[derive(Deserialize)]
    struct TestConfig {
        #[serde(
            default,
            rename = "mlx-config-profiles",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    let toml = r#"
[mlx-config-profiles.test-profile-1]
name = "test-profile-1"
registry_name = "mlx_generic"
description = "First test profile"

[mlx-config-profiles.test-profile-1.config]
SRIOV_EN = true
NUM_OF_VFS = 8

[mlx-config-profiles.test-profile-2]
name = "test-profile-2"
registry_name = "mlx_generic"
description = "Second test profile"

[mlx-config-profiles.test-profile-2.config]
SRIOV_EN = false
NUM_OF_VFS = 4
"#;

    let config: TestConfig = toml::from_str(toml).expect("should deserialize TOML with profiles");

    // Verify we got Some(HashMap) with 2 profiles.
    assert!(config.mlx_config_profiles.is_some());
    let profiles = config.mlx_config_profiles.as_ref().unwrap();
    assert_eq!(profiles.len(), 2);

    // Verify profile 1.
    let profile1 = profiles
        .get("test-profile-1")
        .expect("should have profile 1");
    assert_eq!(profile1.name, "test-profile-1");
    assert_eq!(profile1.registry.name, "mlx_generic");
    assert_eq!(profile1.description, Some("First test profile".to_string()));
    assert_eq!(profile1.variable_count(), 2);

    // Verify profile 2.
    let profile2 = profiles
        .get("test-profile-2")
        .expect("should have profile 2");
    assert_eq!(profile2.name, "test-profile-2");
    assert_eq!(profile2.registry.name, "mlx_generic");
    assert_eq!(
        profile2.description,
        Some("Second test profile".to_string())
    );
    assert_eq!(profile2.variable_count(), 2);
}

#[test]
fn test_option_hashmap_deserialize_missing_field() {
    #[derive(Deserialize)]
    struct TestConfig {
        some_other_field: String,
        #[serde(
            default,
            rename = "mlx-config-profiles",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    let toml = r#"
some_other_field = "test"
# No mlx-config-profiles section at all
"#;

    let config: TestConfig =
        toml::from_str(toml).expect("should deserialize TOML without profiles section");

    // Verify we got None because field was missing and
    // we used the default attribute.
    assert!(config.mlx_config_profiles.is_none());
    assert_eq!(config.some_other_field, "test");
}

#[test]
fn test_option_hashmap_deserialize_empty_section() {
    #[derive(Deserialize)]
    struct TestConfig {
        #[serde(
            default,
            rename = "mlx-config-profiles",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    let toml = r#"
[mlx-config-profiles]
# Empty section - no actual profiles defined
"#;

    let config: TestConfig =
        toml::from_str(toml).expect("should deserialize empty profiles section");

    // Should get Some(empty HashMap) not None,
    // because the section exists.
    assert!(config.mlx_config_profiles.is_some());
    let profiles = config.mlx_config_profiles.unwrap();
    assert_eq!(profiles.len(), 0);
}

#[test]
fn test_option_hashmap_serialize_with_profiles() {
    #[derive(Serialize)]
    struct TestConfig {
        #[serde(
            default,
            rename = "mlx-config-profiles",
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    // Create some test profiles
    let registry = registries::get("mlx_generic")
        .expect("should have mlx_generic registry")
        .clone();

    let profile1 = MlxConfigProfile::new("test-profile-1", registry.clone())
        .with_description("First test profile")
        .with("SRIOV_EN", true)
        .expect("should add config")
        .with("NUM_OF_VFS", 8)
        .expect("should add config");

    let profile2 = MlxConfigProfile::new("test-profile-2", registry)
        .with_description("Second test profile")
        .with("SRIOV_EN", false)
        .expect("should add config")
        .with("NUM_OF_VFS", 4)
        .expect("should add config");

    let mut profiles = HashMap::new();
    profiles.insert("test-profile-1".to_string(), profile1);
    profiles.insert("test-profile-2".to_string(), profile2);

    let config = TestConfig {
        mlx_config_profiles: Some(profiles),
    };

    // Serialize to TOML
    let toml = toml::to_string_pretty(&config).expect("should serialize to TOML");

    // Verify the TOML contains our profiles
    assert!(toml.contains("[mlx-config-profiles.test-profile-1]"));
    assert!(toml.contains("[mlx-config-profiles.test-profile-2]"));
    assert!(toml.contains("name = \"test-profile-1\""));
    assert!(toml.contains("name = \"test-profile-2\""));
    assert!(toml.contains("registry_name = \"mlx_generic\""));
}

#[test]
fn test_option_hashmap_serialize_none() {
    #[derive(Serialize)]
    struct TestConfig {
        some_field: String,
        #[serde(
            default,
            rename = "mlx-config-profiles",
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    let config = TestConfig {
        some_field: "test".to_string(),
        mlx_config_profiles: None,
    };

    // Serialize to TOML.
    let toml = toml::to_string_pretty(&config).expect("should serialize to TOML");

    // Verify the profiles section is NOT in the output
    // (skip_serializing_if works).
    assert!(!toml.contains("mlx-config-profiles"));
    assert!(toml.contains("some_field = \"test\""));
}

#[test]
fn test_option_hashmap_roundtrip() {
    #[derive(Serialize, Deserialize)]
    struct TestConfig {
        #[serde(
            default,
            rename = "mlx-config-profiles",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_option_profile_map",
            serialize_with = "serialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    // Create original config.
    let registry = registries::get("mlx_generic")
        .expect("Should have mlx_generic registry")
        .clone();

    let profile = MlxConfigProfile::new("roundtrip-test", registry)
        .with_description("testing roundtrip")
        .with("SRIOV_EN", true)
        .expect("should add config")
        .with("NUM_OF_VFS", 16)
        .expect("should add config");

    let mut profiles = HashMap::new();
    profiles.insert("roundtrip-test".to_string(), profile);

    let original_config = TestConfig {
        mlx_config_profiles: Some(profiles),
    };

    // Serialize to TOML.
    let toml = toml::to_string_pretty(&original_config).expect("should serialize");

    // Deserialize back.
    let roundtrip_config: TestConfig = toml::from_str(&toml).expect("should deserialize");

    // Verify roundtrip worked.
    assert!(roundtrip_config.mlx_config_profiles.is_some());
    let roundtrip_profiles = roundtrip_config.mlx_config_profiles.as_ref().unwrap();
    assert_eq!(roundtrip_profiles.len(), 1);

    let roundtrip_profile = roundtrip_profiles
        .get("roundtrip-test")
        .expect("should have profile");
    assert_eq!(roundtrip_profile.name, "roundtrip-test");
    assert_eq!(roundtrip_profile.registry.name, "mlx_generic");
    assert_eq!(roundtrip_profile.variable_count(), 2);
}

#[test]
fn test_option_hashmap_with_multiple_profiles_and_arrays() {
    #[derive(Deserialize)]
    struct TestConfig {
        #[serde(
            default,
            rename = "mlx-config-profiles",
            deserialize_with = "deserialize_option_profile_map"
        )]
        mlx_config_profiles: Option<HashMap<String, MlxConfigProfile>>,
    }

    let toml = r#"
[mlx-config-profiles.profile-with-arrays]
name = "profile-with-arrays"
registry_name = "mlx_generic"
description = "Profile testing array support"

[mlx-config-profiles.profile-with-arrays.config]
SRIOV_EN = true
NUM_OF_VFS = 8
# PCI_DOWNSTREAM_PORT_OWNER needs exactly 16 elements
PCI_DOWNSTREAM_PORT_OWNER = [
    "EMBEDDED_CPU",
    "HOST_0",
    "HOST_1",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT",
    "DEVICE_DEFAULT"
]

[mlx-config-profiles.simple-profile]
name = "simple-profile"
registry_name = "mlx_generic"

[mlx-config-profiles.simple-profile.config]
SRIOV_EN = false
NUM_OF_VFS = 2
"#;

    let config: TestConfig = toml::from_str(toml).expect("should deserialize");

    assert!(config.mlx_config_profiles.is_some());
    let profiles = config.mlx_config_profiles.as_ref().unwrap();
    assert_eq!(profiles.len(), 2);

    // Verify profile with arrays
    let array_profile = profiles
        .get("profile-with-arrays")
        .expect("should have array profile");
    assert_eq!(array_profile.variable_count(), 3);

    // Verify simple profile
    let simple_profile = profiles
        .get("simple-profile")
        .expect("should have simple profile");
    assert_eq!(simple_profile.variable_count(), 2);
}
