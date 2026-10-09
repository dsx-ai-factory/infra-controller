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

use std::path::PathBuf;

use clap::Parser;
use figment::Figment;
use figment::providers::{Format, Toml};
use metrics_endpoint::{
    MetricsEndpointConfig, new_metrics_setup, run_metrics_endpoint_with_cancellation,
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use ufm_mock::{StandaloneConfig, UfmAuthToken, UfmMock};

#[derive(Debug, Parser)]
#[command(name = "ufm-mock")]
struct Args {
    #[arg(help = "UFM mock TOML configuration file")]
    config_file: PathBuf,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| eyre::eyre!(error.to_string()))?;

    let args = Args::parse();
    let mut config: StandaloneConfig = Figment::new()
        .merge(Toml::file(&args.config_file))
        .extract()?;
    // Hosted execution uses `enabled` to activate an optional configured section. Launching the
    // standalone binary is itself the activation decision.
    config.mock.enabled = true;
    config.mock.validate()?;

    let auth_token = UfmAuthToken::from_environment()?;
    let metrics_setup = new_metrics_setup("carbide-ufm-mock", "carbide", false)?;
    let ufm_mock = UfmMock::new(&config.mock, &auth_token, &metrics_setup.meter)?;
    let cancellation = CancellationToken::new();
    // Standalone execution has no machine-a-tron control state to use as an in-process provider.
    // Reconciliation therefore obtains inventory only from `inventory.static_sources`.
    let reconciliation = ufm_mock.start_reconciliation(
        config.mock.inventory.clone(),
        None,
        cancellation.child_token(),
    );

    let metrics_task = config.mock.metrics_address.map(|address| {
        let metrics_config = MetricsEndpointConfig {
            address,
            registry: metrics_setup.registry,
            health_controller: Some(metrics_setup.health_controller),
            additional_prefix: None,
        };
        let metrics_cancellation = cancellation.child_token();
        tokio::spawn(async move {
            if let Err(error) =
                run_metrics_endpoint_with_cancellation(&metrics_config, metrics_cancellation).await
            {
                tracing::error!(error = %error, "UFM mock metrics endpoint stopped");
            }
        })
    });
    let _metrics_guard = metrics_setup.meter_provider;

    let signal_cancellation = cancellation.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });

    tracing::info!(
        address = %config.listen_address,
        "Starting UFM mock"
    );
    let server_result = ufm_mock::serve(
        config.listen_address,
        config.tls,
        ufm_mock.router(),
        cancellation.clone(),
    )
    .await;

    cancellation.cancel();
    reconciliation.await?;
    if let Some(metrics_task) = metrics_task {
        metrics_task.await?;
    }
    server_result
}
