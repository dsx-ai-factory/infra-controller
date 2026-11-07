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

// src/mqttea/traits.rs
// Core traits for MQTT message handling
use std::sync::Arc;

use async_trait::async_trait;

// Import the client type for the trait signature
use crate::client::MqtteaClient;

// MqttRecipient enables any type to specify where messages should be sent
// Implement this trait to create strongly-typed addressing.
//
// By default, it's just the type name.
//
// Example: Device routing
// struct Device { id: String, priority: bool }
// impl MqttRecipient for Device {
//     fn to_mqtt_topic(&self) -> String {
//         if self.priority {
//             format!("/priority/devices/{}/alerts", self.id)
//         } else {
//             format!("/devices/{}/messages", self.id)
//         }
//     }
// }
//
// Usage:
// let device = Device { id: "sensor-01".to_string(), priority: true };
// client.send_message(device, &my_message).await?;  // Auto-routes to priority queue
pub trait MqttRecipient {
    // to_mqtt_topic converts recipient into MQTT topic string
    fn to_mqtt_topic(&self) -> String;
}

// String implements MqttRecipient to allow simple topic strings
impl MqttRecipient for String {
    fn to_mqtt_topic(&self) -> String {
        self.clone()
    }
}

// &str implements MqttRecipient to allow string literals as topics
impl MqttRecipient for &str {
    fn to_mqtt_topic(&self) -> String {
        self.to_string()
    }
}

// MessageHandler enables any type to process incoming messages of a specific type
// Implement this trait to create reusable message processors
//
// Example: Custom handler
// struct AlertProcessor { alerts: Arc<Mutex<Vec<AlertMessage>>> }
//
// #[async_trait]
// impl MessageHandler<AlertMessage> for AlertProcessor {
//     async fn handle(&self, client: Arc<MqtteaClient>, message: AlertMessage, topic: String) {
//         let mut alerts = self.alerts.lock().unwrap();
//         alerts.push(message);
//         println!("Alert received on {}", topic);
//
//         // Can now send a response!
//         let response = AlertResponse { acknowledged: true };
//         client.send_message(&format!("{}/ack", topic), &response).await.unwrap();
//     }
// }
//
// Usage:
// let processor = AlertProcessor::new();
// client.register_handler(processor).await;
#[async_trait]
pub trait MessageHandler<T>: Send + Sync {
    // handle processes incoming message of type T from specified topic
    // client parameter enables handlers to send response messages
    async fn handle(&self, client: Arc<MqtteaClient>, message: T, topic: String);
}

// RawMessageType enables custom byte-level serialization for messages.
// Used for working with the Vec<u8> that comes from a publish message,
// if you want full control over the data. Also used for handling catch-all
// unmapped messages.
//
// For example, to just deal w/ basic text:
//
// client.on_message(|client, message: RawMessage, topic| async move {
//     match String::from_utf8(message.payload.clone()) {
//         Ok(text) => println!("Raw text on {}: {}", topic, text),
//         Err(_) => println!("Raw binary on {} ({} bytes)", topic, message.payload.len()),
//     }
// }).await;
//
// Or you could even use it for doing message compression/decompression (
// or encryption/decryption if you wanted):
//
// struct CompressedMessage { data: Vec<u8> }
//
// impl RawMessageType for CompressedMessage {
//     fn to_bytes(&self) -> Vec<u8> {
//         // Apply compression
//         compress(&self.data)
//     }
//
//     fn from_bytes(bytes: Vec<u8>) -> Self {
//         // Decompress
//         Self { data: decompress(bytes) }
//     }
// }
//
// ..with the usage for that being:
// register_raw_message!(CompressedMessage, "compressed-data");

pub trait RawMessageType: Send + Sync + Clone + 'static {
    // to_bytes converts message to byte representation for transmission
    fn to_bytes(&self) -> Vec<u8>;

    // from_bytes recreates message from received bytes
    fn from_bytes(bytes: Vec<u8>) -> Self;
}
