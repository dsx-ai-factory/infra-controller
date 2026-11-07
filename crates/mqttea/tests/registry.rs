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
// tests/registry.rs
// Unit tests for MqttRegistry functionality including pattern matching, message type
// registration, serialization/deserialization, and registry introspection.

use mqttea::QoS;
use mqttea::errors::MqtteaClientError;
use mqttea::message_types::RawMessage;
use mqttea::registry::types::PublishOptions;
use mqttea::registry::{MessageTypeInfo, MqttRegistry, SerializationFormat};
use mqttea::traits::RawMessageType;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, prost::Message)]
struct HelloWorld {
    #[prost(string, tag = "1")]
    message: String,
    #[prost(int64, tag = "2")]
    timestamp: i64,
    #[prost(string, tag = "3")]
    device_id: String,
}

// Test message types for various serialization formats
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CatInfo {
    name: String,
    age: u8,
    breed: String,
    indoor: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct BirdMessage {
    species: String,
    payload: Vec<u8>,
}

impl RawMessageType for BirdMessage {
    fn to_bytes(&self) -> Vec<u8> {
        format!(
            "{}|{}",
            self.species,
            String::from_utf8_lossy(&self.payload)
        )
        .into_bytes()
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        let content = String::from_utf8_lossy(&bytes);
        if let Some((species, payload_str)) = content.split_once('|') {
            Self {
                species: species.to_string(),
                payload: payload_str.as_bytes().to_vec(),
            }
        } else {
            Self {
                species: "unknown".to_string(),
                payload: bytes,
            }
        }
    }
}

// Helper function to create a test registry
fn create_test_registry() -> MqttRegistry {
    MqttRegistry::new()
}

// Tests for registry creation and basic operations
#[test]
fn test_registry_creation() {
    let registry = create_test_registry();
    assert_eq!(registry.pattern_count(), 0);
    assert_eq!(registry.entry_count(), 0);
    assert!(!registry.has_patterns());
}

#[test]
fn test_registry_clear() {
    let mut registry = create_test_registry();

    // Add some registrations
    registry
        .register_raw_message::<RawMessage>(vec!["hamster-wheels".to_string()], None)
        .unwrap();
    assert!(registry.has_patterns());
    assert!(registry.pattern_count() > 0);

    // Clear and verify empty
    registry.clear();
    assert!(!registry.has_patterns());
    assert_eq!(registry.pattern_count(), 0);
    assert_eq!(registry.entry_count(), 0);
}

// Tests for protobuf message registration
#[test]
fn test_protobuf_registration_single_pattern() {
    let mut registry = create_test_registry();

    let result =
        registry.register_protobuf_message::<HelloWorld>(vec!["rabbit-hops".to_string()], None);

    assert!(result.is_ok());
    assert_eq!(registry.pattern_count(), 1);
    assert_eq!(registry.entry_count(), 1);
    assert!(registry.has_patterns());
}

#[test]
fn test_protobuf_registration_multiple_patterns() {
    let mut registry = create_test_registry();

    let patterns = vec![
        "dog-barks".to_string(),
        "dog-woofs".to_string(),
        "puppy-sounds".to_string(),
    ];

    let result = registry.register_protobuf_message::<HelloWorld>(patterns.clone(), None);

    assert!(result.is_ok());
    assert_eq!(registry.pattern_count(), 3);
    assert_eq!(registry.entry_count(), 1);
}

#[test]
fn test_protobuf_registration_with_qos() {
    let mut registry = create_test_registry();

    let result = registry.register_protobuf_message::<HelloWorld>(
        vec!["urgent-mice".to_string()],
        Some(PublishOptions::default().with_qos(QoS::AtMostOnce)),
    );

    assert!(result.is_ok());

    // Verify QoS is stored correctly
    let type_info = registry.get_type_info::<HelloWorld>().unwrap();
    assert_eq!(
        type_info.publish_options.unwrap().qos,
        Some(QoS::AtMostOnce)
    );
}

// Tests for JSON message registration
#[test]
fn test_json_registration() {
    let mut registry = create_test_registry();

    let result = registry.register_json_message::<CatInfo>(vec!["cat-data".to_string()], None);

    assert!(result.is_ok());
    assert_eq!(registry.pattern_count(), 1);

    // Verify format is set correctly
    let type_info = registry.get_type_info::<CatInfo>().unwrap();
    assert_eq!(type_info.format, SerializationFormat::Json);
}

#[test]
fn test_json_registration_with_qos() {
    let mut registry = create_test_registry();

    let result = registry.register_json_message::<CatInfo>(
        vec!["priority-cats".to_string()],
        Some(PublishOptions::default().with_qos(QoS::AtLeastOnce)),
    );

    assert!(result.is_ok());

    let type_info = registry.get_type_info::<CatInfo>().unwrap();
    assert_eq!(
        type_info.publish_options.unwrap().qos,
        Some(QoS::AtLeastOnce)
    );
    assert_eq!(type_info.format, SerializationFormat::Json);
}

// Tests for YAML message registration
#[test]
fn test_yaml_registration() {
    let mut registry = create_test_registry();

    let result = registry.register_yaml_message::<CatInfo>(vec!["yaml-cats".to_string()], None);

    assert!(result.is_ok());

    let type_info = registry.get_type_info::<CatInfo>().unwrap();
    assert_eq!(type_info.format, SerializationFormat::Yaml);
}

// Tests for raw message registration
#[test]
fn test_raw_registration() {
    let mut registry = create_test_registry();

    let result =
        registry.register_raw_message::<RawMessage>(vec!["fish-bubbles".to_string()], None);

    assert!(result.is_ok());

    let type_info = registry.get_type_info::<RawMessage>().unwrap();
    assert_eq!(type_info.format, SerializationFormat::Raw);
}

#[test]
fn test_custom_raw_registration() {
    let mut registry = create_test_registry();

    let result = registry.register_raw_message::<BirdMessage>(
        vec!["bird-songs".to_string()],
        Some(PublishOptions {
            qos: Some(QoS::AtMostOnce),
            retain: None,
        }),
    );

    assert!(result.is_ok());

    let type_info = registry.get_type_info::<BirdMessage>().unwrap();
    assert_eq!(type_info.format, SerializationFormat::Raw);
    assert_eq!(
        type_info.publish_options.unwrap().qos,
        Some(QoS::AtMostOnce)
    );
}

// Tests for pattern matching
#[test]
fn test_pattern_matching_simple() {
    let mut registry = create_test_registry();

    registry
        .register_raw_message::<RawMessage>(vec!["turtle-moves".to_string()], None)
        .unwrap();

    // Should match simple suffix pattern
    let match_result = registry.find_matching_type_for_topic("/some/path/turtle-moves");
    assert!(match_result.is_some());
    // Fix: Use full qualified type name instead of simple name
    assert!(match_result.unwrap().type_name.contains("RawMessage"));
}

#[test]
fn test_pattern_matching_regex() {
    let mut registry = create_test_registry();

    registry
        .register_raw_message::<RawMessage>(vec!["^/animals/.*".to_string()], None)
        .unwrap();

    // Should match regex pattern
    let match_result = registry.find_matching_type_for_topic("/animals/dogs/barking");
    assert!(match_result.is_some());

    // Should not match non-matching topic
    let no_match = registry.find_matching_type_for_topic("/plants/roses/blooming");
    assert!(no_match.is_none());
}

#[test]
fn test_pattern_matching_first_wins() {
    let mut registry = create_test_registry();

    // Register two patterns that could both match
    registry
        .register_raw_message::<RawMessage>(
            vec![".*".to_string(), "specific-pattern".to_string()],
            None,
        )
        .unwrap();

    // Should match the first pattern (.*)
    let match_result = registry.find_matching_type_for_topic("/test/specific-pattern");
    assert!(match_result.is_some());
    // Fix: Use contains instead of exact match for full qualified type name
    assert!(match_result.unwrap().type_name.contains("RawMessage"));
}

#[test]
fn test_pattern_matching_no_match() {
    let mut registry = create_test_registry();

    registry
        .register_raw_message::<RawMessage>(vec!["specific-pattern".to_string()], None)
        .unwrap();

    // Should not match different pattern
    let no_match = registry.find_matching_type_for_topic("/different/topic");
    assert!(no_match.is_none());
}

// Tests for serialization and deserialization
#[test]
fn test_protobuf_serialization_roundtrip() {
    let mut registry = create_test_registry();

    registry
        .register_protobuf_message::<HelloWorld>(vec!["test".to_string()], None)
        .unwrap();

    let hello = HelloWorld {
        message: "test".to_string(),
        timestamp: 12345,
        device_id: "device1".to_string(),
    };

    let bytes = registry.serialize_message(&hello).unwrap();
    let restored: HelloWorld = registry.deserialize_message(&bytes).unwrap();

    assert_eq!(hello, restored);
}

#[test]
fn test_json_serialization_roundtrip() {
    let mut registry = create_test_registry();

    registry
        .register_json_message::<CatInfo>(vec!["test".to_string()], None)
        .unwrap();

    let cat = CatInfo {
        name: "Fluffy".to_string(),
        age: 3,
        breed: "Persian".to_string(),
        indoor: true,
    };

    let bytes = registry.serialize_message(&cat).unwrap();
    let restored: CatInfo = registry.deserialize_message(&bytes).unwrap();

    assert_eq!(cat, restored);
}

#[test]
fn test_yaml_serialization_roundtrip() {
    let mut registry = create_test_registry();

    registry
        .register_yaml_message::<CatInfo>(vec!["test".to_string()], None)
        .unwrap();

    let cat = CatInfo {
        name: "Whiskers".to_string(),
        age: 5,
        breed: "Maine Coon".to_string(),
        indoor: false,
    };

    let bytes = registry.serialize_message(&cat).unwrap();
    let restored: CatInfo = registry.deserialize_message(&bytes).unwrap();

    assert_eq!(cat, restored);
}

#[test]
fn test_raw_serialization_roundtrip() {
    let mut registry = create_test_registry();

    registry
        .register_raw_message::<BirdMessage>(vec!["test".to_string()], None)
        .unwrap();

    let bird = BirdMessage {
        species: "cardinal".to_string(),
        payload: b"chirp chirp".to_vec(),
    };

    let bytes = registry.serialize_message(&bird).unwrap();
    let restored: BirdMessage = registry.deserialize_message(&bytes).unwrap();

    assert_eq!(bird, restored);
}

// Tests for error conditions
#[test]
fn test_serialize_unregistered_type() {
    let registry = create_test_registry();

    let hello = HelloWorld {
        message: "test".to_string(),
        timestamp: 0,
        device_id: "test".to_string(),
    };

    let result = registry.serialize_message(&hello);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqtteaClientError::UnregisteredType(_)
    ));
}

#[test]
fn test_deserialize_unregistered_type() {
    let registry = create_test_registry();

    let some_bytes = b"random data";
    let result: Result<HelloWorld, _> = registry.deserialize_message(some_bytes);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqtteaClientError::UnregisteredType(_)
    ));
}

#[test]
fn test_invalid_protobuf_deserialization() {
    let mut registry = create_test_registry();

    registry
        .register_protobuf_message::<HelloWorld>(vec!["test".to_string()], None)
        .unwrap();

    // Try to deserialize invalid protobuf data
    let invalid_bytes = b"this is not valid protobuf data";
    let result: Result<HelloWorld, _> = registry.deserialize_message(invalid_bytes);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqtteaClientError::ProtobufDeserializationError(_)
    ));
}

#[test]
fn test_invalid_json_deserialization() {
    let mut registry = create_test_registry();

    registry
        .register_json_message::<CatInfo>(vec!["test".to_string()], None)
        .unwrap();

    // Try to deserialize invalid JSON data
    let invalid_bytes = b"{ invalid json }";
    let result: Result<CatInfo, _> = registry.deserialize_message(invalid_bytes);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqtteaClientError::JsonDeserializationError(_)
    ));
}

#[test]
fn test_invalid_yaml_deserialization() {
    let mut registry = create_test_registry();

    registry
        .register_yaml_message::<CatInfo>(vec!["test".to_string()], None)
        .unwrap();

    // Try to deserialize invalid YAML data
    let invalid_bytes = b"{ invalid: yaml: structure: }}}";
    let result: Result<CatInfo, _> = registry.deserialize_message(invalid_bytes);
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqtteaClientError::YamlDeserializationError(_)
    ));
}

// Tests for regex pattern compilation errors
#[test]
fn test_invalid_regex_pattern() {
    let mut registry = create_test_registry();

    // Invalid regex pattern should cause error
    let result = registry.register_raw_message::<RawMessage>(
        vec!["[invalid regex pattern".to_string()], // Missing closing bracket
        None,
    );

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        MqtteaClientError::PatternCompilationError(_)
    ));
}

// Tests for registry introspection
#[test]
fn test_list_registered_types() {
    let mut registry = create_test_registry();

    // Register multiple types
    registry
        .register_protobuf_message::<HelloWorld>(vec!["proto-messages".to_string()], None)
        .unwrap();

    registry
        .register_json_message::<CatInfo>(
            vec!["json-cats".to_string()],
            Some(PublishOptions {
                qos: Some(QoS::AtMostOnce),
                retain: None,
            }),
        )
        .unwrap();

    registry
        .register_raw_message::<RawMessage>(vec!["raw-data".to_string()], None)
        .unwrap();

    let types = registry.list_registered_types();
    assert_eq!(types.len(), 3);

    // Verify we can find our registered types (using contains for full qualified names)
    let type_names: Vec<String> = types.iter().map(|(name, _)| name.clone()).collect();
    assert!(type_names.iter().any(|name| name.contains("HelloWorld")));
    assert!(type_names.iter().any(|name| name.contains("CatInfo")));
    assert!(type_names.iter().any(|name| name.contains("RawMessage")));
}

#[test]
fn test_get_topic_patterns_for_type() {
    let mut registry = create_test_registry();

    let patterns = vec![
        "snake-hisses".to_string(),
        "snake-slithers".to_string(),
        "serpent-moves".to_string(),
    ];

    registry
        .register_raw_message::<RawMessage>(patterns.clone(), None)
        .unwrap();

    // Fix: Use the actual full qualified type name as stored by the registry
    let full_type_name = std::any::type_name::<RawMessage>();
    let retrieved_patterns = registry.get_topic_patterns_for_type(full_type_name);
    assert_eq!(retrieved_patterns.len(), 3);

    // Patterns should be converted to regex format
    for pattern in retrieved_patterns {
        assert!(pattern.contains("snake") || pattern.contains("serpent"));
    }
}

#[test]
fn test_has_entry_for_type() {
    let mut registry = create_test_registry();

    // Initially no types registered
    assert!(!registry.has_entry_for_type::<HelloWorld>());
    assert!(!registry.has_entry_for_type::<CatInfo>());

    // Register one type
    registry
        .register_protobuf_message::<HelloWorld>(vec!["test-pattern".to_string()], None)
        .unwrap();

    // Now should find the registered type but not others
    assert!(registry.has_entry_for_type::<HelloWorld>());
    assert!(!registry.has_entry_for_type::<CatInfo>());
}

#[test]
fn test_get_type_info_methods() {
    let mut registry = create_test_registry();

    registry
        .register_protobuf_message::<HelloWorld>(
            vec!["hello-patterns".to_string()],
            Some(PublishOptions::default().with_qos(QoS::AtMostOnce)),
        )
        .unwrap();

    // Test get_type_info by generic type
    let type_info = registry.get_type_info::<HelloWorld>();
    assert!(type_info.is_some());
    assert_eq!(
        type_info.unwrap().publish_options.unwrap().qos,
        Some(QoS::AtMostOnce)
    );
    assert_eq!(type_info.unwrap().format, SerializationFormat::Protobuf);

    // Test get_type_info_by_name with full qualified type name
    let full_type_name = std::any::type_name::<HelloWorld>();
    let type_info_by_name = registry.get_type_info_by_name(full_type_name);
    assert!(type_info_by_name.is_some());
    assert_eq!(
        type_info_by_name.unwrap().format,
        SerializationFormat::Protobuf
    );
}

// Tests for MessageTypeInfo utility methods
#[test]
fn test_message_type_info_methods() {
    let info = MessageTypeInfo {
        type_name: "TestType".to_string(),
        patterns: vec!["pattern1".to_string(), "pattern2".to_string()],
        publish_options: Some(PublishOptions::default().with_qos(QoS::AtLeastOnce)),
        format: SerializationFormat::Json,
    };

    assert_eq!(info.pattern_count(), 2);
    assert!(info.has_pattern("pattern1"));
    assert!(info.has_pattern("pattern2"));
    assert!(!info.has_pattern("pattern3"));
    assert!(info.uses_qos_override());
    assert_eq!(info.effective_qos(QoS::AtMostOnce), QoS::AtLeastOnce);
    assert!(info.is_format(SerializationFormat::Json));
    assert!(!info.is_format(SerializationFormat::Protobuf));
}

#[test]
fn test_message_type_info_no_qos_override() {
    let info = MessageTypeInfo {
        type_name: "TestType".to_string(),
        patterns: vec!["pattern".to_string()],
        publish_options: None,
        format: SerializationFormat::Raw,
    };

    assert!(!info.uses_qos_override());
    assert_eq!(info.effective_qos(QoS::ExactlyOnce), QoS::ExactlyOnce); // Uses default
}

// Integration tests combining multiple registry features
#[test]
fn test_multiple_formats_same_registry() {
    let mut registry = create_test_registry();

    // Register different message types with different formats
    registry
        .register_protobuf_message::<HelloWorld>(vec!["pb-messages".to_string()], None)
        .unwrap();

    registry
        .register_json_message::<CatInfo>(vec!["json-messages".to_string()], None)
        .unwrap();

    registry
        .register_raw_message::<BirdMessage>(vec!["raw-messages".to_string()], None)
        .unwrap();

    // All should be findable by pattern matching
    assert!(
        registry
            .find_matching_type_for_topic("/test/pb-messages")
            .is_some()
    );
    assert!(
        registry
            .find_matching_type_for_topic("/test/json-messages")
            .is_some()
    );
    assert!(
        registry
            .find_matching_type_for_topic("/test/raw-messages")
            .is_some()
    );

    // All should be serializable
    let hello = HelloWorld {
        message: "test".to_string(),
        timestamp: 0,
        device_id: "test".to_string(),
    };
    let cat = CatInfo {
        name: "Test".to_string(),
        age: 1,
        breed: "Test".to_string(),
        indoor: true,
    };
    let bird = BirdMessage {
        species: "test".to_string(),
        payload: b"test".to_vec(),
    };

    assert!(registry.serialize_message(&hello).is_ok());
    assert!(registry.serialize_message(&cat).is_ok());
    assert!(registry.serialize_message(&bird).is_ok());
}

#[test]
fn test_complex_pattern_matching_scenarios() {
    let mut registry = create_test_registry();

    // Register patterns with different complexities
    registry
        .register_raw_message::<RawMessage>(
            vec![
                "simple-pattern".to_string(),
                "^/complex/.*\\.data$".to_string(),
                ".*emergency.*".to_string(),
            ],
            None,
        )
        .unwrap();

    // Test various topic matching scenarios
    assert!(
        registry
            .find_matching_type_for_topic("/test/simple-pattern")
            .is_some()
    );
    assert!(
        registry
            .find_matching_type_for_topic("/complex/sensor.data")
            .is_some()
    );
    assert!(
        registry
            .find_matching_type_for_topic("/alerts/emergency/fire")
            .is_some()
    );
    assert!(
        registry
            .find_matching_type_for_topic("/unrelated/topic")
            .is_none()
    );
}
