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

//! MQTT hook implementation for publishing state changes.

use std::sync::Arc;

use mqttea::{MqtteaClient, MqtteaClientError};
use tokio::sync::mpsc;
use tokio::time::error::Elapsed;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

use crate::metrics::MqttHookMetrics;

/// Internal queue item containing pre-serialized MQTT message with deadline.
pub struct QueuedMessage {
    pub topic: String,
    /// `machine_id` travels with the queued payload so its eventual `Event` log
    /// can identify the managed host. It never becomes a metric label.
    pub machine_id: String,
    pub payload: Vec<u8>,
    /// Deadline by which this message must be published.
    pub deadline: Instant,
}

/// Trait for MQTT publishing, enabling test mocks.
#[async_trait::async_trait]
pub trait MqttPublisher: Send + Sync + 'static {
    /// Publish a message to the given topic.
    async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError>;
}

#[async_trait::async_trait]
impl MqttPublisher for MqtteaClient {
    async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError> {
        MqtteaClient::publish(self, topic, payload).await
    }
}

#[async_trait::async_trait]
impl<T: MqttPublisher> MqttPublisher for Arc<T> {
    async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError> {
        T::publish(self, topic, payload).await
    }
}

/// `publish_with_deadline` gives the queue-draining [`process_events`] task and
/// the periodic republisher one publish/deadline path. `machine_id` follows the
/// result into the managed-host `Event` log for correlation, but never becomes
/// a metric label.
pub async fn publish_with_deadline<P: MqttPublisher>(
    publisher: &P,
    topic: &str,
    machine_id: &str,
    payload: Vec<u8>,
    deadline: Instant,
    metrics: &MqttHookMetrics,
) {
    match timeout_at(deadline, publisher.publish(topic, payload)).await {
        Ok(Ok(())) => {
            metrics.record_managed_success(topic.to_string(), machine_id.to_string());
        }
        Ok(Err(e)) => {
            metrics.record_managed_publish_error(
                topic.to_string(),
                machine_id.to_string(),
                e.to_string(),
            );
        }
        Err(Elapsed { .. }) => {
            metrics.record_managed_timeout(topic.to_string(), machine_id.to_string());
        }
    }
}

/// Background task that processes queued messages and publishes to MQTT.
pub async fn process_events<P: MqttPublisher>(
    mut receiver: mpsc::Receiver<QueuedMessage>,
    client: P,
    metrics: MqttHookMetrics,
    cancel_token: CancellationToken,
) {
    while let Some(Some(msg)) = cancel_token.run_until_cancelled(receiver.recv()).await {
        publish_with_deadline(
            &client,
            &msg.topic,
            &msg.machine_id,
            msg.payload,
            msg.deadline,
            &metrics,
        )
        .await;
    }
    tracing::debug!("MQTT state change hook background task stopped");
}
