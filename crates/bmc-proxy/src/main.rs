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

use std::io;
use std::sync::Arc;

mod acl;
mod bmc_proxy;
mod config;
mod metrics;
mod net;
mod setup;
mod span_isolation;

use bmc_proxy::{BmcProxyError, BmcProxyParams};
use clap::Parser;
use config::{Config, ConfigError};
use setup::{SetupError, setup_logging, setup_metrics};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[clap(name = "carbide-bmc-proxy")]
struct Args {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    version: bool,

    #[clap(short, long)]
    debug: bool,

    #[clap(long)]
    config_path: String,
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("configuration error: {0}")]
    Config(Box<ConfigError>),
    #[error("error setting up bmc-proxy: {0}")]
    Setup(#[from] SetupError),
    #[error("error running bmc-proxy: {0}")]
    BmcProxy(#[from] BmcProxyError),
    #[error("error running metrics endpoint: {0}")]
    Metrics(io::Error),
}

impl From<ConfigError> for Error {
    fn from(e: ConfigError) -> Self {
        Self::Config(Box::new(e))
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let args = Args::parse();
    if args.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }

    let config = tokio::fs::read_to_string(&args.config_path)
        .await
        .map_err(|e| {
            ConfigError::Read(format!(
                "Error opening config file at {}: {}",
                args.config_path, e
            ))
        })
        .and_then(|s| Config::parse(&s))?;

    let tracing_guard = setup_logging(args.debug, &config.tracing)?;

    let mut join_set = JoinSet::new();
    let cancel_token = CancellationToken::new();

    // Run metrics endpoint
    let metrics_setup = setup_metrics()?;
    let meter = metrics_setup.meter.clone();
    carbide_instrument::log_events::register(&meter);
    metrics::start(
        config.metrics_endpoint,
        metrics_setup,
        cancel_token.clone(),
        &mut join_set,
    )
    .await
    .map_err(Error::Metrics)?;

    // Run the BMC proxy
    bmc_proxy::start(
        BmcProxyParams {
            config: Arc::new(config),
        },
        cancel_token.clone(),
        &mut join_set,
    )
    .await?;

    // Cancel things when we get a ctrl+c
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        cancel_token.cancel();
    });

    // Wait until tasks are complete, propagating any panics
    join_set.join_all().await;

    tracing_guard.shutdown().await;

    Ok(())
}
