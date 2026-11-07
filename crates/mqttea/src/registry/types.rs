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

// src/registry/types.rs
// Common types and enums used throughout the registry system.
//
// Contains shared registry bits like serialization formats and
// message metadata that are used throughout this code.

use rumqttc::QoS;

use crate::errors::MqtteaClientError;

// SerializationFormat defines the supported message serialization
// formats. This is a core registry concept used for routing messages
// to appropriate handlers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SerializationFormat {
    Protobuf,
    Json,
    Yaml,
    Raw,
}

// MessageTypeInfo stores metadata about a registered message type.
// Contains all information needed to route and configure message
// handling for a specific type.
#[derive(Clone, Debug)]
pub struct MessageTypeInfo {
    // type_name is the human-readable type name for debugging
    // and logging.
    pub type_name: String,
    // patterns are regex patterns that map topics to this
    // message type.
    pub patterns: Vec<String>,
    // publish_options are the optional overrides for
    // any global publish options.
    pub publish_options: Option<PublishOptions>,
    // format specifies which serialization format this type uses.
    pub format: SerializationFormat,
}

// PublishOptions contains options used for publishing
// messages. There is a global/default PublishOptions
// that are used for all messages, and then users can
// set type and topic-specific overrides as needed.
#[derive(Clone, Copy, Debug, Default)]
pub struct PublishOptions {
    // qos is the MQTT QoS level override for this
    // message type.
    pub qos: Option<QoS>,
    // retain is the MQTT retain override for this message type.
    pub retain: Option<bool>,
}
impl PublishOptions {
    pub fn with_qos(mut self, qos: QoS) -> Self {
        self.qos = Some(qos);
        self
    }

    pub fn with_retain(mut self, retain: bool) -> Self {
        self.retain = Some(retain);
        self
    }
}

// SerializeHandler converts any message to bytes for
// MQTT transmission.
pub type SerializeHandler =
    Box<dyn Fn(&dyn std::any::Any) -> Result<Vec<u8>, MqtteaClientError> + Send + Sync>;

// DeserializeHandler converts bytes back to typed messages
// from MQTT reception.
pub type DeserializeHandler =
    Box<dyn Fn(&[u8]) -> Result<Box<dyn std::any::Any + Send>, MqtteaClientError> + Send + Sync>;

impl MessageTypeInfo {
    // has_pattern checks if this message type is registered
    // for a specific pattern. Useful for validating registration state
    // and debugging pattern conflicts.
    pub fn has_pattern(&self, pattern: &str) -> bool {
        self.patterns.iter().any(|p| p == pattern)
    }

    // pattern_count returns the number of patterns registered for
    // this message type. Useful for monitoring and debugging message
    // type registration complexity.
    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    // uses_qos_override checks if this message type has a custom
    // QoS setting. Useful for determining message delivery guarantees
    // and debugging QoS behavior.
    pub fn uses_qos_override(&self) -> bool {
        self.publish_options
            .map(|opts| opts.qos.is_some())
            .unwrap_or(false)
    }

    // effective_qos returns the QoS that should be used for this
    // message type. Falls back to provided default if no type-specific
    // override is set.
    pub fn effective_qos(&self, default_qos: QoS) -> QoS {
        self.publish_options
            .and_then(|opts| opts.qos)
            .unwrap_or(default_qos)
    }

    // effective_retain returns the retain that should be used for this
    // message type. Falls back to provided default if no type-specific
    // override is set.
    pub fn effective_retain(&self, default_retain: bool) -> bool {
        self.publish_options
            .and_then(|opts| opts.retain)
            .unwrap_or(default_retain)
    }

    // is_format checks if this message type uses a specific
    // serialization format. Useful for filtering and categorizing
    // registered message types.
    pub fn is_format(&self, format: SerializationFormat) -> bool {
        self.format == format
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy)]
    enum PublishOptionsBuild {
        Default,
        Qos,
        Retain,
        QosAndRetain,
    }

    #[derive(Clone, Copy)]
    enum MessageInfoBuild {
        JsonDefault,
        ProtobufQosOverride,
        RawRetainOverride,
    }

    #[derive(Debug, PartialEq)]
    struct MessageInfoSummary {
        has_alpha: bool,
        has_missing: bool,
        pattern_count: usize,
        uses_qos_override: bool,
        effective_qos: QoS,
        effective_retain: bool,
        is_json: bool,
        is_protobuf: bool,
        is_raw: bool,
    }

    fn build_publish_options(build: PublishOptionsBuild) -> PublishOptions {
        match build {
            PublishOptionsBuild::Default => PublishOptions::default(),
            PublishOptionsBuild::Qos => PublishOptions::default().with_qos(QoS::AtLeastOnce),
            PublishOptionsBuild::Retain => PublishOptions::default().with_retain(true),
            PublishOptionsBuild::QosAndRetain => PublishOptions::default()
                .with_qos(QoS::ExactlyOnce)
                .with_retain(false),
        }
    }

    fn build_message_info(build: MessageInfoBuild) -> MessageTypeInfo {
        match build {
            MessageInfoBuild::JsonDefault => MessageTypeInfo {
                type_name: "JsonMessage".to_string(),
                patterns: vec!["alpha".to_string(), "beta".to_string()],
                publish_options: None,
                format: SerializationFormat::Json,
            },
            MessageInfoBuild::ProtobufQosOverride => MessageTypeInfo {
                type_name: "ProtoMessage".to_string(),
                patterns: vec!["alpha".to_string()],
                publish_options: Some(PublishOptions::default().with_qos(QoS::ExactlyOnce)),
                format: SerializationFormat::Protobuf,
            },
            MessageInfoBuild::RawRetainOverride => MessageTypeInfo {
                type_name: "RawMessage".to_string(),
                patterns: vec!["alpha".to_string()],
                publish_options: Some(PublishOptions::default().with_retain(true)),
                format: SerializationFormat::Raw,
            },
        }
    }

    fn summarize_message_info(info: MessageTypeInfo) -> MessageInfoSummary {
        MessageInfoSummary {
            has_alpha: info.has_pattern("alpha"),
            has_missing: info.has_pattern("missing"),
            pattern_count: info.pattern_count(),
            uses_qos_override: info.uses_qos_override(),
            effective_qos: info.effective_qos(QoS::AtMostOnce),
            effective_retain: info.effective_retain(false),
            is_json: info.is_format(SerializationFormat::Json),
            is_protobuf: info.is_format(SerializationFormat::Protobuf),
            is_raw: info.is_format(SerializationFormat::Raw),
        }
    }

    #[test]
    fn test_publish_options_builders() {
        value_scenarios!(
            run = |build| {
                let options = build_publish_options(build);
                (options.qos, options.retain)
            };
            "default" {
                PublishOptionsBuild::Default => (None, None),
            }

            "qos" {
                PublishOptionsBuild::Qos => (Some(QoS::AtLeastOnce), None),
            }

            "retain" {
                PublishOptionsBuild::Retain => (None, Some(true)),
            }

            "qos and retain" {
                PublishOptionsBuild::QosAndRetain => (Some(QoS::ExactlyOnce), Some(false)),
            }
        );
    }

    #[test]
    fn test_message_type_info_helpers() {
        value_scenarios!(
            run = |build| summarize_message_info(build_message_info(build));
            "json default" {
                MessageInfoBuild::JsonDefault => MessageInfoSummary {
                    has_alpha: true,
                    has_missing: false,
                    pattern_count: 2,
                    uses_qos_override: false,
                    effective_qos: QoS::AtMostOnce,
                    effective_retain: false,
                    is_json: true,
                    is_protobuf: false,
                    is_raw: false,
                },
            }

            "protobuf qos override" {
                MessageInfoBuild::ProtobufQosOverride => MessageInfoSummary {
                    has_alpha: true,
                    has_missing: false,
                    pattern_count: 1,
                    uses_qos_override: true,
                    effective_qos: QoS::ExactlyOnce,
                    effective_retain: false,
                    is_json: false,
                    is_protobuf: true,
                    is_raw: false,
                },
            }

            "raw retain override" {
                MessageInfoBuild::RawRetainOverride => MessageInfoSummary {
                    has_alpha: true,
                    has_missing: false,
                    pattern_count: 1,
                    uses_qos_override: false,
                    effective_qos: QoS::AtMostOnce,
                    effective_retain: true,
                    is_json: false,
                    is_protobuf: false,
                    is_raw: true,
                },
            }
        );
    }
}
