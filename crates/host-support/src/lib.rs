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

//! carbide-host-support is a library that is used by applications that run on
//! carbide managed hosts

use std::sync::Once;

use tracing::metadata::LevelFilter;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;

pub mod agent_config;
pub mod bootstrap_ca;
pub mod dpa_cmds;
#[cfg(feature = "linux-build")]
pub mod hardware_enumeration;
#[cfg(feature = "linux-build")]
pub mod lldp_collector;
#[cfg(feature = "linux-build")]
pub mod lldp_collector_task;
#[cfg(feature = "linux-build")]
pub mod lldp_snapshot_cache;
pub mod registration;

static LOG_SETUP: Once = Once::new();

/// Initialize global logging output to STDOUT. Applies to all threads.
/// Use `export RUST_LOG=trace|debug|info|warn|error` to change log level.
///
/// `component` tags every log line with `component=<value>` (e.g. `nico-scout`,
/// `nico-dpu-agent`) so logs can be filtered by the emitting binary. It must be
/// passed by the caller because this setup is shared across binaries.
pub fn init_logging(component: &str) -> eyre::Result<()> {
    LOG_SETUP.call_once(|| {
        let log_events = carbide_instrument::LogEventsMetric::new(component.to_string());
        let subscriber = tracing_subscriber::registry()
            .with(log_events.layer().with_filter(base_env_filter()))
            .with(stdout_layer(component).with_filter(base_env_filter()));
        subscriber
            .try_init()
            .expect("tracing_subscriber setup failed");
    });
    Ok(())
}

// A logging subscriber for use on the current thread.
// Usually you want `init_logging()` instead.
//
// Usage: `let guard = subscriber("nico-scout").set_default()`
// Subscriber is unregistered when guard is dropped.
pub fn subscriber(component: &str) -> impl SubscriberInitExt {
    Box::new(
        tracing_subscriber::registry().with(stdout_layer(component).with_filter(base_env_filter())),
    )
}

fn base_env_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("tower=warn".parse().unwrap())
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("sqlx=info".parse().unwrap())
        .add_directive("tokio_util::codec=warn".parse().unwrap())
        .add_directive("h2=warn".parse().unwrap())
        .add_directive("hickory_resolver::error=info".parse().unwrap())
        .add_directive("hickory_proto::xfer=info".parse().unwrap())
        .add_directive("hickory_resolver::name_server=info".parse().unwrap())
        .add_directive("hickory_proto=info".parse().unwrap())
        .add_directive("netlink_proto=warn".parse().unwrap())
}

fn stdout_layer<S>(component: &str) -> logfmt::LogFmtLayer<S>
where
    S: tracing::Subscriber + for<'span> tracing_subscriber::registry::LookupSpan<'span>,
{
    logfmt::layer().with_event_fields([logfmt::EventField::with_default("component", component)])
}
