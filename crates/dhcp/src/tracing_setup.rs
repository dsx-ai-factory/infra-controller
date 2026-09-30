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

//! Span export for the Kea hook library.
//!
//! Kea owns the process and its logging, so this installs a subscriber that
//! only exports spans. Hook log lines keep going through [`crate::LOGGER`] into
//! Kea's logger.

use std::sync::Mutex;

use carbide_instrument::otlp_tracing::{self, Status};
use once_cell::sync::Lazy;
use tokio::runtime::{Builder, Runtime};
use tracing_subscriber::layer::SubscriberExt;

/// Drives the exporter's gRPC connection. The hook's shared runtime is
/// current-thread and only makes progress inside a `block_on` call, which would
/// hold an export until the next DHCP packet arrives.
static EXPORT_RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("nico-dhcp-span-export")
        .enable_all()
        .build()
        .expect("unable to build span export runtime?")
});

static TRACING: Mutex<Option<otlp_tracing::Tracing>> = Mutex::new(None);

/// Installs the span-export subscriber. Call once, from the hook's `load`.
pub(crate) fn init() {
    let guard = EXPORT_RUNTIME.enter();
    let (span_layer, tracing) = otlp_tracing::setup(otlp_tracing::Config::new("nico-dhcp"));
    drop(guard);

    // Sets the subscriber directly instead of calling `try_init`, which also
    // installs tracing-log's `LogTracer` as the global `log` logger. Kea's logger
    // already holds that slot, so `try_init` fails after setting the subscriber.
    let subscriber = tracing_subscriber::registry().with(span_layer);
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        log::warn!("a tracing subscriber is already installed; not exporting spans");
        return;
    }

    // Hook logs go to Kea, not to `tracing`, so report the result through `log`.
    match tracing.status() {
        Status::Off => log::info!("no OTLP endpoint configured; span export off"),
        Status::On { endpoint } => log::info!("exporting spans over OTLP/gRPC to {endpoint}"),
        Status::Failed { endpoint, error } => log::warn!(
            "OTLP span exporter for {endpoint} could not be built; continuing without span export: {error}"
        ),
    }

    *TRACING.lock().expect("tracing lock poisoned?") = Some(tracing);
}

/// Sends the spans the exporter has not batched yet. Call from the hook's `unload`.
pub(crate) fn shutdown() {
    let Some(tracing) = TRACING.lock().expect("tracing lock poisoned?").take() else {
        return;
    };

    EXPORT_RUNTIME.block_on(tracing.shutdown());
}
