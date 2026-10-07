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

// src/mqttea/stats/queue.rs
// Queue statistics tracking for received message processing
// performance monitoring.
//
// Provides thread-safe atomic counters for tracking message
// processing pipeline health. Used to monitor queue depth,
// throughput, and error rates in real-time.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;

// QueueStats stores a snapshot of received message processing
// statistics.
#[derive(Debug, Clone)]
pub struct QueueStats {
    // pending_messages is count of messages waiting to be
    // processed (current queue depth)
    pub pending_messages: usize,
    // pending_bytes is total size of messages waiting to be
    // processed (current memory usage)
    pub pending_bytes: usize,
    // total_processed is count of messages successfully
    // handled since startup/reset
    pub total_processed: usize,
    // total_failed is count of messages that failed processing
    // since startup/reset.
    pub total_failed: usize,
    // total_bytes_processed is total size of messages
    // successfully handled (throughput metric).
    pub total_bytes_processed: usize,
    // total_dropped is the count of messages that were
    // dropped due to a full message queue.
    pub total_dropped: usize,
    // total_bytes_dropped is the total size of messages
    // dropped due to a full message queue.
    pub total_bytes_dropped: usize,
    // total_event_loop_errors is the number of times
    // a connection error was encounted in the asyncclient
    // event loop.
    pub total_event_loop_errors: usize,
    // total_unmatched_topics is the number of messages
    // received whose topic didn't have a registered handler
    // pattern match.
    pub total_unmatched_topics: usize,
}

// QueueStatsTracker enables thread-safe updates to queue
// statistics using atomic operations. Lock-free. Ensures
// statistics don't impact message processing performance.
#[derive(Debug)]
pub struct QueueStatsTracker {
    // pending_count tracks current number of messages in
    // processing queue.
    pending_count: Arc<AtomicUsize>,
    // pending_bytes tracks current total size of messages
    // in processing queue.
    pending_bytes: Arc<AtomicUsize>,
    // processed_count tracks total number of messages
    // successfully processed.
    processed_count: Arc<AtomicUsize>,
    // processed_bytes tracks total size of messages
    // successfully processed.
    processed_bytes: Arc<AtomicUsize>,
    // dropped_count tracks total number of messages
    // that were dropped due to a full queue.
    dropped_count: Arc<AtomicUsize>,
    // dropped_bytes tracks total size of messages
    // that were dropped.
    dropped_bytes: Arc<AtomicUsize>,
    // failed_count tracks total number of messages
    // that failed processing.
    failed_count: Arc<AtomicUsize>,
    // event_loop_errors tracks the number of times
    // an event loop error occurred in the queue loop.
    event_loop_errors: Arc<AtomicUsize>,
    // unmatched_topics is incremented when a message
    // comes in for a topic that doesn't have a handler match.
    unmatched_topics: Arc<AtomicUsize>,
}

impl Default for QueueStatsTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueStatsTracker {
    // new will create a QueueStatsTracker with all counters initialized
    // to zero. Creates atomic counters wrapped in Arc for safe sharing
    // across async tasks (e.g. used during MqtteaClient initialization).
    pub fn new() -> Self {
        Self {
            pending_count: Arc::new(AtomicUsize::new(0)),
            pending_bytes: Arc::new(AtomicUsize::new(0)),
            processed_count: Arc::new(AtomicUsize::new(0)),
            processed_bytes: Arc::new(AtomicUsize::new(0)),
            dropped_count: Arc::new(AtomicUsize::new(0)),
            dropped_bytes: Arc::new(AtomicUsize::new(0)),
            failed_count: Arc::new(AtomicUsize::new(0)),
            event_loop_errors: Arc::new(AtomicUsize::new(0)),
            unmatched_topics: Arc::new(AtomicUsize::new(0)),
        }
    }

    // increment_pending will record a new message entering the processing queue.
    // Called when MQTT event loop receives a message and queues it for processing
    // (e.g. increment_pending(256) for 256-byte message).
    pub fn increment_pending(&self, bytes: usize) {
        self.pending_count.fetch_add(1, Ordering::Relaxed);
        self.pending_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    // increment_dropped will record a message that was dropped instead of being
    // added to the processing queue. Called when MQTT event loop receives a
    // TrySendError::Full.
    pub fn increment_dropped(&self, bytes: usize) {
        self.dropped_count.fetch_add(1, Ordering::Relaxed);
        self.dropped_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    // increment_event_loop_errors is updated any time the message
    // queue tx loop encounters a connection error reading a message
    // from the AsyncClient.
    pub fn increment_event_loop_errors(&self) {
        self.event_loop_errors.fetch_add(1, Ordering::Relaxed);
    }

    // increment_unmatched_topics is updated any time a message
    // is received on a topic whose topic doesn't match a
    // registered pattern in the registry.
    pub fn increment_unmatched_topics(&self) {
        self.unmatched_topics.fetch_add(1, Ordering::Relaxed);
    }

    // decrement_pending_increment_processed will record successful message processing.
    // Atomically moves counters from pending to processed state (e.g. called when
    // handler successfully processes a 256-byte message). Ensures accurate accounting
    // of message lifecycle.
    pub fn decrement_pending_increment_processed(&self, bytes: usize) {
        self.pending_count.fetch_sub(1, Ordering::Relaxed);
        self.pending_bytes.fetch_sub(bytes, Ordering::Relaxed);
        self.processed_count.fetch_add(1, Ordering::Relaxed);
        self.processed_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    // decrement_pending_increment_failed will record failed message processing.
    // Atomically moves counters from pending to failed state
    // (e.g. called when message deserialization fails or handler throws error).
    // Enables monitoring of error rates and processing health.
    pub fn decrement_pending_increment_failed(&self, bytes: usize) {
        self.pending_count.fetch_sub(1, Ordering::Relaxed);
        self.pending_bytes.fetch_sub(bytes, Ordering::Relaxed);
        self.failed_count.fetch_add(1, Ordering::Relaxed);
    }

    // is_empty will check if the processing queue is currently empty.
    // Used for graceful shutdown and determining when processing is caught up
    // (e.g. wait for queue to drain before shutting down).
    // Returns true if no messages are pending processing.
    pub fn is_empty(&self) -> bool {
        self.pending_count.load(Ordering::Relaxed) == 0
    }

    // reset_counters will clear processed and failed counters back to zero.
    // Useful for periodic reporting, testing, or monitoring system resets
    // (e.g. reset daily stats at midnight for clean reporting periods).
    // Note: We don't reset pending counts as they reflect current state.
    pub fn reset_counters(&self) {
        self.processed_count.store(0, Ordering::Relaxed);
        self.processed_bytes.store(0, Ordering::Relaxed);
        self.dropped_count.store(0, Ordering::Relaxed);
        self.dropped_bytes.store(0, Ordering::Relaxed);
        self.failed_count.store(0, Ordering::Relaxed);
        self.event_loop_errors.store(0, Ordering::Relaxed);
        self.unmatched_topics.store(0, Ordering::Relaxed);
    }

    // register_metrics registers observable instruments over the tracker's
    // atomics on the given meter: gauges for the point-in-time pending
    // counters, and observable counters for the monotonic totals. Every
    // series is labeled client=<client> so multiple clients in one process
    // stay distinct; the value must be a compile-time literal (it is the
    // cardinality bound). Call once per tracker -- a second registration
    // would mint duplicate series.
    //
    // The callbacks read the atomics at collection time; nothing on the
    // message path changes. Note that reset_counters() shows up on the
    // observable counters as an ordinary Prometheus counter reset.
    pub fn register_metrics(&self, meter: &Meter, client: &'static str) {
        let gauges = [
            (
                "carbide_mqtt_queue_pending_messages",
                "Number of received MQTT messages currently waiting in the client's processing queue",
                &self.pending_count,
            ),
            (
                "carbide_mqtt_queue_pending_bytes",
                "Number of bytes of received MQTT messages currently waiting in the client's processing queue",
                &self.pending_bytes,
            ),
        ];
        for (name, description, value) in gauges {
            let value = value.clone();
            meter
                .u64_observable_gauge(name)
                .with_description(description)
                .with_callback(move |observer| {
                    observer.observe(
                        value.load(Ordering::Relaxed) as u64,
                        &[KeyValue::new("client", client)],
                    );
                })
                .build();
        }

        // Monotonic totals; the Prometheus exporter appends the `_total`
        // suffix, so these expose as `carbide_mqtt_messages_processed_total`
        // and so on.
        let counters = [
            (
                "carbide_mqtt_messages_processed",
                "Number of received MQTT messages successfully processed by a registered handler",
                &self.processed_count,
            ),
            (
                "carbide_mqtt_processed_bytes",
                "Number of bytes of received MQTT messages successfully processed by a registered handler",
                &self.processed_bytes,
            ),
            (
                "carbide_mqtt_messages_failed",
                "Number of received MQTT messages whose handler failed or that had no handler registered",
                &self.failed_count,
            ),
            (
                "carbide_mqtt_messages_dropped",
                "Number of received MQTT messages dropped because the client's processing queue was full",
                &self.dropped_count,
            ),
            (
                "carbide_mqtt_dropped_bytes",
                "Number of bytes of received MQTT messages dropped because the client's processing queue was full",
                &self.dropped_bytes,
            ),
            (
                "carbide_mqtt_event_loop_errors",
                "Number of connection errors encountered by the MQTT client's event loop",
                &self.event_loop_errors,
            ),
            (
                "carbide_mqtt_unmatched_topics",
                "Number of received MQTT messages whose topic matched no registered handler pattern",
                &self.unmatched_topics,
            ),
        ];
        for (name, description, total) in counters {
            let total = total.clone();
            meter
                .u64_observable_counter(name)
                .with_description(description)
                .with_callback(move |observer| {
                    observer.observe(
                        total.load(Ordering::Relaxed) as u64,
                        &[KeyValue::new("client", client)],
                    );
                })
                .build();
        }
    }

    // to_stats will create an immutable snapshot of current statistics.
    // Safe to call frequently as it only reads atomic values without locks
    // (e.g. called by client.queue_stats() for user queries).
    // Returns QueueStats struct with current counter values.
    pub fn to_stats(&self) -> QueueStats {
        QueueStats {
            pending_messages: self.pending_count.load(Ordering::Relaxed),
            pending_bytes: self.pending_bytes.load(Ordering::Relaxed),
            total_processed: self.processed_count.load(Ordering::Relaxed),
            total_failed: self.failed_count.load(Ordering::Relaxed),
            total_bytes_processed: self.processed_bytes.load(Ordering::Relaxed),
            total_bytes_dropped: self.dropped_bytes.load(Ordering::Relaxed),
            total_dropped: self.dropped_count.load(Ordering::Relaxed),
            total_event_loop_errors: self.event_loop_errors.load(Ordering::Relaxed),
            total_unmatched_topics: self.unmatched_topics.load(Ordering::Relaxed),
        }
    }
}
