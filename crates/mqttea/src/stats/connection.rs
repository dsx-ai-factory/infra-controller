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

// src/mqttea/stats/connection.rs
// Broker connection state tracking.
//
// Holds the client's point-in-time connection flag: the event loop sets
// it on every ConnAck, clears it on every connection error, and
// disconnect() clears it on a clean shutdown. Lock-free, like the other
// stats trackers.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;

// ConnectionStateTracker enables thread-safe updates to the client's
// broker connection state using atomic operations.
#[derive(Debug)]
pub struct ConnectionStateTracker {
    // connected is true while the client holds an acknowledged broker
    // connection (a ConnAck arrived and no connection error has been
    // seen since).
    connected: Arc<AtomicBool>,
}

impl Default for ConnectionStateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionStateTracker {
    // new creates a ConnectionStateTracker in the disconnected state
    // (e.g. used during MqtteaClient initialization, before connect()).
    pub fn new() -> Self {
        Self {
            connected: Arc::new(AtomicBool::new(false)),
        }
    }

    // set_connected records a connection state transition. The event loop
    // calls this with true on every ConnAck and with false on every
    // connection error; disconnect() calls it with false.
    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
    }

    // is_connected reports whether the client currently holds an
    // acknowledged broker connection.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    // register_metrics registers an observable gauge over the connection
    // flag on the given meter. The series is labeled client=<client> so
    // multiple clients in one process stay distinct; the value must be a
    // compile-time literal (it is the cardinality bound). Call once per
    // tracker -- a second registration would mint duplicate series.
    pub fn register_metrics(&self, meter: &Meter, client: &'static str) {
        let connected = self.connected.clone();
        meter
            .u64_observable_gauge("carbide_mqtt_connected")
            .with_description(
                "Number of active broker connections held by the MQTT client (1 while connected, 0 otherwise)",
            )
            .with_callback(move |observer| {
                observer.observe(
                    connected.load(Ordering::Relaxed) as u64,
                    &[KeyValue::new("client", client)],
                );
            })
            .build();
    }
}
