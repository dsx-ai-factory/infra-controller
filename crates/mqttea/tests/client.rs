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
// tests/client.rs
// Unit tests for MqtteaClient core functionality including connection management,
// message handling, subscription management, and statistics tracking.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mqttea::client::ClientOptions;
use mqttea::registry::traits::{JsonRegistration, ProtobufRegistration, RawRegistration};
use mqttea::traits::{MessageHandler, RawMessageType};
use mqttea::{MqtteaClient, MqtteaClientError, QoS};
use rumqttc::AsyncClient;
use tokio::sync::Mutex;

#[derive(Clone, PartialEq, prost::Message, serde::Serialize, serde::Deserialize)]
struct HelloWorld {
    #[prost(string, tag = "1")]
    message: String,
    #[prost(int64, tag = "2")]
    timestamp: i64,
    #[prost(string, tag = "3")]
    device_id: String,
}

// Test message type for registration testing
#[derive(Clone, Debug, PartialEq)]
struct DogMessage {
    breed: String,
    age: u32,
    payload: Vec<u8>,
}

impl RawMessageType for DogMessage {
    fn to_bytes(&self) -> Vec<u8> {
        format!(
            "{}:{}:{}",
            self.breed,
            self.age,
            String::from_utf8_lossy(&self.payload)
        )
        .into_bytes()
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        let content = String::from_utf8_lossy(&bytes);
        let parts: Vec<&str> = content.splitn(3, ':').collect();
        if parts.len() >= 3 {
            Self {
                breed: parts[0].to_string(),
                age: parts[1].parse().unwrap_or(0),
                payload: parts[2].as_bytes().to_vec(),
            }
        } else {
            Self {
                breed: "unknown".to_string(),
                age: 0,
                payload: bytes,
            }
        }
    }
}

// Test helper to create a client for testing without requiring a real MQTT broker
async fn create_test_client() -> Result<Arc<MqtteaClient>, MqtteaClientError> {
    MqtteaClient::new(
        "localhost",
        1883,
        "test-cat-client",
        Some(ClientOptions::default().with_qos(QoS::AtMostOnce)),
    )
    .await
}

// Handler for tracking received messages in tests
struct TestHandler<T> {
    messages: Arc<Mutex<Vec<(T, String)>>>,
    count: Arc<AtomicUsize>,
}

impl<T> TestHandler<T> {
    fn new() -> Self {
        Self {
            messages: Arc::new(Mutex::new(Vec::new())),
            count: Arc::new(AtomicUsize::new(0)),
        }
    }
}

// Verify rumqttc client is async.
#[test]
fn test_async_client_send_sync() {
    fn assert_send<T: Send>() {}
    fn assert_sync<T: Sync>() {}

    assert_send::<AsyncClient>();
    assert_sync::<AsyncClient>();
}

#[async_trait::async_trait]
impl<T: Clone + Send + Sync + 'static> MessageHandler<T> for TestHandler<T> {
    async fn handle(&self, _client: Arc<MqtteaClient>, message: T, topic: String) {
        self.count.fetch_add(1, Ordering::Relaxed);
        let mut messages = self.messages.lock().await;
        messages.push((message, topic));
    }
}

// Tests for MqtteaClient creation and basic configuration
#[tokio::test]
async fn test_client_creation() {
    let client = create_test_client().await;
    assert!(client.is_ok(), "Client creation should succeed");
}

#[tokio::test]
async fn test_client_creation_with_custom_qos() {
    let client = MqtteaClient::new(
        "localhost",
        1883,
        "test-hamster-client",
        Some(ClientOptions::default().with_qos(QoS::AtMostOnce)),
    )
    .await;
    assert!(
        client.is_ok(),
        "Client creation with custom QoS should succeed"
    );
}

// Tests for protobuf message registration
#[tokio::test]
async fn test_protobuf_registration_single_pattern() {
    let client = create_test_client().await.unwrap();

    let result = client
        .register_protobuf_message::<HelloWorld>("cat-messages")
        .await;
    assert!(result.is_ok(), "Protobuf registration should succeed");
}

#[tokio::test]
async fn test_protobuf_registration_multiple_patterns() {
    let client = create_test_client().await.unwrap();

    let patterns = vec!["dog-barks", "dog-woofs", "puppy-sounds"];
    let result = client
        .register_protobuf_message::<HelloWorld>(patterns)
        .await;
    assert!(
        result.is_ok(),
        "Multiple pattern registration should succeed"
    );
}

// Tests for raw message registration
#[tokio::test]
async fn test_raw_registration() {
    let client = create_test_client().await.unwrap();

    let result = client.register_raw_message::<DogMessage>("raw-dogs").await;
    assert!(result.is_ok(), "Raw message registration should succeed");
}

// Tests for JSON message registration
#[tokio::test]
async fn test_json_registration() {
    let client = create_test_client().await.unwrap();

    let result = client
        .register_json_message::<HelloWorld>("json-hellos")
        .await;
    assert!(result.is_ok(), "JSON message registration should succeed");
}

// Tests for message handler registration
#[tokio::test]
async fn test_register_handler() {
    let client = create_test_client().await.unwrap();

    // Register the message type first
    client
        .register_protobuf_message::<HelloWorld>("test-handler")
        .await
        .unwrap();

    // Register a handler
    let handler = TestHandler::<HelloWorld>::new();
    client.register_handler(handler).await;
    // Note: register_handler doesn't return a Result, it's infallible
}

// Tests for on_message closure registration
#[tokio::test]
async fn test_on_message_closure() {
    let client = create_test_client().await.unwrap();

    // Register the message type first
    client
        .register_protobuf_message::<HelloWorld>("test-closure")
        .await
        .unwrap();

    // Test the closure signature with Arc<MqtteaClient>
    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = count.clone();

    client
        .on_message(
            move |_client: Arc<MqtteaClient>, _msg: HelloWorld, _topic: String| {
                let count = count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            },
        )
        .await;

    // Note: on_message doesn't return a Result, it's infallible
}

// Test for bidirectional communication capability
#[tokio::test]
async fn test_bidirectional_communication() {
    let client = create_test_client().await.unwrap();

    // Register the message type first
    client
        .register_protobuf_message::<HelloWorld>("test-bidirectional")
        .await
        .unwrap();

    let response_count = Arc::new(AtomicUsize::new(0));

    // Test handler that can send responses
    client
        .on_message(
            move |client: Arc<MqtteaClient>, msg: HelloWorld, topic: String| {
                let count = response_count.clone();
                async move {
                    count.fetch_add(1, Ordering::Relaxed);

                    // Create a response message
                    let response = HelloWorld {
                        message: format!("Response to: {}", msg.message),
                        timestamp: msg.timestamp + 1,
                        device_id: "test-responder".to_string(),
                    };

                    // Send the response (this tests that the client reference works)
                    let response_topic = format!("{topic}/response");
                    if let Err(e) = client.send_message(&response_topic, &response).await {
                        eprintln!("Failed to send response: {e}");
                    }
                }
            },
        )
        .await;

    // Note: on_message doesn't return a Result, it's infallible
}

// Tests for subscription management
#[tokio::test]
async fn test_subscribe_to_topic() {
    let client = create_test_client().await.unwrap();

    let result = client.subscribe("test/subscription", QoS::AtMostOnce).await;
    // Note: This might succeed or fail depending on implementation
    // The important thing is that the API works and doesn't panic
    match result {
        Ok(_) => println!("Subscribe succeeded (mock implementation)"),
        Err(_) => println!("Subscribe failed as expected without real broker"),
    }
    // Just test that the method exists and can be called
}

// Note: unsubscribe method doesn't exist in current API
// #[tokio::test]
// async fn test_unsubscribe_from_topic() {
//     let client = create_test_client().await.unwrap();
//     let result = client.unsubscribe("test/unsubscription").await;
//     assert!(result.is_err(), "Unsubscribe should fail without broker connection");
// }

// Tests for statistics functionality
#[tokio::test]
async fn test_queue_stats() {
    let client = create_test_client().await.unwrap();

    let stats = client.queue_stats();
    assert_eq!(
        stats.pending_messages, 0,
        "Initial pending messages should be 0"
    );
    assert_eq!(stats.pending_bytes, 0, "Initial pending bytes should be 0");
    assert_eq!(
        stats.total_processed, 0,
        "Initial processed messages should be 0"
    );
    assert_eq!(stats.total_failed, 0, "Initial failed messages should be 0");
    assert_eq!(
        stats.total_bytes_processed, 0,
        "Initial processed bytes should be 0"
    );
}

#[tokio::test]
async fn test_publish_stats() {
    let client = create_test_client().await.unwrap();

    let stats = client.publish_stats();
    assert_eq!(
        stats.total_published, 0,
        "Initial published messages should be 0"
    );
    assert_eq!(
        stats.total_failed, 0,
        "Initial failed publishes should be 0"
    );
    assert_eq!(
        stats.total_bytes_published, 0,
        "Initial published bytes should be 0"
    );
}
