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

use std::time::Duration;

use carbide_mqtt_common::hook::{MqttPublisher, QueuedMessage, process_events};
use carbide_mqtt_common::metrics::{MqttHookMetrics, PublishComponent};
use carbide_uuid::machine::HostMachineId;
use model::machine::ManagedHostState;
use opentelemetry::metrics::Meter;
use state_controller::state_change_emitter::{StateChangeEvent, StateChangeHook};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::mqtt_state_change_hook::message::ManagedHostStateMessage;

/// MQTT hook that publishes `ManagedHostState` changes to the MQTT broker.
///
/// Implements the AsyncAPI specification in `carbide.yaml`, publishing to
/// `{topic_prefix}/{machineId}/state` where `topic_prefix` is supplied by the
/// caller (defaults to `NICO/v1/machine` when sourced from config).
///
/// This hook maintains an internal queue and processes events in a background task.
/// If the queue is full, events are dropped and a warning is logged.
pub(crate) struct MqttStateChangeHook {
    sender: mpsc::Sender<QueuedMessage>,
    publish_timeout: Duration,
    topic_prefix: String,
    metrics: MqttHookMetrics,
}

impl MqttStateChangeHook {
    /// `MqttStateChangeHook::new` creates the bounded event queue and starts its
    /// publisher task. Publish outcomes feed
    /// `carbide_dsx_event_bus_publish_count_total`; queue occupancy remains in
    /// `carbide_dsx_event_bus_queue_depth`.
    pub(crate) fn new<P: MqttPublisher>(
        client: P,
        join_set: &mut JoinSet<()>,
        publish_timeout: Duration,
        topic_prefix: String,
        queue_capacity: usize,
        meter: &Meter,
        cancel_token: CancellationToken,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(queue_capacity);
        let metrics =
            MqttHookMetrics::new(meter, sender.downgrade(), PublishComponent::ManagedHost);
        join_set.spawn(process_events(
            receiver,
            client,
            metrics.clone(),
            cancel_token,
        ));
        Self {
            sender,
            publish_timeout,
            topic_prefix,
            metrics,
        }
    }
}

impl StateChangeHook<HostMachineId, ManagedHostState> for MqttStateChangeHook {
    fn on_state_changed(&self, event: &StateChangeEvent<'_, HostMachineId, ManagedHostState>) {
        // Serialize immediately to avoid cloning state
        let message = ManagedHostStateMessage {
            machine_id: event.object_id,
            managed_host_state: event.new_state,
            timestamp: event.timestamp,
        };
        let topic = message.topic(&self.topic_prefix);

        match message.to_json_bytes() {
            Ok(payload) => {
                let deadline = Instant::now() + self.publish_timeout;
                let queued = QueuedMessage {
                    topic,
                    machine_id: event.object_id.to_string(),
                    payload,
                    deadline,
                };
                if let Err(error) = self.sender.try_send(queued) {
                    let error_message = error.to_string();
                    let queued = error.into_inner();
                    self.metrics
                        .record_overflow(queued.topic, queued.machine_id, error_message);
                }
            }
            Err(error) => {
                self.metrics.record_state_change_serialization_error(
                    topic,
                    event.object_id.to_string(),
                    error.to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mqttea::MqtteaClientError;
    use opentelemetry::global;
    use tokio::sync::Barrier;

    use super::*;

    fn test_meter() -> opentelemetry::metrics::Meter {
        global::meter("test")
    }

    fn test_machine_id() -> HostMachineId {
        use carbide_uuid::machine::{MachineIdSource, MachineType};
        carbide_uuid::machine::MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [0; 32],
            MachineType::Host,
        )
        .try_into()
        .unwrap()
    }

    fn make_event<'a>(
        id: &'a HostMachineId,
        state: &'a ManagedHostState,
    ) -> StateChangeEvent<'a, HostMachineId, ManagedHostState> {
        StateChangeEvent {
            object_id: id,
            previous_state: None,
            new_state: state,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Publisher that signals via channel when publish completes.
    struct SignalingPublisher {
        sender: tokio::sync::mpsc::UnboundedSender<(String, Vec<u8>)>,
    }

    impl SignalingPublisher {
        fn new() -> (
            Self,
            tokio::sync::mpsc::UnboundedReceiver<(String, Vec<u8>)>,
        ) {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            (Self { sender }, receiver)
        }
    }

    #[async_trait::async_trait]
    impl MqttPublisher for SignalingPublisher {
        async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError> {
            let _ = self.sender.send((topic.to_string(), payload));
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_events_are_published() {
        let (publisher, mut receiver) = SignalingPublisher::new();
        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();
        let hook = MqttStateChangeHook::new(
            publisher,
            &mut join_set,
            Duration::from_secs(1),
            "NICO/v1/machine".to_string(),
            16,
            &test_meter(),
            cancel_token.clone(),
        );

        let id = test_machine_id();
        let state = ManagedHostState::Ready;
        hook.on_state_changed(&make_event(&id, &state));

        // Wait for publish to complete
        let (topic, payload) = receiver.recv().await.expect("should receive message");

        assert!(topic.starts_with("NICO/v1/machine/"));
        assert!(topic.ends_with("/state"));

        let parsed: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        let state_obj = parsed.get("managed_host_state").unwrap();
        assert_eq!(state_obj.get("state").unwrap(), "ready");

        cancel_token.cancel();
        join_set.join_all().await;
    }

    #[tokio::test]
    async fn test_publish_timeout() {
        let started = Arc::new(Barrier::new(2));
        let call_count = Arc::new(AtomicUsize::new(0));
        let complete_count = Arc::new(AtomicUsize::new(0));

        struct TimeoutPublisher {
            started: Arc<Barrier>,
            call_count: Arc<AtomicUsize>,
            complete_count: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl MqttPublisher for TimeoutPublisher {
            async fn publish(&self, _: &str, _: Vec<u8>) -> Result<(), MqtteaClientError> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                self.started.wait().await;
                // Block forever - timeout should cancel before we get here
                std::future::pending::<()>().await;
                self.complete_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        let publisher = TimeoutPublisher {
            started: started.clone(),
            call_count: call_count.clone(),
            complete_count: complete_count.clone(),
        };

        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();
        let hook = MqttStateChangeHook::new(
            publisher,
            &mut join_set,
            Duration::from_millis(1),
            "NICO/v1/machine".to_string(),
            16,
            &test_meter(),
            cancel_token.clone(),
        );

        let id = test_machine_id();
        let state = ManagedHostState::Ready;
        hook.on_state_changed(&make_event(&id, &state));

        // Wait for publish to start
        started.wait().await;

        // Publish was called but will never complete (blocked on pending)
        // Timeout should have cancelled it
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(complete_count.load(Ordering::SeqCst), 0);

        cancel_token.cancel();
        join_set.join_all().await;
    }

    #[tokio::test]
    async fn test_queue_overflow_drops_events() {
        const QUEUE_SIZE: usize = 4;

        let (gate_tx, gate_rx) = tokio::sync::watch::channel(false);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        struct GatedPublisher {
            gate: tokio::sync::watch::Receiver<bool>,
            tx: tokio::sync::mpsc::UnboundedSender<()>,
        }

        #[async_trait::async_trait]
        impl MqttPublisher for GatedPublisher {
            async fn publish(&self, _: &str, _: Vec<u8>) -> Result<(), MqtteaClientError> {
                // Wait for gate to open
                let _ = self.gate.clone().wait_for(|&open| open).await;
                let _ = self.tx.send(());
                Ok(())
            }
        }

        let publisher = GatedPublisher { gate: gate_rx, tx };

        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();
        let hook = MqttStateChangeHook::new(
            publisher,
            &mut join_set,
            Duration::from_secs(10),
            "NICO/v1/machine".to_string(),
            QUEUE_SIZE,
            &test_meter(),
            cancel_token.clone(),
        );

        let id = test_machine_id();
        let state = ManagedHostState::Ready;

        // Push more events than queue can hold
        for _ in 0..(QUEUE_SIZE + 10) {
            hook.on_state_changed(&make_event(&id, &state));
        }

        // Release the gate - processor will drain queue
        gate_tx.send(true).unwrap();

        // Drop hook to close sender
        drop(hook);

        // Count how many were actually published
        let mut count = 0;
        while rx.recv().await.is_some() {
            count += 1;
        }

        // Queue can hold QUEUE_SIZE events, so exactly that many should be processed
        assert_eq!(count, QUEUE_SIZE);

        cancel_token.cancel();
        join_set.join_all().await;
    }

    #[tokio::test]
    async fn test_custom_topic_prefix_is_used() {
        let (publisher, mut receiver) = SignalingPublisher::new();
        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();
        let hook = MqttStateChangeHook::new(
            publisher,
            &mut join_set,
            Duration::from_secs(1),
            "custom/prefix".to_string(),
            16,
            &test_meter(),
            cancel_token.clone(),
        );

        let id = test_machine_id();
        let state = ManagedHostState::Ready;
        hook.on_state_changed(&make_event(&id, &state));

        let (topic, _payload) = receiver.recv().await.expect("should receive message");

        assert!(
            topic.starts_with("custom/prefix/"),
            "topic should use the configured prefix, got {topic}"
        );
        assert!(topic.ends_with("/state"));

        cancel_token.cancel();
        join_set.join_all().await;
    }
}
