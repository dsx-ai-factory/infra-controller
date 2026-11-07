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

// src/registry/registry.rs
// MqttRegistry implementation for client-scoped message type
// registration and routing.
//
// Contains the main registry logic for pattern matching, type
// lookup, and format-specific registration methods. Each client
// gets its own registry instance for complete isolation.

use std::collections::HashMap;

use regex::Regex;
use tracing::debug;

use super::entry::MqttRegistryEntry;
use super::types::{
    DeserializeHandler, MessageTypeInfo, PublishOptions, SerializationFormat, SerializeHandler,
};
use crate::errors::MqtteaClientError;

// MqttRegistry encapsulates all message type registration and routing
// logic. Each client gets its own registry instance.
pub struct MqttRegistry {
    // topic_patterns stores regex patterns in registration
    // order (first-match-wins)
    topic_patterns: Vec<(Regex, String)>,
    // entries maps type names to complete registration information.
    entries: HashMap<String, MqttRegistryEntry>,
}

impl MqttRegistry {
    // new creates an empty MqttRegistry. Called during MqtteaClient
    // initialization to create client-scoped registry.
    pub fn new() -> Self {
        Self {
            topic_patterns: Vec::new(),
            entries: HashMap::new(),
        }
    }

    // register_message_type registers a message type with patterns
    // and serialization handlers. This is the core registration method
    // that sets up everything needed for a message type.
    pub fn register_message_type<T: 'static>(
        &mut self,
        patterns: Vec<String>,
        publish_options: Option<PublishOptions>,
        format: SerializationFormat,
        serialize_handler: SerializeHandler,
        deserialize_handler: DeserializeHandler,
    ) -> Result<(), MqtteaClientError> {
        // TODO(chet): Consider moving to TypeId here.
        let type_name = std::any::type_name::<T>();

        // Create message type info
        let info = MessageTypeInfo {
            type_name: type_name.to_string(),
            patterns: patterns.clone(),
            publish_options,
            format,
        };

        // Create registry entry with all components.
        let entry = MqttRegistryEntry {
            message_type_info: info,
            serialize_handler,
            deserialize_handler,
        };

        // Store complete entry.
        self.entries.insert(type_name.to_string(), entry);

        // Register topic patterns.
        for pattern in patterns {
            let regex_pattern = if self.is_regex_pattern(&pattern) {
                pattern
            } else {
                format!("/{pattern}$")
            };

            let regex = Regex::new(&regex_pattern)
                .map_err(|e| MqtteaClientError::PatternCompilationError(e.to_string()))?;

            self.topic_patterns.push((regex, type_name.to_string()));
            debug!(
                pattern = %regex_pattern,
                message_type = %type_name,
                "Registered MQTT topic pattern",
            );
        }

        Ok(())
    }

    // is_regex_pattern detects if a string contains regex metacharacters.
    // Used to determine whether to convert simple strings to suffix patterns
    fn is_regex_pattern(&self, s: &str) -> bool {
        s.contains('^')
            || s.contains('$')
            || s.contains('*')
            || s.contains('.')
            || s.contains('+')
            || s.contains('?')
            || s.contains('[')
            || s.contains(']')
            || s.contains('{')
            || s.contains('}')
            || s.contains('(')
            || s.contains(')')
            || s.contains('|')
            || s.contains('\\')
    }

    // find_matching_type_for_topic determines which message type
    // handles a given topic. Implements first-match-wins behavior
    // by checking patterns in registration order.
    pub fn find_matching_type_for_topic(&self, topic: &str) -> Option<&MessageTypeInfo> {
        for (regex, type_name) in &self.topic_patterns {
            if regex.is_match(topic) {
                debug!(
                    %topic,
                    pattern = %regex.as_str(),
                    message_type = %type_name,
                    "Topic matched registered pattern",
                );
                return self
                    .entries
                    .get(type_name)
                    .map(|entry| &entry.message_type_info);
            }
        }
        debug!(%topic, "Topic did not match any registered patterns");
        None
    }

    // get_entry_by_name retrieves complete registry entry by type name string.
    // Provides access to both metadata and handlers for a registered type.
    pub fn get_entry_by_name(&self, type_name: &str) -> Option<&MqttRegistryEntry> {
        self.entries.get(type_name)
    }

    // get_type_info_by_name retrieves message type metadata by type name string.
    pub fn get_type_info_by_name(&self, type_name: &str) -> Option<&MessageTypeInfo> {
        self.entries
            .get(type_name)
            .map(|entry| &entry.message_type_info)
    }

    // get_type_info retrieves message type metadata by Rust type.
    pub fn get_type_info<T: 'static>(&self) -> Option<&MessageTypeInfo> {
        let type_id = std::any::type_name::<T>();
        for entry in self.entries.values() {
            // TODO(chetn): Simplified mapping. Might need better type_id
            // to type_name mapping.
            if type_id.contains(&entry.message_type_info.type_name) {
                return Some(&entry.message_type_info);
            }
        }
        None
    }

    // get_entry_for_type retrieves complete registry entry by Rust type.
    pub fn get_entry_for_type<T: 'static>(&self) -> Option<&MqttRegistryEntry> {
        let type_id = std::any::type_name::<T>();
        // TODO(chetn): Simplified mapping. Might need better type_id
        // to type_name mapping.
        self.entries
            .values()
            .find(|&entry| type_id.contains(&entry.message_type_info.type_name))
    }

    // serialize_message serializes a message using client-scoped handlers.
    pub fn serialize_message<T: 'static>(&self, message: &T) -> Result<Vec<u8>, MqtteaClientError> {
        if let Some(entry) = self.get_entry_for_type::<T>() {
            entry.serialize(message as &dyn std::any::Any)
        } else {
            let type_id = std::any::type_name::<T>();
            Err(MqtteaClientError::UnregisteredType(type_id.to_string()))
        }
    }

    // deserialize_message deserializes bytes to a message using client-scoped handlers.
    pub fn deserialize_message<T: 'static>(&self, bytes: &[u8]) -> Result<T, MqtteaClientError> {
        if let Some(entry) = self.get_entry_for_type::<T>() {
            // Use the type-specific deserialize handler to convert bytes
            // back to a message.
            let boxed_any = entry.deserialize(bytes)?;

            // The handler returns Box<dyn Any> for type erasure, BUT, we know it
            // contains a value of type T (since we looked up the handler by
            // type T). Downcast to recover the actual type.
            if let Ok(message) = boxed_any.downcast::<T>() {
                Ok(*message)
            } else {
                // TODO(chet): This should never happen. Probably use a
                // different error type to make it really obvious something
                // is wrong with registry management logic.
                let type_id = std::any::type_name::<T>();
                Err(MqtteaClientError::UnregisteredType(format!(
                    "Failed to downcast {type_id} after deserialization"
                )))
            }
        } else {
            let type_id = std::any::type_name::<T>();
            Err(MqtteaClientError::UnregisteredType(type_id.to_string()))
        }
    }

    // list_registered_types returns all registered message types with their
    // metadata. Useful for debugging, configuration display, introspection, etc.
    pub fn list_registered_types(&self) -> Vec<(String, &MessageTypeInfo)> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.clone(), &entry.message_type_info))
            .collect()
    }

    // list_registered_entries returns all registered entries.
    pub fn list_registered_entries(&self) -> Vec<(String, &MqttRegistryEntry)> {
        self.entries
            .iter()
            .map(|(name, entry)| (name.clone(), entry))
            .collect()
    }

    // get_topic_patterns_for_type returns all patterns registered for a
    // specific type. Useful for debugging which topics will route to a
    // particular message handler.
    pub fn get_topic_patterns_for_type(&self, type_name: &str) -> Vec<String> {
        self.topic_patterns
            .iter()
            .filter(|(_, name)| name == type_name)
            .map(|(regex, _)| regex.as_str().to_string())
            .collect()
    }

    // pattern_count returns the total number of registered patterns.
    pub fn pattern_count(&self) -> usize {
        self.topic_patterns.len()
    }

    // entry_count returns the total number of registered message types.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    // has_entry_for_type checks if a specific Rust type is registered.
    pub fn has_entry_for_type<T: 'static>(&self) -> bool {
        self.get_entry_for_type::<T>().is_some()
    }

    // clear clears all registrations from the registry.
    pub fn clear(&mut self) {
        self.topic_patterns.clear();
        self.entries.clear();
        debug!("Cleared all registry data");
    }

    // has_patterns checks if any patterns are registered.
    pub fn has_patterns(&self) -> bool {
        !self.topic_patterns.is_empty()
    }

    // register_protobuf_message registers a protobuf message type with
    // client-scoped handlers Creates type-specific serialization handlers
    // and registers topic patterns
    pub fn register_protobuf_message<T: prost::Message + Default + 'static>(
        &mut self,
        patterns: Vec<String>,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let serialize_handler: SerializeHandler = Box::new(|any_msg| {
            if let Some(msg) = any_msg.downcast_ref::<T>() {
                Ok(msg.encode_to_vec())
            } else {
                Err(MqtteaClientError::UnregisteredType(
                    "Failed to downcast for protobuf serialization".to_string(),
                ))
            }
        });

        let deserialize_handler: DeserializeHandler = Box::new(|bytes| {
            let msg = T::decode(bytes).map_err(MqtteaClientError::ProtobufDeserializationError)?;
            Ok(Box::new(msg) as Box<dyn std::any::Any + Send>)
        });

        self.register_message_type::<T>(
            patterns,
            publish_options,
            SerializationFormat::Protobuf,
            serialize_handler,
            deserialize_handler,
        )
    }

    // register_json_message registers a JSON message type with
    // client-scoped handlers. Creates serde-based serialization
    // handlers and registers topic patterns.
    pub fn register_json_message<
        T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    >(
        &mut self,
        patterns: Vec<String>,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let serialize_handler: SerializeHandler = Box::new(|any_msg| {
            if let Some(msg) = any_msg.downcast_ref::<T>() {
                serde_json::to_vec(msg).map_err(MqtteaClientError::JsonSerializationError)
            } else {
                Err(MqtteaClientError::UnregisteredType(
                    "Failed to downcast for JSON serialization".to_string(),
                ))
            }
        });

        let deserialize_handler: DeserializeHandler = Box::new(|bytes| {
            let msg: T = serde_json::from_slice(bytes)
                .map_err(MqtteaClientError::JsonDeserializationError)?;
            Ok(Box::new(msg) as Box<dyn std::any::Any + Send>)
        });

        self.register_message_type::<T>(
            patterns,
            publish_options,
            SerializationFormat::Json,
            serialize_handler,
            deserialize_handler,
        )
    }

    // register_yaml_message registers a YAML message type with
    // client-scoped handlers. Creates serde YAML serialization
    // handlers and registers topic patterns.
    pub fn register_yaml_message<
        T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    >(
        &mut self,
        patterns: Vec<String>,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let serialize_handler: SerializeHandler = Box::new(|any_msg| {
            if let Some(msg) = any_msg.downcast_ref::<T>() {
                serde_yaml::to_string(msg)
                    .map(|s| s.into_bytes())
                    .map_err(MqtteaClientError::YamlSerializationError)
            } else {
                Err(MqtteaClientError::UnregisteredType(
                    "Failed to downcast for YAML serialization".to_string(),
                ))
            }
        });

        let deserialize_handler: DeserializeHandler = Box::new(|bytes| {
            let s = std::str::from_utf8(bytes)
                .map_err(|e| MqtteaClientError::InvalidUtf8(e.to_string()))?;
            let msg: T =
                serde_yaml::from_str(s).map_err(MqtteaClientError::YamlDeserializationError)?;
            Ok(Box::new(msg) as Box<dyn std::any::Any + Send>)
        });

        self.register_message_type::<T>(
            patterns,
            publish_options,
            SerializationFormat::Yaml,
            serialize_handler,
            deserialize_handler,
        )
    }

    // register_raw_message registers a raw message type with
    // client-scoped handlers. Creates RawMessageType-based serialization
    // handlers and registers topic patterns.
    pub fn register_raw_message<T: crate::traits::RawMessageType + 'static>(
        &mut self,
        patterns: Vec<String>,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let serialize_handler: SerializeHandler = Box::new(|any_msg| {
            if let Some(msg) = any_msg.downcast_ref::<T>() {
                Ok(msg.to_bytes())
            } else {
                Err(MqtteaClientError::UnregisteredType(
                    "Failed to downcast for raw serialization".to_string(),
                ))
            }
        });

        let deserialize_handler: DeserializeHandler = Box::new(|bytes| {
            let msg = T::from_bytes(bytes.to_vec());
            Ok(Box::new(msg) as Box<dyn std::any::Any + Send>)
        });

        self.register_message_type::<T>(
            patterns,
            publish_options,
            SerializationFormat::Raw,
            serialize_handler,
            deserialize_handler,
        )
    }
}

impl Default for MqttRegistry {
    fn default() -> Self {
        Self::new()
    }
}
