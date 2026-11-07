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

// src/lib.rs
// Main exports for the mqttea MQTT client library.

pub mod auth;
pub mod client;
pub mod client_id;
pub mod errors;
pub mod message_types;
pub mod registry;
pub mod stats;
pub mod traits;

// Export some things for convenience.
// Re-export auth types for convenience.
pub use auth::{
    ClientCredentialsProvider, ClientId, ClientSecret, CredentialsProvider, OAuth2Config,
    OAuth2TokenProvider, StaticCredentials, TokenCredentialsProvider, TokenProvider,
};
pub use client::{MqtteaClient, TopicPatterns};
pub use client_id::unique_client_id;
pub use errors::MqtteaClientError;
pub use message_types::RawMessage;
pub use registry::{MessageTypeInfo, MqttRegistry, SerializationFormat};
pub use rumqttc::QoS;
pub use stats::{PublishStats, QueueStats};
pub use traits::{MessageHandler, MqttRecipient, RawMessageType};
