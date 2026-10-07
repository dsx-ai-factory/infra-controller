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
// mockdpa used to test Carbide <-> DPA interactions
// mockdpa subscribes to the command channel and picks up
// commands posted by Carbide. It then sends acks on the
// ack channel. For each macaddr, we store the last
// config command received. When we get a heartbeat for
// that macaddr, we reply with the stored config if we have
// any. It's possible that we got restarted and don't have
// any stored config when we receive the heartbeat. In that
// case, we just echo the heartbeat message. Carbide will
// detect that we don't have the current config, and will
// send the config to us again. This situation mimics
// the DPA being powercycled and losing its config and having
// to be reprogrammed by Carbide.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use carbide_dpa::rpc::SetVni;
use chrono::Local;
use clap::Parser;
use mqttea::client::{ClientOptions, MqtteaClient};
use mqttea::registry::traits::ProtobufRegistration;
use rumqttc::QoS;
use tokio::time::{Duration, sleep};

#[derive(Parser)]
#[command(name = "mockdpa")]
#[command(about = "A MQTT client library with type-mapped topics.", long_about = None)]
struct Cli {
    // MQTT broker hostname
    #[arg(long, default_value = "10.217.117.44")]
    host: String,

    // MQTT broker port
    #[arg(long, default_value = "1884")]
    port: u16,

    // Default QoS level
    #[arg(long, default_value = "0")]
    qos: u8,
}

#[derive(Clone)]
struct InterfaceState {
    client: Arc<MqtteaClient>,
    last_set_msg: Arc<Mutex<HashMap<String, SetVni>>>,
}

// Callback routine invoked when a message is received from the broker
async fn handle_host_message(mystate: &mut InterfaceState, message: SetVni, topic: String) {
    println!(
        "[{}] INFO: handle_dpa_message topic: {topic} msg: {message:#?}",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );

    let tokens: Vec<&str> = topic.split("/").collect();
    if tokens.len() < 3 {
        println!(
            "[{}] ERROR: unusable topic: {topic}",
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        return;
    }

    let macaddr = tokens[2];

    let topic = format!("dpa/ack/{macaddr}/SetVni");

    let md = match message.clone().metadata {
        Some(md) => md,
        None => {
            println!(
                "[{}] ERROR: message metadata not present msg: {message:#?}",
                Local::now().format("%Y-%m-%d %H:%M:%S")
            );
            return;
        }
    };

    let mut reply = message.clone();

    if md.revision == "NIL" {
        // We just received a heartbeat message.
        // We should reply with our current configuration
        // It's possible that we don't have any current config
        // if we just restarted. In that case, we will just
        // echo the message we received.
        let mguard = mystate.last_set_msg.lock().unwrap();
        if mguard.contains_key(macaddr)
            && let Some(rep) = mguard.get(macaddr)
        {
            reply = rep.clone();
        }
    } else {
        // This is not a heartbeat. Carbide is actually configuring us.
        // Remember the config so that we can send it back in response to
        // heartbeat messages.
        let mut mguard = mystate.last_set_msg.lock().unwrap();
        mguard.insert(macaddr.to_string(), message.clone());
    }

    match mystate.client.send_message(&topic, &reply).await {
        Ok(()) => {
            println!(
                "[{}] INFO: sent message: {reply:#?} to topic: {topic}",
                Local::now().format("%Y-%m-%d %H:%M:%S")
            );
        }
        Err(e) => {
            println!(
                "[{}] ERROR: send_dpa_command error: {e:#?} sending message: {reply:#?} to topic: {topic}",
                Local::now().format("%Y-%m-%d %H:%M:%S")
            );
        }
    }
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
    let client_id = "dpa-client".to_string();

    // Convert QoS level to a rumqttc QoS.
    let qos = match cli.qos {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => return Err("Invalid QoS level. Use 0, 1, or 2.".into()),
    };

    // Lets gooooo.
    println!("[{}] Starting up", Local::now().format("%Y-%m-%d %H:%M:%S"));
    println!(
        "[{}]   Broker: {}:{}",
        cli.host,
        cli.port,
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    println!(
        "[{}]   Client ID: {client_id}",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );

    // Create the client. Provide some client-specific PublishOptions
    // just to showcase that PublishOptions are a thing.
    let client = MqtteaClient::new(
        &cli.host,
        cli.port,
        &client_id,
        Some(ClientOptions::default().with_qos(qos)),
    )
    .await?;

    let mystate = InterfaceState {
        client: client.clone(),
        last_set_msg: Arc::new(Mutex::new(HashMap::new())),
    };

    let default_qos = QoS::AtMostOnce;

    println!(
        "[{}] INFO: Registering message types with registry.",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    client.register_protobuf_message::<SetVni>("SetVni").await?;

    let ns = "dpa/command/#".to_string();

    client.subscribe(&ns, default_qos).await?;

    println!(
        "[{}] INFO: Subscribed to namespace: {ns}",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );

    let ms = mystate.clone();

    client
        .on_message(move |_client, message: SetVni, topic| {
            let mut value = ms.clone();
            // Call the ack handler
            async move {
                if let Err(e) = tokio::spawn(async move {
                    handle_host_message(&mut value, message, topic).await;
                })
                .await
                {
                    println!(
                        "[{}] ERROR: handle_dpa_message failed: {e}",
                        Local::now().format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
        })
        .await;

    // This doesn't need to be called last but it is here
    // just because.
    client.connect().await?;

    println!(
        "[{}] INFO: Subscribed and listening for messages.",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    println!(
        "[{}] INFO: Press Ctrl+C to stop",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    );

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
                "[{}] INFO: Stats: {} received, {} sent, {} pending",
                queue_stats.total_processed,
                publish_stats.total_published,
                queue_stats.pending_messages,
                Local::now().format("%Y-%m-%d %H:%M:%S")
            );
            last_processed = queue_stats.total_processed;
            last_sent = publish_stats.total_published;
        }

        sleep(Duration::from_secs(5)).await;
    }
}
