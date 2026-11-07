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

// src/registry/entry.rs
// MqttRegistryEntry implementation for message registration data
// encapsulation..
//
// Contains the entry abstraction that encapsulates message metadata
// plus serialization handlers as one entry type. Each entry represents
// a complete registration for one message type with all the information
// needed to handle that type.

use super::types::{
    DeserializeHandler, MessageTypeInfo, PublishOptions, SerializationFormat, SerializeHandler,
};
use crate::errors::MqtteaClientError;

// MqttRegistryEntry stores complete registration information for a
// message type. It encapsulates metadata and handlers together.
pub struct MqttRegistryEntry {
    // message_type_info contains the metadata for this
    // registered message type.
    pub message_type_info: MessageTypeInfo,
    // serialize_handler converts typed messages to bytes for
    // MQTT transmission.
    pub serialize_handler: SerializeHandler,
    // deserialize_handler converts received bytes back to
    // typed messages.
    pub deserialize_handler: DeserializeHandler,
}

// Debug implementation for MqttRegistryEntry.
impl std::fmt::Debug for MqttRegistryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MqttRegistryEntry")
            .field("message_type_info", &self.message_type_info)
            .field("serialize_handler", &"<function>")
            .field("deserialize_handler", &"<function>")
            .finish()
    }
}

impl MqttRegistryEntry {
    // type_name returns the human-readable type name for this entry.
    pub fn type_name(&self) -> &str {
        &self.message_type_info.type_name
    }

    // patterns returns the topic patterns for this entry.
    pub fn patterns(&self) -> &[String] {
        &self.message_type_info.patterns
    }

    // publish_options returns the PublishOptions override for this entry.
    pub fn publish_options(&self) -> Option<PublishOptions> {
        self.message_type_info.publish_options
    }

    // qos returns the QoS override for this entry.
    pub fn qos(&self) -> Option<rumqttc::QoS> {
        self.message_type_info
            .publish_options
            .and_then(|opts| opts.qos)
    }

    // qos returns the retain override for this entry.
    pub fn retain(&self) -> Option<bool> {
        self.message_type_info
            .publish_options
            .and_then(|opts| opts.retain)
    }

    // format returns the serialization format for this entry.
    pub fn format(&self) -> SerializationFormat {
        self.message_type_info.format
    }

    // serialize converts a message to bytes using this entry's handler.
    pub fn serialize(&self, message: &dyn std::any::Any) -> Result<Vec<u8>, MqtteaClientError> {
        (self.serialize_handler)(message)
    }

    // deserialize converts bytes to a message using this entry's handler.
    pub fn deserialize(
        &self,
        bytes: &[u8],
    ) -> Result<Box<dyn std::any::Any + Send>, MqtteaClientError> {
        (self.deserialize_handler)(bytes)
    }

    // pattern_count returns the number of patterns registered for this entry.
    pub fn pattern_count(&self) -> usize {
        self.message_type_info.pattern_count()
    }

    // has_pattern checks if this entry is registered for a specific pattern.
    pub fn has_pattern(&self, pattern: &str) -> bool {
        self.message_type_info.has_pattern(pattern)
    }

    // uses_qos_override checks if this entry has a custom QoS setting.
    pub fn uses_qos_override(&self) -> bool {
        self.message_type_info.uses_qos_override()
    }

    // effective_qos returns the QoS that would be used for this entry.
    pub fn effective_qos(&self, default_qos: rumqttc::QoS) -> rumqttc::QoS {
        self.message_type_info.effective_qos(default_qos)
    }

    // is_format checks if this entry uses a specific serialization format.
    pub fn is_format(&self, format: SerializationFormat) -> bool {
        self.message_type_info.is_format(format)
    }

    // validate_serialization_round_trip performs a test serialization and
    // deserialization. Useful for debugging serialization handler implementations.
    pub fn validate_serialization_round_trip<T: 'static + PartialEq + std::fmt::Debug>(
        &self,
        test_message: &T,
    ) -> Result<(), MqtteaClientError> {
        // Serialize the test message
        let bytes = self.serialize(test_message as &dyn std::any::Any)?;

        // Deserialize back to Any
        let any_result = self.deserialize(&bytes)?;

        // Try to downcast back to original type
        if let Ok(restored) = any_result.downcast::<T>() {
            if test_message == &*restored {
                Ok(())
            } else {
                Err(MqtteaClientError::RawMessageError(
                    "Round-trip test failed: restored message differs from original".to_string(),
                ))
            }
        } else {
            Err(MqtteaClientError::RawMessageError(
                "Round-trip test failed: could not downcast restored message".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::any::Any;

    use carbide_test_support::value_scenarios;
    use rumqttc::QoS;

    use super::*;

    #[derive(Clone, Copy)]
    enum EntryBuild {
        DefaultJson,
        QosOverride,
        RetainOverride,
    }

    #[derive(Debug, PartialEq)]
    struct EntrySummary {
        type_name: String,
        patterns: Vec<String>,
        publish_options: Option<(Option<QoS>, Option<bool>)>,
        qos: Option<QoS>,
        retain: Option<bool>,
        format: SerializationFormat,
        pattern_count: usize,
        has_alpha: bool,
        has_missing: bool,
        uses_qos_override: bool,
        effective_qos: QoS,
        is_json: bool,
        debug_mentions_type: bool,
        debug_hides_handlers: bool,
    }

    fn string_entry(
        publish_options: Option<PublishOptions>,
        format: SerializationFormat,
    ) -> MqttRegistryEntry {
        MqttRegistryEntry {
            message_type_info: MessageTypeInfo {
                type_name: "String".to_string(),
                patterns: vec!["alpha".to_string(), "beta".to_string()],
                publish_options,
                format,
            },
            serialize_handler: Box::new(|message| {
                message
                    .downcast_ref::<String>()
                    .map(|message| message.as_bytes().to_vec())
                    .ok_or_else(|| MqtteaClientError::raw_message_error("expected String"))
            }),
            deserialize_handler: Box::new(|bytes| {
                String::from_utf8(bytes.to_vec())
                    .map(|message| Box::new(message) as Box<dyn Any + Send>)
                    .map_err(|err| MqtteaClientError::invalid_utf8(err.to_string()))
            }),
        }
    }

    fn entry_from(build: EntryBuild) -> MqttRegistryEntry {
        match build {
            EntryBuild::DefaultJson => string_entry(None, SerializationFormat::Json),
            EntryBuild::QosOverride => string_entry(
                Some(PublishOptions::default().with_qos(QoS::ExactlyOnce)),
                SerializationFormat::Raw,
            ),
            EntryBuild::RetainOverride => string_entry(
                Some(PublishOptions::default().with_retain(true)),
                SerializationFormat::Yaml,
            ),
        }
    }

    fn summarize(entry: MqttRegistryEntry) -> EntrySummary {
        EntrySummary {
            type_name: entry.type_name().to_string(),
            patterns: entry.patterns().to_vec(),
            publish_options: entry
                .publish_options()
                .map(|options| (options.qos, options.retain)),
            qos: entry.qos(),
            retain: entry.retain(),
            format: entry.format(),
            pattern_count: entry.pattern_count(),
            has_alpha: entry.has_pattern("alpha"),
            has_missing: entry.has_pattern("missing"),
            uses_qos_override: entry.uses_qos_override(),
            effective_qos: entry.effective_qos(QoS::AtMostOnce),
            is_json: entry.is_format(SerializationFormat::Json),
            debug_mentions_type: format!("{entry:?}").contains("String"),
            debug_hides_handlers: format!("{entry:?}").contains("<function>"),
        }
    }

    #[test]
    fn test_registry_entry_accessors() {
        value_scenarios!(
            run = |build| summarize(entry_from(build));
            "default JSON entry" {
                EntryBuild::DefaultJson => EntrySummary {
                    type_name: "String".to_string(),
                    patterns: vec!["alpha".to_string(), "beta".to_string()],
                    publish_options: None,
                    qos: None,
                    retain: None,
                    format: SerializationFormat::Json,
                    pattern_count: 2,
                    has_alpha: true,
                    has_missing: false,
                    uses_qos_override: false,
                    effective_qos: QoS::AtMostOnce,
                    is_json: true,
                    debug_mentions_type: true,
                    debug_hides_handlers: true,
                },
            }

            "QoS override" {
                EntryBuild::QosOverride => EntrySummary {
                    type_name: "String".to_string(),
                    patterns: vec!["alpha".to_string(), "beta".to_string()],
                    publish_options: Some((Some(QoS::ExactlyOnce), None)),
                    qos: Some(QoS::ExactlyOnce),
                    retain: None,
                    format: SerializationFormat::Raw,
                    pattern_count: 2,
                    has_alpha: true,
                    has_missing: false,
                    uses_qos_override: true,
                    effective_qos: QoS::ExactlyOnce,
                    is_json: false,
                    debug_mentions_type: true,
                    debug_hides_handlers: true,
                },
            }

            "retain override" {
                EntryBuild::RetainOverride => EntrySummary {
                    type_name: "String".to_string(),
                    patterns: vec!["alpha".to_string(), "beta".to_string()],
                    publish_options: Some((None, Some(true))),
                    qos: None,
                    retain: Some(true),
                    format: SerializationFormat::Yaml,
                    pattern_count: 2,
                    has_alpha: true,
                    has_missing: false,
                    uses_qos_override: false,
                    effective_qos: QoS::AtMostOnce,
                    is_json: false,
                    debug_mentions_type: true,
                    debug_hides_handlers: true,
                },
            }
        );
    }

    #[test]
    fn test_registry_entry_handlers() {
        let entry = entry_from(EntryBuild::DefaultJson);
        let message = "hello".to_string();

        assert_eq!(entry.serialize(&message).expect("serialize"), b"hello");

        let restored = entry
            .deserialize(b"hello")
            .expect("deserialize")
            .downcast::<String>()
            .expect("restored String");
        assert_eq!(*restored, "hello");

        assert!(entry.validate_serialization_round_trip(&message).is_ok());
        assert!(entry.serialize(&42usize).is_err());
        assert!(entry.deserialize(&[0xff]).is_err());
    }
}
