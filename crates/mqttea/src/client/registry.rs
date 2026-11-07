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

// src/client/registry.rs
// Registry trait implementations for MqtteaClient.
//
// Implements all the format-specific registration traits for MqtteaClient by
// delegating to the client's internal registry. This separates registration
// logic from the main client implementation while maintaining clean trait-based
// organization.

use async_trait::async_trait;

use crate::client::{MqtteaClient, TopicPatterns};
use crate::errors::MqtteaClientError;
use crate::registry::traits::{
    JsonRegistration, ProtobufRegistration, RawRegistration, YamlRegistration,
};
use crate::registry::types::PublishOptions;

#[async_trait]
impl ProtobufRegistration for MqtteaClient {
    // register_protobuf_message registers a protobuf message type.
    async fn register_protobuf_message<T: prost::Message + Default + 'static>(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
    ) -> Result<(), MqtteaClientError> {
        self.register_protobuf_message_with_opts::<T>(patterns, None)
            .await
    }

    // register_protobuf_message_with_opts registers a protobuf message type
    // with explicit PublishOptions for QoS, retain, etc.
    async fn register_protobuf_message_with_opts<T: prost::Message + Default + 'static>(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let patterns_vec = patterns.into().into_vec();
        let mut registry_guard = self.registry.write().await;
        registry_guard.register_protobuf_message::<T>(patterns_vec, publish_options)
    }
}

#[async_trait]
impl JsonRegistration for MqtteaClient {
    // register_json_message registers a JSON message type.
    async fn register_json_message<
        T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
    ) -> Result<(), MqtteaClientError> {
        self.register_json_message_with_opts::<T>(patterns, None)
            .await
    }

    // register_json_message_with_opts registers a JSON message type
    // with explicit PublishOptions for QoS, retain, etc.
    async fn register_json_message_with_opts<
        T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let patterns_vec = patterns.into().into_vec();
        let mut registry_guard = self.registry.write().await;
        registry_guard.register_json_message::<T>(patterns_vec, publish_options)
    }
}

#[async_trait]
impl YamlRegistration for MqtteaClient {
    // register_yaml_message registers a YAML message type.
    async fn register_yaml_message<
        T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
    ) -> Result<(), MqtteaClientError> {
        self.register_yaml_message_with_opts::<T>(patterns, None)
            .await
    }

    // register_yaml_message_with_opts registers a YAML message type
    // with explicit PublishOptions for QoS, retain, etc.
    async fn register_yaml_message_with_opts<
        T: serde::Serialize + serde::de::DeserializeOwned + Send + 'static,
    >(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let patterns_vec = patterns.into().into_vec();
        let mut registry_guard = self.registry.write().await;
        registry_guard.register_yaml_message::<T>(patterns_vec, publish_options)
    }
}

#[async_trait]
impl RawRegistration for MqtteaClient {
    // register_raw_message registers a raw message type.
    async fn register_raw_message<T: crate::traits::RawMessageType + 'static>(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
    ) -> Result<(), MqtteaClientError> {
        self.register_raw_message_with_opts::<T>(patterns, None)
            .await
    }

    // register_raw_message_with_opts registers a raw message
    // with explicit PublishOptions for QoS, retain, etc.
    async fn register_raw_message_with_opts<T: crate::traits::RawMessageType + 'static>(
        &self,
        patterns: impl Into<TopicPatterns> + Send,
        publish_options: Option<PublishOptions>,
    ) -> Result<(), MqtteaClientError> {
        let patterns_vec = patterns.into().into_vec();
        let mut registry_guard = self.registry.write().await;
        registry_guard.register_raw_message::<T>(patterns_vec, publish_options)
    }
}
