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

use carbide_health::{Config, HealthError};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<(), HealthError> {
    let config_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    let config = Config::load(config_path.as_deref()).map_err(HealthError::GenericError)?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    // Counts events from startup; `run_service` registers the metric with the
    // framework registry its /metrics response exposes.
    let log_events = carbide_instrument::LogEventsMetric::new("nico-hardware-health");
    tracing_subscriber::registry()
        .with(log_events.layer())
        .with(
            logfmt::layer().with_event_fields([logfmt::EventField::with_default(
                "component",
                "nico-hardware-health",
            )]),
        )
        .with(env_filter)
        .init();

    tracing::info!(
        version = carbide_version::v!(build_version),
        config = ?config,
        "Started carbide-hw-health"
    );

    carbide_health::run_service(config).await?;

    tracing::info!(
        version = carbide_version::v!(build_version),
        "Stopped carbide-hw-health"
    );

    Ok(())
}
