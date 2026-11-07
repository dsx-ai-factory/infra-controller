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

use carbide_dsx_exchange_consumer::{Config, DsxConsumerError};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

#[tokio::main]
async fn main() -> Result<(), DsxConsumerError> {
    let config_path = std::env::args().nth(1).map(std::path::PathBuf::from);
    let config = Config::load(config_path.as_deref()).map_err(DsxConsumerError::Config)?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let log_events = carbide_instrument::LogEventsMetric::new("nico-dsx-exchange-consumer");
    tracing_subscriber::registry()
        .with(log_events.layer().with_filter(env_filter.clone()))
        .with(
            logfmt::layer()
                .with_event_fields([logfmt::EventField::with_default(
                    "component",
                    "nico-dsx-exchange-consumer",
                )])
                .with_filter(env_filter),
        )
        .try_init()
        .map_err(|e| DsxConsumerError::Config(e.to_string()))?;

    tracing::info!(
        version = carbide_version::v!(build_version),
        config = ?config,
        "Started carbide-dsx-exchange-consumer"
    );

    carbide_dsx_exchange_consumer::run_service(config).await?;

    tracing::info!(
        version = carbide_version::v!(build_version),
        "Stopped carbide-dsx-exchange-consumer"
    );

    Ok(())
}
