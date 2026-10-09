// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Standalone NICo PostgreSQL exporter. Database failures do not stop the metrics server.

mod collector;
mod config;
mod metrics;

use clap::Parser;
use eyre::{WrapErr, eyre};
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let config = config::Config::parse();
    tracing_subscriber::registry()
        .with(
            logfmt::layer().with_event_fields([logfmt::EventField::with_default(
                "component",
                "nico-postgres-exporter",
            )]),
        )
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .try_init()
        .map_err(|error| eyre!("initialize logging: {error}"))?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(collector::PROBE_TIMEOUT)
        .connect_lazy_with(config.connect_options()?);
    let setup = metrics_endpoint::new_metrics_setup("nico-postgres-exporter", "nico", false)?;
    let metrics = metrics::Metrics::new(&setup.meter);
    let endpoint = metrics_endpoint::MetricsEndpointConfig {
        address: config.listen,
        registry: setup.registry.clone(),
        health_controller: Some(setup.health_controller.clone()),
        additional_prefix: None,
    };
    let listener = metrics_endpoint::bind_tcp_listener(config.listen)
        .await
        .wrap_err("bind metrics endpoint")?;

    tracing::info!(listen = %config.listen, "starting PostgreSQL exporter");

    // Launch tasks in a JoinSet, cancelled via sigint or sigterm.
    let mut join_set: JoinSet<eyre::Result<()>> = JoinSet::new();
    let cancel_token = carbide_utils::shutdown_handler::start()?;

    // Spawn metrics endpoint
    join_set.spawn({
        let cancel_token = cancel_token.clone();
        async move {
            metrics_endpoint::run_metrics_endpoint_with_listener(&endpoint, cancel_token, listener)
                .await
                .wrap_err("running metrics endpoint")
        }
    });

    // Spawn metrics collector
    join_set.spawn({
        let cancel_token = cancel_token.clone();
        async move {
            collector::run(&pool, &metrics, cancel_token).await;
            Ok(())
        }
    });

    // Wait for all tasks to finish
    while let Some(result) = join_set.join_next().await {
        result.expect("task paniced")?;
    }

    Ok(())
}
