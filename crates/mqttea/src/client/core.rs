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

// src/client/core.rs
// Main MQTT client implementation with client-scoped MqttRegistry and message processing.
//
// Each client has its own registry instance, enabling different clients to have completely
// different message type mappings without any global state conflicts. The client handles
// MQTT connection management, message routing, and statistics tracking.

use std::collections::HashMap;
use std::sync::Arc;

use carbide_instrument::emit;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use tokio::sync::{Mutex, RwLock, Semaphore, mpsc};
use tracing::{debug, error, info, warn};

use crate::auth::CredentialsProvider;
use crate::client::{ClientOptions, ClosureAdapter, ErasedHandler, ReceivedMessage};
use crate::errors::MqtteaClientError;
use crate::registry::MqttRegistry;
use crate::registry::types::PublishOptions;
use crate::stats::{
    ConnectionStateTracker, PublishStats, PublishStatsTracker, QueueStats, QueueStatsTracker,
};
use crate::traits::MessageHandler;

const DEFAULT_KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(300);
const DEFAULT_QOS: QoS = QoS::AtLeastOnce;
const DEFAULT_RETAIN: bool = false;
const DEFAULT_MESSAGE_CHANNEL_CAPACITY: usize = 1000;

const DEFAULT_CLIENT_QUEUE_SIZE: usize = 5000;

// MqtteaClient provides client-scoped MQTT functionality with embedded registry.
// Each client instance has its own registry for complete isolation between clients.
pub struct MqtteaClient {
    // client is the underlying MQTT client for actual network
    // communication.
    client: Arc<AsyncClient>,
    // client_id is the client ID that we pass to the
    // underlying rumqttc::AsyncClient. The AsyncClient
    // itself doesn't provide access to it, so we store
    // it here for logging/identification purposes.
    client_id: String,
    // event_loop is stored to be used in start() method
    event_loop: Arc<Mutex<Option<EventLoop>>>,
    // client_options is used when no explicit PublishOptions are provided
    // for a given message type or topic pattern. If this is None, then
    // the default consts are used as fallback.
    client_options: Option<ClientOptions>,
    // credentials_provider is stored to refresh credentials on reconnection.
    // When the connection drops and needs to reconnect, fresh credentials
    // will be fetched from this provider (e.g., to get a new OAuth2 token).
    credentials_provider: Option<Arc<dyn CredentialsProvider>>,
    // subscriptions tracks the topic patterns and QoS values passed
    // to `subscribe()` so they can be replayed when the broker reports
    // that a reconnect created a fresh session.
    subscriptions: Arc<RwLock<HashMap<String, QoS>>>,
    // handlers stores message-type-specific handlers for processing
    // received messages
    handlers: Arc<RwLock<HashMap<String, ErasedHandler>>>,
    // queue_stats tracks MQTT queue depth and throughput statistics.
    queue_stats: Arc<QueueStatsTracker>,
    // publish_stats tracks message publishing statistics and success rates.
    publish_stats: Arc<PublishStatsTracker>,
    // connection_state tracks whether the client currently holds an
    // acknowledged broker connection.
    connection_state: Arc<ConnectionStateTracker>,
    // registry encapsulates all message type registration and routing logic.
    // Made pub(crate) so trait implementations in registry.rs can access it.
    pub(crate) registry: Arc<RwLock<MqttRegistry>>,
    // concurrency_semaphore is the semaphore used for
    // managing processing concurrency. Messages are pulled
    // from the message queue, and then we fire off a handler
    // for the message. By setting this to > 1, you can achieve
    // parallel processing of messages (the default is to
    // just process messages sequentially).
    concurrency_semaphore: Arc<Semaphore>,
}

impl MqtteaClient {
    // new creates a new MQTT client with empty client-scoped registry.
    // Each client gets its own independent registry for complete isolation.
    // Call connect() after registering handlers to begin processing messages.
    //
    // This is an async function because credentials may need to be fetched
    // from a credentials provider (e.g., OAuth2 token provider).
    pub async fn new(
        broker_host: &str,
        broker_port: u16,
        client_id: &str,
        client_options: Option<ClientOptions>,
    ) -> Result<Arc<Self>, MqtteaClientError> {
        let mqtt_options =
            build_mqtt_options(client_id, broker_host, broker_port, client_options.as_ref())
                .await?;

        let (client, event_loop) = AsyncClient::new(
            mqtt_options,
            client_options
                .as_ref()
                .and_then(|opts| opts.message_channel_capacity)
                .unwrap_or(DEFAULT_MESSAGE_CHANNEL_CAPACITY),
        );
        let handlers: Arc<RwLock<HashMap<String, ErasedHandler>>> =
            Arc::new(RwLock::new(HashMap::new()));

        let queue_stats = Arc::new(QueueStatsTracker::new());
        let publish_stats = Arc::new(PublishStatsTracker::new());
        let connection_state = Arc::new(ConnectionStateTracker::new());

        // Create client-scoped registry instead of using global static.
        let registry = Arc::new(RwLock::new(MqttRegistry::new()));

        let concurrency_limit = client_options
            .as_ref()
            .and_then(|opts| opts.max_concurrency)
            .unwrap_or(1)
            .max(1);

        // Extract credentials_provider for use during reconnection.
        let credentials_provider = client_options
            .as_ref()
            .and_then(|opts| opts.credentials_provider.clone());

        info!(%broker_host, broker_port, "Created MQTT client");

        Ok(Arc::new(Self {
            client: Arc::new(client),
            client_id: client_id.into(),
            event_loop: Arc::new(Mutex::new(Some(event_loop))),
            concurrency_semaphore: Arc::new(Semaphore::new(concurrency_limit)),
            client_options,
            credentials_provider,
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            handlers,
            queue_stats,
            publish_stats,
            connection_state,
            registry,
        }))
    }

    async fn replay_subscriptions(&self) {
        let subs_snapshot: Vec<(String, QoS)> = self
            .subscriptions
            .read()
            .await
            .iter()
            .map(|(topic, qos)| (topic.clone(), *qos))
            .collect();

        if subs_snapshot.is_empty() {
            return;
        }

        info!(
            subscription_count = subs_snapshot.len(),
            "Replaying MQTT subscriptions after broker reported a fresh session"
        );

        for (topic, qos) in subs_snapshot {
            if let Err(e) = self.client.subscribe(topic.as_str(), qos).await {
                error!(
                    %topic,
                    error = %e,
                    "Failed to re-subscribe after fresh session",
                );
            }
        }
    }

    // connect actually connects and starts the event_loop for both
    // *listening* AND *sending*.
    pub async fn connect(self: &Arc<Self>) -> Result<(), MqtteaClientError> {
        self.clone().start_internal().await // Clone [the Arc] internally.
    }

    // start_internal begins message processing with background tasks.
    // This is wrapped by connect, so connect doesn't consume the Arc
    // and allows us to start things while letting the caller just call
    // client.connect().
    //
    // The two async tasks that get created are:
    // - Event loop that waits for events on our AsyncClient (as in,
    //   new MQTT messages), and puts them into our local message queue.
    // - Message processing loop that reads from the local message queue,
    //   decides if the registry has a topic pattern match, and then either
    //   fires it off to the deserialization handler for that type, or logs
    //   that there was no type match.
    async fn start_internal(self: Arc<Self>) -> Result<(), MqtteaClientError> {
        let mut event_loop = self
            .event_loop
            .lock()
            .await
            .take()
            .ok_or_else(|| MqtteaClientError::AlreadyStartedError)?;

        // Create the message queue used between the event loop and
        // message processing tasks. The client queue size should
        // be >= the message channel capacity.
        let (message_queue_tx, mut message_queue_rx) = mpsc::channel::<ReceivedMessage>(
            self.client_options
                .as_ref()
                .and_then(|opts| opts.client_queue_size)
                .unwrap_or(DEFAULT_CLIENT_QUEUE_SIZE),
        );

        let warn_on_unmatched_topic = self
            .client_options
            .as_ref()
            .and_then(|opts| opts.warn_on_unmatched_topic)
            .unwrap_or(true);

        // Event loop task. This waits for events coming in on our AsyncClient
        // implementation (as in, new messages), and then loads it into a
        // new ReceivedMessage and adds it into our local message queue,
        // freeing up the underlying message channel to continue receiving
        // messages.
        let queue_stats_producer = self.queue_stats.clone();
        let connection_state = self.connection_state.clone();
        let registry_clone = self.registry.clone();
        let credentials_provider = self.credentials_provider.clone();
        let client_for_reconnect = self.clone();
        let mut has_connected = false;
        let mut backoff_strategy = SuperBasicBackoff::new();
        tokio::spawn(async move {
            loop {
                match event_loop.poll().await {
                    Ok(event) => {
                        if let Event::Incoming(Packet::ConnAck(connack)) = &event {
                            let should_replay =
                                should_replay_subscriptions(has_connected, connack.session_present);
                            if has_connected {
                                emit(MqttReconnected {});
                            }
                            has_connected = true;
                            connection_state.set_connected(true);
                            backoff_strategy.reset();

                            if should_replay {
                                client_for_reconnect.replay_subscriptions().await;
                            }
                        }

                        if let Event::Incoming(Packet::Publish(publish)) = event {
                            if let Some(msg) =
                                ReceivedMessage::from_publish(&publish, registry_clone.clone())
                                    .await
                            {
                                let payload_size = msg.payload_size;
                                match message_queue_tx.try_send(msg) {
                                    Ok(_) => {
                                        queue_stats_producer.increment_pending(payload_size);
                                        // Any time a message is successfully send, just
                                        // blindly reset the backoff.
                                        backoff_strategy.reset();
                                    }
                                    Err(mpsc::error::TrySendError::Full(_)) => {
                                        warn!(
                                            topic = %publish.topic,
                                            "Message queue full, dropping message from topic",
                                        );
                                        queue_stats_producer.increment_dropped(payload_size);
                                        tokio::time::sleep(backoff_strategy.next_delay()).await;
                                    }
                                    Err(mpsc::error::TrySendError::Closed(_)) => {
                                        // This shouldn't happen -- the receiving end of the channel
                                        // should only close if there's been a panic or the application
                                        // is being shut down.
                                        //
                                        // TODO(chet): Should this be a panic itself?
                                        error!("Message receiver has been dropped");
                                        break;
                                    }
                                }
                            } else {
                                queue_stats_producer.increment_unmatched_topics();
                                if warn_on_unmatched_topic {
                                    warn!(
                                        topic = %publish.topic,
                                        "No registered pattern matched topic",
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "MQTT event loop connection error");
                        connection_state.set_connected(false);
                        queue_stats_producer.increment_event_loop_errors();

                        // Refresh credentials before reconnection attempt if a provider is configured.
                        // This ensures we use fresh tokens (e.g., OAuth2) for the next connection.
                        //
                        // NOTE: This credential refresh on reconnection logic requires integration
                        // testing with a real MQTT broker, as it cannot be easily unit tested
                        // without mocking the EventLoop and simulating connection failures.
                        if let Some(ref provider) = credentials_provider {
                            match provider.get_credentials().await {
                                Ok(credentials) => {
                                    debug!("Refreshed credentials for reconnection");
                                    event_loop.mqtt_options.set_credentials(
                                        credentials.username,
                                        credentials.password,
                                    );
                                }
                                Err(cred_err) => {
                                    error!(
                                        error = %cred_err,
                                        "Failed to refresh credentials for reconnection",
                                    );
                                }
                            }
                        }

                        tokio::time::sleep(backoff_strategy.next_delay()).await;
                    }
                }
            }
        });

        // Message processing task. This looks for new ReceivedMessages that are
        // pushed into our local message queue by the event loop task above,
        // and will [attempt to] deserialize + fire off the callback handler
        // for the message type.
        let handlers_clone = self.handlers.clone();
        let queue_stats_processor = self.queue_stats.clone();
        let handler_client = self.clone();

        tokio::spawn(async move {
            while let Some(msg) = message_queue_rx.recv().await {
                let payload_size = msg.payload_size;
                let handlers_guard = handlers_clone.read().await;

                if let Some(handler) = handlers_guard.get(&msg.type_name) {
                    match handler(handler_client.clone(), msg.payload, msg.topic).await {
                        Ok(_) => {
                            queue_stats_processor
                                .decrement_pending_increment_processed(payload_size);
                        }
                        Err(e) => {
                            error!(
                                message_type = %msg.type_name,
                                error = %e,
                                "Handler error",
                            );
                            queue_stats_processor.decrement_pending_increment_failed(payload_size);
                        }
                    }
                } else {
                    warn!(
                        message_type = %msg.type_name,
                        topic = %msg.topic,
                        "No handler registered for message type on topic",
                    );
                    queue_stats_processor.decrement_pending_increment_failed(payload_size);
                }
            }
        });

        info!("MQTT client started and processing messages");
        Ok(())
    }

    // on_message is used for registering handler callbacks, wrapping
    // on_message_internal with support for parallel processing, and
    // as a side effect, supporting handlers which need to be run safely
    // within their own tokio task (and not a shared Send + Sync handler).
    pub async fn on_message<T, F, Fut>(&self, handler: F)
    where
        T: Send + Sync + 'static,
        F: Fn(Arc<MqtteaClient>, T, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let handler_cb = Arc::new(handler);
        let concurrency_semaphore = self.concurrency_semaphore.clone();

        self.on_message_internal(move |client, message, topic| {
            let handler_internal = handler_cb.clone();
            let semaphore_internal = concurrency_semaphore.clone();
            async move {
                tokio::spawn(async move {
                    // Acquire a SemaphorePermit and hold it -- it
                    // will drop when it drops out of scope; no need
                    // to use the variable anywhere. This will only
                    // be acquired if the number of tasks is less than
                    // max_concurrency. Requests to acquire the semaphore
                    // will be granted in they order they were requested.
                    let _permit = match semaphore_internal.acquire().await {
                        Ok(permit) => permit,
                        Err(e) => {
                            emit(HandlerDispatchDropped {
                                message_type: std::any::type_name::<T>(),
                                error: e.to_string(),
                            });
                            return;
                        }
                    };
                    handler_internal(client, message, topic).await;
                });
            }
        })
        .await;
    }

    // on_message_internal provides basic closure-based handler registration
    // with type inference. Technically, for handlers that support Send + Sync,
    // we could just use this, and not on_message. This *used* to be on_message,
    // until it was decided we wanted to add support for parallel processing of
    // messages, as well as being able to handle non-Sync-safe handlers, such
    // as handlers which were opening database transactions (and needed to be
    // run within a tokio::spawn task anyway).
    async fn on_message_internal<T, F, Fut>(&self, handler: F)
    where
        T: Send + Sync + 'static,
        F: Fn(Arc<MqtteaClient>, T, String) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + Sync + 'static,
    {
        self.register_handler(ClosureAdapter::new(handler)).await;
    }

    // register_handler registers a handler for a specific message type.
    pub async fn register_handler<T, H>(&self, handler: H)
    where
        T: Send + Sync + 'static,
        H: MessageHandler<T> + 'static,
    {
        let handler = Arc::new(handler);
        let type_erased_handler: ErasedHandler = Box::new(move |client, payload, topic| {
            let handler = handler.clone();
            Box::pin(async move {
                // Get the registry to deserialize the message
                let registry_guard = client.registry.read().await;
                let message = registry_guard.deserialize_message::<T>(&payload)?;
                drop(registry_guard);

                handler.handle(client, message, topic).await;
                Ok(())
            })
        });

        let mut handlers_guard = self.handlers.write().await;
        handlers_guard.insert(std::any::type_name::<T>().to_string(), type_erased_handler);
        info!(
            message_type = std::any::type_name::<T>(),
            "Registered handler for message type",
        );
    }

    // subscribe subscribes to a topic with the specified QoS.
    //
    // The subscription is also stored in the client so it can be
    // replayed after a reconnect when the broker reports that the
    // previous session was not resumed.
    pub async fn subscribe(&self, topic: &str, qos: QoS) -> Result<(), MqtteaClientError> {
        self.subscriptions
            .write()
            .await
            .insert(topic.to_string(), qos);

        self.client
            .subscribe(topic, qos)
            .await
            .map_err(MqtteaClientError::ConnectionError)?;

        info!(%topic, ?qos, "Subscribed to topic");
        Ok(())
    }

    pub async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError> {
        self.publish_with_opts(
            topic,
            self.client_options
                .as_ref()
                .and_then(|opts| opts.publish_options),
            payload,
        )
        .await
    }
    // publish_with_opts sends raw bytes to the specified MQTT topic.
    // This is the low-level publishing method for direct topic control.
    pub async fn publish_with_opts(
        &self,
        topic: &str,
        publish_options: Option<PublishOptions>,
        payload: Vec<u8>,
    ) -> Result<(), MqtteaClientError> {
        let payload_size = payload.len();

        // Try to get the QoS and retain from the provided PublishOptions,
        // and if not set, then fall back to the client-wide PublishOptions,
        // and if not set, then fall back to the const defaults for each.
        let qos = publish_options
            .and_then(|opts| opts.qos)
            .or_else(|| {
                self.client_options
                    .as_ref()
                    .and_then(|client_opts| client_opts.publish_options)
                    .and_then(|opts| opts.qos)
            })
            .unwrap_or(DEFAULT_QOS);
        let retain = publish_options
            .and_then(|opts| opts.retain)
            .or_else(|| {
                self.client_options
                    .as_ref()
                    .and_then(|client_opts| client_opts.publish_options)
                    .and_then(|opts| opts.retain)
            })
            .unwrap_or(DEFAULT_RETAIN);

        match self.client.publish(topic, qos, retain, payload).await {
            Ok(_) => {
                self.publish_stats.increment_published(payload_size);
                debug!(%topic, "Published message to topic");
                Ok(())
            }
            Err(e) => {
                self.publish_stats.increment_failed();
                Err(MqtteaClientError::ConnectionError(e))
            }
        }
    }

    // send_message sends a message to a specific topic using
    // client-scoped serialization.
    pub async fn send_message<T>(&self, topic: &str, message: &T) -> Result<(), MqtteaClientError>
    where
        T: 'static,
    {
        let registry_guard = self.registry.read().await;
        let payload = registry_guard.serialize_message(message)?;
        // Get QoS from type info or use default.
        let publish_options = registry_guard
            .get_type_info::<T>()
            .and_then(|info| info.publish_options);
        drop(registry_guard);

        self.publish_with_opts(topic, publish_options, payload)
            .await
    }

    // disconnect gracefully shuts down the MQTT client connection. Should
    // be called before dropping the client to ensure clean shutdown
    pub async fn disconnect(&self) -> Result<(), MqtteaClientError> {
        self.client
            .disconnect()
            .await
            .map_err(MqtteaClientError::ConnectionError)?;

        self.connection_state.set_connected(false);
        info!("MQTT client disconnected");
        Ok(())
    }

    pub fn client_id(&self) -> String {
        self.client_id.clone()
    }

    // Useful for monitoring client performance and message throughput.
    pub fn queue_stats(&self) -> QueueStats {
        self.queue_stats.to_stats()
    }

    // publish_stats returns current publish statistics for monitoring.
    // Tracks success/failure rates and throughput of outgoing messages.
    pub fn publish_stats(&self) -> PublishStats {
        self.publish_stats.to_stats()
    }

    // is_connected reports whether the client currently holds an
    // acknowledged broker connection: true after a ConnAck, false after a
    // connection error or disconnect() (and before connect()).
    pub fn is_connected(&self) -> bool {
        self.connection_state.is_connected()
    }

    // register_metrics registers observable instruments over the client's
    // stats trackers on the given meter: gauges for point-in-time state
    // (queue depth in messages and bytes, connection state) and observable
    // counters for the monotonic totals (processed/failed/dropped and
    // published/failed message and byte counts, event loop errors,
    // unmatched topics). The callbacks read the existing atomics at
    // collection time; nothing on the message path changes.
    //
    // Every series is labeled client=<client> so multiple clients in one
    // process stay distinct. The value must be a compile-time literal
    // naming the client's role (e.g. "dsx_event_bus") -- it is the
    // cardinality bound, so never pass a configured or generated client id.
    // Call once per client, any time after construction.
    pub fn register_metrics(&self, meter: &opentelemetry::metrics::Meter, client: &'static str) {
        self.queue_stats.register_metrics(meter, client);
        self.publish_stats.register_metrics(meter, client);
        self.connection_state.register_metrics(meter, client);
    }

    // is_queue_empty checks if the internal message queue is empty.
    // Useful for determining when all pending messages have been processed.
    pub fn is_queue_empty(&self) -> bool {
        self.queue_stats.is_empty()
    }

    // wait_for_queue_empty blocks until all queued messages are processed.
    // Useful for graceful shutdown or ensuring message delivery.
    pub async fn wait_for_queue_empty(&self) {
        while !self.is_queue_empty() {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    // reset_stats resets all statistical counters to zero.
    // Useful for periodic monitoring or testing scenarios.
    pub fn reset_stats(&self) {
        self.queue_stats.reset_counters();
        self.publish_stats.reset_counters();
    }
}

// build_mqtt_options assembles rumqttc `MqttOptions`, fetching fresh
// credentials from the credentials provider if configured.
async fn build_mqtt_options(
    client_id: &str,
    broker_host: &str,
    broker_port: u16,
    client_options: Option<&ClientOptions>,
) -> Result<MqttOptions, MqtteaClientError> {
    let mut mqtt_options = MqttOptions::new(client_id, broker_host, broker_port);
    mqtt_options.set_keep_alive(
        client_options
            .and_then(|opts| opts.keep_alive)
            .unwrap_or(DEFAULT_KEEP_ALIVE),
    );
    mqtt_options.set_clean_session(false);

    if let Some(provider) = client_options.and_then(|opts| opts.credentials_provider.as_ref()) {
        let credentials = provider.get_credentials().await?;
        mqtt_options.set_credentials(credentials.username, credentials.password);
    }

    Ok(mqtt_options)
}

fn should_replay_subscriptions(has_connected_before: bool, session_present: bool) -> bool {
    has_connected_before && !session_present
}

// The event loop received a ConnAck after having been connected before:
// the broker connection was re-established. The rate is the signal (a
// healthy client reconnects rarely); the surrounding reconnect machinery
// already logs its own details, so this event is metric-only.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "mqtt_reconnected",
    metric_name = "carbide_mqtt_reconnects_total",
    component = "mqttea",
    log = off,
    metric = counter,
    describe = "Number of times an MQTT client re-established its broker connection after the initial connect"
)]
struct MqttReconnected {}

// A received message was dropped at handler dispatch because the
// concurrency semaphore could not be acquired (it only fails once the
// semaphore is closed, so this also means the message's handler will
// never run).
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "mqtt_handler_dispatch_dropped",
    metric_name = "carbide_mqtt_dispatch_dropped_total",
    component = "mqttea",
    log = error,
    metric = counter,
    message = "failed to acquire semaphore permit, dropping message",
    describe = "Number of received MQTT messages dropped because the handler concurrency semaphore could not be acquired"
)]
struct HandlerDispatchDropped {
    #[context]
    message_type: &'static str,
    #[context]
    error: String,
}

// SuperBasicBackoff is a basic backoff I'm implementing
// to back off if there are errors during event loop
// processing. Right now it's just hard-coded to start
// at 100ms and increase up to 30 seconds. I think there
// are probably some fancy crates out there too but
// this was quick to write.
struct SuperBasicBackoff {
    current: std::time::Duration,
    max: std::time::Duration,
}

impl SuperBasicBackoff {
    fn new() -> Self {
        Self {
            current: std::time::Duration::from_millis(100),
            max: std::time::Duration::from_secs(30),
        }
    }

    fn next_delay(&mut self) -> std::time::Duration {
        let delay = self.current;
        self.current = std::cmp::min(self.current * 2, self.max);
        warn!(
            delay_milliseconds = delay.as_millis(),
            "Message event loop backoff updated"
        );
        delay
    }

    fn reset(&mut self) {
        self.current = std::time::Duration::from_millis(100);
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};

    use super::{HandlerDispatchDropped, MqttReconnected, should_replay_subscriptions};

    #[test]
    fn dispatch_dropped_event_logs_error_and_ticks_counter() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(HandlerDispatchDropped {
                message_type: "mqttea::tests::Demo",
                error: "semaphore closed".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::ERROR);
        assert_eq!(
            logs[0].message,
            "failed to acquire semaphore permit, dropping message"
        );
        assert!(logs[0].fields.contains(&(
            "message_type".to_string(),
            "mqttea::tests::Demo".to_string()
        )));
        assert!(
            logs[0]
                .fields
                .contains(&("error".to_string(), "semaphore closed".to_string()))
        );
        assert_eq!(
            metrics.counter_delta("carbide_mqtt_dispatch_dropped_total", &[]),
            1.0
        );
    }

    #[test]
    fn reconnect_event_is_metric_only() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| emit(MqttReconnected {}));

        assert!(logs.is_empty());
        assert_eq!(
            metrics.counter_delta("carbide_mqtt_reconnects_total", &[]),
            1.0
        );
    }

    #[test]
    fn does_not_replay_on_initial_connect() {
        assert!(!should_replay_subscriptions(false, false));
        assert!(!should_replay_subscriptions(false, true));
    }

    #[test]
    fn replays_on_reconnect_when_broker_has_fresh_session() {
        assert!(should_replay_subscriptions(true, false));
    }

    #[test]
    fn does_not_replay_on_reconnect_when_broker_resumed_session() {
        assert!(!should_replay_subscriptions(true, true));
    }
}
