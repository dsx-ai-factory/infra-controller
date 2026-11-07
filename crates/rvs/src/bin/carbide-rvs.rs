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

//! Rack Validation Service (RVS)
//!
//! External validation orchestrator for NICo. Bridges NICo with test
//! frameworks (Benchpress, MPI-based, SLURM-based, etc.) to perform
//! partition-aware rack validation.

use std::path::PathBuf;

use carbide_rvs::config::Config;
use carbide_rvs::ctx::RvsCtx;
use carbide_rvs::error::RvsError;
use carbide_rvs::partitions::Partitions;
use carbide_rvs::{artifact, client, rack, scenario, validation};
use clap::Parser;
use forge_tls::client_config::ClientCert;
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
#[command(about = "Rack Validation Service")]
struct Cli {
    /// Path to TOML config file. Defaults and CARBIDE_RVS__* env vars apply if omitted.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), RvsError> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(
            logfmt::layer()
                .with_event_fields([logfmt::EventField::with_default("component", "nico-rvs")]),
        )
        .with(env_filter)
        .init();

    tracing::info!("carbide-rvs: Rack Validation Service starting");

    let cli = Cli::parse();

    // Load config: defaults -> optional TOML -> CARBIDE_RVS__* env vars
    let cfg = Config::load(cli.config.as_deref())?;
    tracing::info!(config = ?cfg, "config loaded");

    // Load all scenarios -- soft fail per file so a single bad config doesn't block others.
    let scenarios: Vec<scenario::Scenario> = cfg
        .scenario_config_paths
        .iter()
        .filter_map(|path| {
            match scenario::Scenario::load(std::path::Path::new(path)) {
                Ok(s) => {
                    tracing::info!(path, model = %s.rack.model, sot_release = %s.rack.sot_release, "scenario loaded");
                    Some(s)
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "scenario not loaded, skipping");
                    None
                }
            }
        })
        .collect();

    // Build NICo client from config
    let client_cert = ClientCert {
        cert_path: cfg.tls.identity_pemfile_path.clone(),
        key_path: cfg.tls.identity_keyfile_path.clone(),
    };
    let client_config = ForgeClientConfig::new(cfg.tls.root_cafile_path.clone(), Some(client_cert));
    let api_config = ApiConfig::new(&cfg.nico.url, &client_config);
    let nico = client::NicoClient::new(&api_config);

    let ctx = RvsCtx {
        nico,
        scenarios,
        cfg,
    };

    // TODO[#416]: re-introduce a liveness/health probe (bound to
    // `cfg.metrics_endpoint`) once RVS runs as a long-lived service with
    // graceful shutdown and real health checks. For now, "alive" just means
    // the process is running -- a stub probe would only echo 200 and buys
    // nothing.

    let cancel_token = CancellationToken::new();
    let validation_cancel_token = cancel_token.clone();

    tokio::spawn(async move {
        let Ok(mut sigint) = signal(SignalKind::interrupt()) else {
            return;
        };
        let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
            return;
        };
        loop {
            // Wait for SIGINT or SIGTERM
            let received_signal = tokio::select! {
                _ = sigint.recv() => "SIGINT",
                _ = sigterm.recv() => "SIGTERM",
            };

            if cancel_token.is_cancelled() {
                std::process::exit(130);
            } else {
                eprintln!(
                    "{received_signal} received, shutting down gracefully. Send {received_signal} again to exit."
                );
                cancel_token.cancel();
            }
        }
    });

    run_validation(&ctx, validation_cancel_token).await
}

// Rack validation high-level flow
async fn run_validation(ctx: &RvsCtx, cancel_token: CancellationToken) -> Result<(), RvsError> {
    artifact::start_cache_server(ctx).await?;
    let poll_interval_secs = ctx.cfg.poll_interval_secs;
    let interval = std::time::Duration::from_secs(poll_interval_secs);
    loop {
        let racks = rack::fetch_racks(&ctx.nico).await?;
        artifact::process_artifacts(&racks, ctx).await?;
        let os_uri = ctx
            .scenarios
            .first()
            .map(|s| s.os.uri.as_str())
            .unwrap_or("");
        for job in validation::plan(Partitions::try_from(racks)?, &ctx.nico, os_uri).await? {
            let report = validation::validate_partition(job).await?;
            validation::submit_report(report).await?;
        }
        tracing::info!(
            poll_interval_seconds = poll_interval_secs,
            "validation: cycle complete, sleeping"
        );
        if cancel_token
            .run_until_cancelled(tokio::time::sleep(interval))
            .await
            .is_none()
        {
            break Ok(());
        }
    }
}
