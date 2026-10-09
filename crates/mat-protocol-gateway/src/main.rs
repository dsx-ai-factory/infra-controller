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
use mat_protocol_gateway::{ExitReason, GatewayConfig, run};
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use ufm_mock::UfmAuthToken;

#[derive(Debug, Parser)]
#[command(name = "mat-protocol-gateway")]
struct Args {
    #[arg(
        help = "Gateway TOML configuration file; every key can also be set through MAT_PROTOCOL_GATEWAY__ environment variables"
    )]
    config_file: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| eyre::eyre!(error.to_string()))?;

    let args = Args::parse();
    let config = GatewayConfig::load(args.config_file.as_deref())?;
    let auth_token = UfmAuthToken::from_environment()?;

    let shutdown = CancellationToken::new();
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
        tracing::info!("Shutdown signal received");
        signal_shutdown.cancel();
    });

    let reason = run(config, &auth_token, shutdown).await?;
    match &reason {
        ExitReason::Shutdown => Ok(()),
        ExitReason::SourceListChanged(change) => {
            tracing::warn!(
                startup = %change.startup,
                current = %change.current,
                added = ?change.added,
                removed = ?change.removed,
                exit_code = reason.exit_code(),
                "Exiting so the container restarts against the current source set"
            );
            std::process::exit(reason.exit_code());
        }
    }
}
