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

//! Metrics for the DSX Exchange Consumer service.

use std::hash::Hash;

use carbide_instrument::{Event, LabelValue};
use moka::future::Cache;
use opentelemetry::StringValue;
use opentelemetry::metrics::Meter;
use tokio::sync::mpsc;

use crate::messages::LeakPointType;

pub static METRICS_PREFIX: &str = "carbide_dsx_exchange_consumer";

/// Register a gauge for the metadata cache size.
///
/// Cloning the cache is cheap: moka caches are internally Arc'd.
pub fn register_metadata_cache_gauge<K, V>(meter: &Meter, cache: &Cache<K, V>)
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    let cache = cache.clone();
    meter
        .u64_observable_gauge(format!("{METRICS_PREFIX}_metadata_cache_size"))
        .with_description("Number of entries in the metadata cache")
        .with_callback(move |observer| {
            observer.observe(cache.entry_count(), &[]);
        })
        .build();
}

/// Register a gauge for the value state cache size.
///
/// Cloning the cache is cheap: moka caches are internally Arc'd.
pub fn register_value_state_cache_gauge<K, V>(meter: &Meter, cache: &Cache<K, V>)
where
    K: Eq + Hash + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    let cache = cache.clone();
    meter
        .u64_observable_gauge(format!("{METRICS_PREFIX}_value_state_cache_size"))
        .with_description("Number of entries in the value state cache")
        .with_callback(move |observer| {
            observer.observe(cache.entry_count(), &[]);
        })
        .build();
}

/// Register a gauge for the number of messages queued in the processing
/// channel, so backpressure is visible before the drop counter starts moving.
///
/// Takes the sender by value and keeps only a weak handle for the meter's
/// (process) lifetime. A strong clone would pin the channel open and defeat
/// the consumer's shutdown, which completes only when the last real sender
/// drops and the receiver observes the close. The callback upgrades briefly to
/// read the depth and reports nothing once the senders are gone.
pub fn register_queue_pending_gauge<T>(meter: &Meter, tx: mpsc::Sender<T>)
where
    T: Send + 'static,
{
    let weak_tx = tx.downgrade();
    meter
        .u64_observable_gauge(format!("{METRICS_PREFIX}_queue_pending_messages"))
        .with_description(
            "Number of messages queued in the DSX exchange consumer's processing channel",
        )
        .with_callback(move |observer| {
            // Upgrade only for the read; a strong handle held between scrapes
            // would pin the channel open. Occupied slots = configured capacity
            // minus the free slots the sender currently reports.
            if let Some(tx) = weak_tx.upgrade() {
                let pending = tx.max_capacity().saturating_sub(tx.capacity());
                observer.observe(pending as u64, &[]);
            }
        })
        .build();
    // The moved-in strong sender drops here, leaving only `weak_tx`.
}

// The message counters are `carbide-instrument` events. Each declares a name
// ending in a single `_total`: the framework strips one `_total` before
// registering the instrument and the OpenTelemetry Prometheus exporter
// appends its own `_total`, so `/metrics` exposes the name exactly as declared
// here.

/// An MQTT message reached a subscription handler, before any queueing.
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_message_received",
    metric_name = "carbide_dsx_exchange_consumer_messages_received_total",
    component = "nico-dsx-exchange-consumer",
    log = off,
    metric = counter,
    describe = "Number of MQTT messages received"
)]
pub struct MessageReceived;

/// A message was correlated with its metadata and its rack health update
/// applied (or its alert cleared).
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_message_processed",
    metric_name = "carbide_dsx_exchange_consumer_messages_processed_total",
    component = "nico-dsx-exchange-consumer",
    log = off,
    metric = counter,
    describe = "Number of messages successfully processed"
)]
pub struct MessageProcessed;

/// The bounded internal queue was full, so an incoming message was dropped.
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_message_dropped",
    metric_name = "carbide_dsx_exchange_consumer_messages_dropped_total",
    component = "nico-dsx-exchange-consumer",
    log = warn,
    metric = counter,
    message = dynamic,
    describe = "Number of messages dropped due to queue overflow"
)]
pub struct MessageDropped {
    /// `message_type` identifies which bounded MQTT subscription handler
    /// rejected the message. It stays log-only so
    /// `carbide_dsx_exchange_consumer_messages_dropped_total` remains a
    /// zero-label metric.
    #[context]
    pub message_type: DroppedMessageType,
}

/// `DroppedMessageType` identifies which MQTT payload could not enter the
/// bounded consumer queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DroppedMessageType {
    Metadata,
    Value,
}

impl std::fmt::Display for DroppedMessageType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            DroppedMessageType::Metadata => "metadata",
            DroppedMessageType::Value => "value",
        })
    }
}

impl carbide_instrument::DynamicMessage for MessageDropped {
    fn message(&self) -> &'static str {
        match self.message_type {
            DroppedMessageType::Metadata => "Message queue full, dropping metadata message",
            DroppedMessageType::Value => "Message queue full, dropping value message",
        }
    }
}

/// A value matched the state already cached for its point, so no API update
/// was sent.
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_message_deduplicated",
    metric_name = "carbide_dsx_exchange_consumer_dedup_skipped_total",
    component = "nico-dsx-exchange-consumer",
    log = trace,
    metric = counter,
    message = "Deduplicating unchanged value",
    describe = "Number of messages skipped due to deduplication"
)]
pub struct MessageDeduplicated {
    #[context]
    pub point_path: String,
    #[context]
    pub point_type: LeakPointType,
    #[context]
    pub value: String,
}

/// `LeakAlertDetected` records an active BMS leak or sensor fault immediately
/// before the consumer inserts its rack health override.
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_leak_alert_detected",
    metric_name = "carbide_dsx_exchange_consumer_alerts_detected_total",
    component = "nico-dsx-exchange-consumer",
    log = info,
    metric = counter,
    message = "Leak alert detected, inserting health override",
    describe = "Number of leak alerts detected"
)]
pub struct LeakAlertDetected {
    #[label]
    pub point_type: LeakPointType,
    #[context]
    pub point_path: String,
    #[context]
    pub rack_id: String,
    #[context]
    pub rack_name: String,
    #[context]
    pub value: String,
}

/// `leak_point_type_name` preserves the BMS's canonical CamelCase spelling for
/// the public `point_type` metric label. `LeakPointType` is closed, but deriving
/// `LabelValue` would render these variants as snake_case.
fn leak_point_type_name(point_type: &LeakPointType) -> &'static str {
    match point_type {
        LeakPointType::LeakDetectRack => "LeakDetectRack",
        LeakPointType::LeakSensorFaultRack => "LeakSensorFaultRack",
        LeakPointType::LeakDetectRackTray => "LeakDetectRackTray",
    }
}

impl std::fmt::Display for LeakPointType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(leak_point_type_name(self))
    }
}

impl LabelValue for LeakPointType {
    fn label_value(&self) -> StringValue {
        StringValue::from(leak_point_type_name(self))
    }
}

/// How far behind the BMS event time we are when a value message reaches
/// processing: end-to-end consumer lag (MQTT transit plus time spent queued).
///
/// Metric-only histogram. The `_seconds` suffix declares the unit, and the
/// framework records the `Duration` observation in seconds.
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_message_age_sampled",
    metric_name = "carbide_dsx_exchange_consumer_message_age_seconds",
    component = "nico-dsx-exchange-consumer",
    log = off,
    metric = histogram,
    describe = "Age of consumed BMS value messages at processing time (consumer lag), in seconds"
)]
pub struct MessageAge {
    #[observation]
    pub age: std::time::Duration,
}

/// A rack health report could not be persisted to the Carbide API -- either a
/// coolant-leak override insert or its clearing removal failed. The value
/// re-arrives on the next message, so processing still retries, but a rising
/// rate is safety-relevant: leak state is not reaching the API. Logs the
/// failure and moves the counter from one `emit`.
///
/// Unlike the grandfathered counters above, this is a new metric, so it uses
/// the standard checked name: the exporter appends the single `_total` that
/// `/metrics` shows.
#[derive(Event)]
#[event(
    event_name = "dsx_exchange_health_report_persist_failed",
    metric_name = "carbide_dsx_exchange_consumer_health_report_persist_failures_total",
    component = "nico-dsx-exchange-consumer",
    log = warn,
    metric = counter,
    message = "Failed to persist rack health report",
    describe = "Number of rack health report persist failures against the Carbide API"
)]
pub struct HealthReportPersistFailed {
    /// The rack whose health report could not be persisted.
    #[context]
    pub rack_id: String,
    /// The API error that prevented the persist.
    #[context]
    pub error: String,
}
