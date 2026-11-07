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

// src/mqttea/errors.rs
// Error types for error handling throughout the MQTT client library.

use thiserror::Error;

// MqtteaClientError covers all possible error conditions in the
// MQTT client. Each variant provides specific context about what
// went wrong and why, or it should, at least.
#[derive(Error, Debug)]
pub enum MqtteaClientError {
    // ConnectionError occurs when MQTT broker communication fails
    // (network issues, auth failures).
    #[error("MQTT connection error: {0}")]
    ConnectionError(#[from] rumqttc::ClientError),
    // SerializationError occurs when converting messages to bytes
    // fails (malformed data).
    #[error("message serialization error: {0}")]
    SerializationError(#[from] prost::EncodeError),
    // ProtobufDeserializationError occurs when protobuf message
    // parsing fails (corrupted data, schema mismatch).
    #[error("protobuf deserialization error: {0}")]
    ProtobufDeserializationError(#[from] prost::DecodeError),
    // JsonSerializationError occurs when JSON message serialization fails.
    #[error("JSON serialization error: {0}")]
    JsonSerializationError(#[from] serde_json::Error),
    // JsonDeserializationError occurs when JSON message parsing
    // fails (invalid syntax, type mismatch).
    #[error("JSON deserialization error: {0}")]
    JsonDeserializationError(#[source] serde_json::Error),
    // YamlSerializationError occurs when YAML message serialization fails.
    #[error("YAML serialization error: {0}")]
    YamlSerializationError(#[from] serde_yaml::Error),
    // YamlDeserializationError occurs when YAML message parsing fails
    // (invalid syntax, type mismatch).
    #[error("YAML deserialization error: {0}")]
    YamlDeserializationError(#[source] serde_yaml::Error),
    // UnknownMessageType occurs when received message topic doesn't
    // match any registered patterns.
    #[error("unknown message type for topic: {0}")]
    UnknownMessageType(String),
    // TopicParsingError occurs when topic format is invalid or
    // contains illegal characters.
    #[error("topic parsing error: {0}")]
    TopicParsingError(String),
    // UnsupportedSerializationType occurs when message uses unknown
    // serialization format.
    #[error("unsupported serialization type: {0:?}")]
    UnsupportedSerializationType(String),
    // RawMessageError occurs when custom raw message processing fails
    // (custom validation, format errors).
    #[error("raw message error: {0}")]
    RawMessageError(String),
    // UnregisteredType occurs when trying to serialize/deserialize a
    // type that wasn't registered.
    #[error("type not registered in message registry: {0}")]
    UnregisteredType(String),
    //InvalidUtf8 occurs when converting bytes to string fails.
    #[error("invalid UTF-8 encoding: {0}")]
    InvalidUtf8(String),
    // PatternCompilationError occurs when regex pattern compilation fails.
    #[error("pattern compilation error: {0}")]
    PatternCompilationError(String),
    // AlreadyStartedError occurs when connect() has already
    // been called on the client.
    #[error("already started error: connect() has already been called on the client")]
    AlreadyStartedError,
    // CredentialsError occurs when fetching credentials from a provider fails.
    #[error("credentials provider error: {0}")]
    CredentialsError(String),
}

// Convenience implementations for creating common error types.
impl MqtteaClientError {
    // Create an UnknownMessageType error for a specific topic.
    pub fn unknown_message_type(topic: impl Into<String>) -> Self {
        Self::UnknownMessageType(topic.into())
    }

    // Create a TopicParsingError with a descriptive message.
    pub fn topic_parsing_error(message: impl Into<String>) -> Self {
        Self::TopicParsingError(message.into())
    }

    // Create a RawMessageError with a descriptive message.
    pub fn raw_message_error(message: impl Into<String>) -> Self {
        Self::RawMessageError(message.into())
    }

    // Create an UnregisteredType error.
    pub fn unregistered_type(type_name: impl Into<String>) -> Self {
        Self::UnregisteredType(type_name.into())
    }

    // Create an InvalidUtf8 error.
    pub fn invalid_utf8(message: impl Into<String>) -> Self {
        Self::InvalidUtf8(message.into())
    }

    // Create a PatternCompilationError.
    pub fn pattern_compilation_error(message: impl Into<String>) -> Self {
        Self::PatternCompilationError(message.into())
    }

    // Create a CredentialsError.
    pub fn credentials_error(message: impl Into<String>) -> Self {
        Self::CredentialsError(message.into())
    }

    // Check if this error is related to network connectivity.
    pub fn is_connection_error(&self) -> bool {
        matches!(self, Self::ConnectionError(_))
    }

    // Check if this error is related to message format/parsing.
    pub fn is_deserialization_error(&self) -> bool {
        matches!(
            self,
            Self::ProtobufDeserializationError(_)
                | Self::JsonDeserializationError(_)
                | Self::YamlDeserializationError(_)
        )
    }

    // Check if this error is related to serialization.
    pub fn is_serialization_error(&self) -> bool {
        matches!(
            self,
            Self::SerializationError(_)
                | Self::JsonSerializationError(_)
                | Self::YamlSerializationError(_)
        )
    }

    // Check if this error is related to topic handling.
    pub fn is_topic_error(&self) -> bool {
        matches!(
            self,
            Self::UnknownMessageType(_) | Self::TopicParsingError(_)
        )
    }

    // Check if this error is related to registry operations.
    pub fn is_registry_error(&self) -> bool {
        matches!(
            self,
            Self::UnregisteredType(_) | Self::PatternCompilationError(_)
        )
    }
}

// Special handling for the UnregisteredType error without type name.
impl Default for MqtteaClientError {
    fn default() -> Self {
        Self::UnregisteredType("unknown".to_string())
    }
}

// Convenience for creating UnregisteredType errors from type info.
pub fn unregistered_type_error<T: 'static>() -> MqtteaClientError {
    MqtteaClientError::UnregisteredType(std::any::type_name::<T>().to_string())
}
