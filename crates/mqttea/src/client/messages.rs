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

// src/client/message.rs
// MQTT message envelope for internal processing and routing.
//
// ReceivedMessage represents a parsed MQTT message that has been
// matched to a registered message type through the client's registry.
// It contains all information needed to route and deserialize an
// incoming message to the appropriate handler.

use std::sync::Arc;

use rumqttc::Publish;
use tokio::sync::RwLock;
use tracing::debug;

use crate::registry::MqttRegistry;

// ReceivedMessage stores a parsed MQTT message ready for processing. It
// contains all information needed to route and deserialize an
// incoming message.
#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    // topic is the full MQTT topic where the message was received.
    pub topic: String,
    // type_name identifies the registered message type that matched this topic.
    pub type_name: String,
    // payload contains the raw message bytes for deserialization.
    pub payload: Vec<u8>,
    // payload_size caches the payload size for efficient statistics tracking.
    pub payload_size: usize,
}

impl ReceivedMessage {
    // from_publish converts MQTT publish packet to internal message
    // format (e.g. parsing HelloWorld from publish packet). Uses registry
    // to determine message type from topic patterns.
    pub async fn from_publish(
        publish: &Publish,
        registry: Arc<RwLock<MqttRegistry>>,
    ) -> Option<Self> {
        let topic = publish.topic.clone();
        let payload = publish.payload.to_vec();
        let payload_size = payload.len();

        debug!(%topic, "Looking for pattern match for topic");
        let registry_guard = registry.read().await;
        registry_guard
            .find_matching_type_for_topic(&topic)
            .map(|type_info| Self {
                topic,
                type_name: type_info.type_name.clone(),
                payload,
                payload_size,
            })
    }
}
