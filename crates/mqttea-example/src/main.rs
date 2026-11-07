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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

// src/main.rs
// PoC/demo of the mqttea client library.

use clap::Parser;
use mqttea::client::{ClientOptions, MqtteaClient};
use mqttea::message_types::{RawMessage, StringMessage};
use mqttea::registry::traits::{JsonRegistration, ProtobufRegistration, RawRegistration};
use mqttea::registry::types::PublishOptions;
use rumqttc::QoS;
use serde::{Deserialize, Serialize};
use tokio::time::{Duration, sleep};
use tracing::{info, warn};

pub mod sample_protos;
use crate::sample_protos::HelloWorld;

#[derive(Parser)]
#[command(name = "mqttea-example")]
#[command(about = "A MQTT client library with type-mapped topics.", long_about = None)]
struct Cli {
    // Listen mode - subscribe and wait for messages
    #[arg(short, long)]
    listen: bool,

    // MQTT broker hostname
    #[arg(long, default_value = "localhost")]
    host: String,

    // MQTT broker port
    #[arg(long, default_value = "1883")]
    port: u16,

    // Client ID prefix
    #[arg(long, default_value = "mqttea-demo")]
    client_id: String,

    // Topic namespace prefix
    #[arg(long, default_value = "/mqttea-example-ns")]
    namespace: String,

    // Device ID for this instance
    #[arg(long, default_value = "demo-device")]
    device_id: String,

    // Message content to send
    #[arg(long, default_value = "Hello from mqttea!")]
    message: String,

    // Default QoS level
    #[arg(long, default_value = "0")]
    qos: u8,
}

// CatStatus is a type that implements Serialize
// and Deserialize, and can now be registered as
// a JSON message with mqttea.
#[derive(Serialize, Deserialize)]
struct CatStatus {
    name: String,
    mood: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging with nice formatting.
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(false)
        .with_level(true)
        .init();

    // Parse the CLI args and set a unique listener
    // and sender ID. If they're the same, the broker
    // is going to be like ????
    let cli = Cli::parse();
    let client_id = if cli.listen {
        format!("{}-listener-{}", cli.client_id, std::process::id())
    } else {
        format!("{}-sender-{}", cli.client_id, std::process::id())
    };

    // Convert QoS level to a rumqttc QoS.
    let qos = match cli.qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => return Err("Invalid QoS level. Use 0, 1, or 2.".into()),
    };

    // Lets gooooo.
    println!("Starting up");
    println!("  Broker: {}:{}", cli.host, cli.port);
    println!("  Client ID: {client_id}");
    println!("  Namespace: {}", cli.namespace);
    println!("  Device ID: {}", cli.device_id);

    // Create the client. Provide some client-specific PublishOptions
    // just to showcase that PublishOptions are a thing.
    let client = MqtteaClient::new(
        &cli.host,
        cli.port,
        &client_id,
        Some(ClientOptions::default().with_qos(qos)),
    )
    .await?;

    println!("Registering message types with registry.");
    client
        .register_protobuf_message::<HelloWorld>("hello-world")
        .await?;
    client
        .register_json_message_with_opts::<CatStatus>(
            "cat-fancy",
            Some(PublishOptions {
                qos: Some(qos),
                retain: None,
            }),
        )
        .await?;

    // TODO(chet): Make it so patterns is Option, so you can pass
    // None, instead of an empty vec.
    client
        .register_raw_message::<StringMessage>(Vec::<String>::new())
        .await?;

    // Register catch-all for unmapped messages (this happens last,
    // because patterns are processed in registration order).
    client.register_raw_message::<RawMessage>(".*").await?;

    println!("Message types registered successfully");
    println!("  HelloWorld: '/hello-world$'");
    println!("  CatStatus:  '/cat-fancy$'");
    println!("  RawMessage: '.*'");

    if cli.listen {
        println!("Listener mode - subscribing to topics.");

        // Remember, # is for wildcards in MQTT topic subscriptions!
        client
            .subscribe(&format!("{}/#", cli.namespace), QoS::ExactlyOnce)
            .await?;

        println!("Subscribed to namespace: {}", cli.namespace);

        // Register HelloWorld message handler.
        client
            .on_message(|client, message: HelloWorld, topic| async move {
                info!(
                    topic = %topic,
                    payload = %message.message,
                    device_id = %message.device_id,
                    timestamp = message.timestamp,
                    "received HelloWorld message"
                );
                let response_message: StringMessage =
                    format!("i got your message that said: {}", message.message).into();
                if let Err(publish_err) = client
                    .send_message(
                        &format!(
                            "/mqttea-example-ns/{}/hello-world-response",
                            client.client_id(),
                        ),
                        &response_message,
                    )
                    .await
                {
                    warn!(
                        error = ?publish_err,
                        "failed to publish response message"
                    );
                }
            })
            .await;

        // Register CatStatus message handler.
        client
            .on_message(|_client, message: CatStatus, topic| async move {
                info!(
                    topic = %topic,
                    cat_name = %message.name,
                    mood = %message.mood,
                    "received CatStatus message"
                );
            })
            .await;

        // Register RawMessage handler for unmapped topics.
        client
            .on_message(|_client, message: RawMessage, topic| async move {
                println!("FYI: Received message on unmapped topic: '{topic}'");
                match String::from_utf8(message.payload.clone()) {
                    Ok(text) => info!(
                        topic = %topic,
                        payload = %text,
                        "received raw text message"
                    ),
                    Err(_) => info!(
                        topic = %topic,
                        payload_bytes = message.payload.len(),
                        "received raw binary message"
                    ),
                }
            })
            .await;

        // This doesn't need to be called last but it is here
        // just because.
        client.connect().await?;
        println!("Subscribed and listening for messages.");
        println!("Press Ctrl+C to stop");

        // Stats monitoring loop
        let mut last_processed = 0;
        let mut last_sent = 0;

        loop {
            let queue_stats = client.queue_stats();
            let publish_stats = client.publish_stats();

            // Only show stats if they changed
            if queue_stats.total_processed != last_processed
                || publish_stats.total_published != last_sent
            {
                println!(
                    "Stats: {} received, {} sent, {} pending",
                    queue_stats.total_processed,
                    publish_stats.total_published,
                    queue_stats.pending_messages
                );
                last_processed = queue_stats.total_processed;
                last_sent = publish_stats.total_published;
            }

            sleep(Duration::from_secs(5)).await;
        }
    } else {
        println!("Sender mode!");
        client.connect().await?;
        println!("Ready to send messages!");

        // Send a CatStatus message (JSON).
        println!("First: Sending CatStatus message...");
        let status = CatStatus {
            name: "fluffy".to_string(),
            mood: "fine".to_string(),
        };

        let topic = format!("{}/{}/cat-fancy", cli.namespace, status.name);
        client.send_message(&topic, &status).await?;
        println!("Sent CatStatus to {topic}");

        // Send a HelloWorld message (protobuf).
        println!("Next, sending HelloWorld message...");
        let hello = HelloWorld {
            device_id: cli.device_id.clone(),
            message: cli.message.clone(),
            timestamp: chrono::Utc::now().timestamp(),
        };

        let topic = format!("{}/{}/hello-world", cli.namespace, cli.device_id);
        client.send_message(&topic, &hello).await?;
        println!("Sent HelloWorld to {topic}");

        // Send a raw message (to be picked up by the listener
        // side as RawMessage via its unmapped handler).
        println!("Sending test unmapped message...");
        client
            .publish(
                &format!("{}/unmapped/test", cli.namespace),
                b"This is unmapped raw data!".to_vec(),
            )
            .await?;
        println!("Sent raw message to {}/unmapped/test", cli.namespace);

        // Wait a moment to see any responses.
        sleep(Duration::from_millis(500)).await;

        // Show final stats.
        let queue_stats = client.queue_stats();
        let publish_stats = client.publish_stats();

        println!("Statistics:");
        println!("  Messages received: {}", queue_stats.total_processed);
        println!("  Messages sent: {}", publish_stats.total_published);
        println!("  Bytes sent: {}", publish_stats.total_bytes_published);
        println!("  Currently pending: {}", queue_stats.pending_messages);
        println!(
            "  Failed: {}",
            queue_stats.total_failed + publish_stats.total_failed
        );

        // Disconnect.
        client.disconnect().await?;
    }

    Ok(())
}
