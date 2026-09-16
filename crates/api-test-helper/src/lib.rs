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

mod api_client;

pub mod api_server;
pub mod dns;
pub mod domain;
pub mod instance;
pub mod machine;
pub mod machine_a_tron;
pub mod metrics;
pub mod mock_rms;
pub mod subnet;
pub mod tenant;
pub mod utils;
pub mod vault;
pub mod vpc;
pub mod vpc_prefix;

pub use utils::IntegrationTestEnvironment;

pub fn setup_logging() {
    use tracing::metadata::LevelFilter;
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::fmt::TestWriter;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;

    if let Err(e) = tracing_subscriber::registry()
        .with(carbide_instrument::LogEventsMetric::new("nico-api").layer())
        .with(
            tracing_subscriber::fmt::Layer::default()
                .compact()
                .with_writer(TestWriter::new),
        )
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy()
                .add_directive("sqlx=warn".parse().unwrap())
                .add_directive("tower=warn".parse().unwrap())
                .add_directive("rustify=off".parse().unwrap())
                .add_directive("rustls=warn".parse().unwrap())
                .add_directive("hyper=warn".parse().unwrap())
                .add_directive("h2=warn".parse().unwrap())
                // Suppress expected, repetitive environment diagnostics in integration tests.
                .add_directive("carbide_diagnostics=off".parse().unwrap())
                // Silence permissive mode related messages
                .add_directive("carbide_api_core::auth=error".parse().unwrap()),
        )
        .try_init()
    {
        // Note: Resist the temptation to ignore this error. We really should only have one place in
        // the test binary that initializes logging.
        panic!(
            "Failed to initialize trace logging for api-test tests. It's possible some earlier \
            code path has already set a global default log subscriber: {e}"
        );
    }
}
