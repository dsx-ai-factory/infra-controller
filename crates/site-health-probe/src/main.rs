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

//! site-health-probe runs synthetic probes against a NICo site's APIs and
//! exposes the results as Prometheus metrics (issue #5360).

mod config;
mod framework;
mod logging;
mod metrics;
mod probes;

use std::sync::Arc;

use clap::Parser;
use eyre::{WrapErr, eyre};
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(about = "Synthetic monitoring probes against a NICo site's APIs")]
struct Args {
    /// Path to the YAML configuration file.
    #[arg(long, default_value = "/etc/nico/site-health-probe/config.yaml")]
    config: String,
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        // The logfmt subscriber is installed first thing in run(), so this is
        // rendered like every other log line.
        tracing::error!(error = format!("{err:#}"), "fatal");
        std::process::exit(1);
    }
}

async fn run() -> eyre::Result<()> {
    let args = Args::parse();
    logging::init_logging().map_err(|e| eyre!("initialize logging: {e}"))?;

    let cfg = config::Config::load(&args.config).wrap_err("configuration")?;
    let listen_addr = cfg.listen_addr()?;

    let enabled = build_probes(&cfg).wrap_err("build probes")?;
    if enabled.is_empty() {
        return Err(eyre!(
            "no probes enabled — refusing to run a monitor that monitors nothing"
        ));
    }
    let names: Vec<&str> = enabled.iter().map(|p| p.name()).collect();
    tracing::info!(probes = ?names, metrics_listen = %listen_addr, "starting");

    // The global meter stays unset on purpose: instrumented clients in linked
    // crates would otherwise add series beyond the probe's documented set.
    let metrics_setup = metrics_endpoint::new_metrics_setup("site-health-probe", "nico", false)?;
    // The controller defaults to ready; this process is only ready once the
    // pipeline below is running.
    metrics_setup.health_controller.set_ready(false);
    let sink = Arc::new(metrics::Metrics::new(&metrics_setup.meter));

    // Two shutdown stages: probes stop and drain first, the metrics endpoint
    // last — so the final results of a shutdown still land in /metrics-served
    // state for as long as the endpoint lives.
    let stop_probes = CancellationToken::new();
    let stop_server = CancellationToken::new();

    let mut server = tokio::spawn({
        let endpoint_config = metrics_endpoint::MetricsEndpointConfig {
            address: listen_addr,
            registry: metrics_setup.registry.clone(),
            health_controller: Some(metrics_setup.health_controller.clone()),
            additional_prefix: None,
        };
        let stop_server = stop_server.clone();
        async move {
            metrics_endpoint::run_metrics_endpoint_with_cancellation(&endpoint_config, stop_server)
                .await
        }
    });

    // The pipeline: probe loops produce results on a channel; the collector
    // consumes them into the metrics sink. Future consumers (e.g. site-stats
    // aggregation) attach at this seam.
    let (results, loops) = framework::start(enabled, stop_probes.clone());
    let mut scheduler = tokio::spawn(framework::join_loops(loops));
    let collector = tokio::spawn(framework::collect(results, sink));
    metrics_setup.health_controller.set_ready(true);

    let mut sigterm = signal(SignalKind::terminate()).wrap_err("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("install SIGINT handler")?;

    let mut scheduler_ended = false;
    let mut server_ended = false;
    let failure = tokio::select! {
        _ = sigterm.recv() => None,
        _ = sigint.recv() => None,
        joined = &mut scheduler => {
            // Probe loops only end early on a panic — a scheduler bug.
            scheduler_ended = true;
            Some(match joined {
                Ok(Ok(())) => eyre!("probe loops exited unexpectedly"),
                Ok(Err(err)) => err,
                Err(err) => eyre!("probe supervisor task: {err}"),
            })
        }
        joined = &mut server => {
            server_ended = true;
            Some(match joined {
                Ok(Ok(())) => eyre!("metrics server exited unexpectedly"),
                Ok(Err(err)) => eyre!("metrics server: {err}"),
                Err(err) => eyre!("metrics server task: {err}"),
            })
        }
    };

    // Shutdown in dependency order: probes stop and drain first, then the
    // endpoint is cancelled and joined, so the final results still land in
    // served /metrics state and no task is left mid-shutdown. Tasks that
    // already ended in the select above must not be polled again.
    tracing::info!("shutting down");
    metrics_setup.health_controller.set_ready(false);
    stop_probes.cancel();
    let mut shutdown: eyre::Result<()> = Ok(());
    if !scheduler_ended
        && let Err(err) = scheduler
            .await
            .map_err(|e| eyre!("probe supervisor task: {e}"))
            .and_then(|joined| joined)
    {
        shutdown = Err(err);
    }
    if let Err(err) = collector.await
        && shutdown.is_ok()
    {
        shutdown = Err(eyre!("collector task: {err}"));
    }
    stop_server.cancel();
    if !server_ended
        && let Err(err) = server
            .await
            .map_err(|e| eyre!("metrics server task: {e}"))
            .and_then(|joined| joined.map_err(|e| eyre!("metrics server shutdown: {e}")))
        && shutdown.is_ok()
    {
        shutdown = Err(err);
    }

    match failure {
        Some(err) => Err(err),
        None => shutdown,
    }
}

/// Assembles the enabled probe set from configuration. nico-api (gRPC) and
/// nico-rest-api probes come from distinct modules, each owning its
/// transport/auth machinery.
fn build_probes(cfg: &config::Config) -> eyre::Result<Vec<Arc<dyn framework::Probe>>> {
    let mut out: Vec<Arc<dyn framework::Probe>> = Vec::new();
    if cfg.probes.grpc_machines.enabled {
        out.push(Arc::new(probes::nicoapi::MachinesProbe::new(
            cfg.probes.grpc_machines.clone(),
        )));
    }
    if cfg.probes.rest_machines.enabled {
        out.push(Arc::new(probes::restapi::new_machines_probe(
            cfg.probes.rest_machines.clone(),
        )?));
    }
    if cfg.probes.rest_instances.enabled {
        out.push(Arc::new(probes::restapi::new_instances_probe(
            cfg.probes.rest_instances.clone(),
        )?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::config::{Config, GrpcProbe, Probes, RestProbe};

    fn config_with(grpc: bool, rest_machines: bool, rest_instances: bool) -> Config {
        Config {
            metrics_listen: String::new(),
            probes: Probes {
                grpc_machines: GrpcProbe {
                    enabled: grpc,
                    ..GrpcProbe::default()
                },
                rest_machines: RestProbe {
                    enabled: rest_machines,
                    ..RestProbe::default()
                },
                rest_instances: RestProbe {
                    enabled: rest_instances,
                    ..RestProbe::default()
                },
            },
        }
    }

    /// Every probe toggles independently, and the build order is fixed:
    /// grpc_machines, rest_machines, rest_instances.
    #[test]
    fn build_probes_combinations() {
        check_values(
            [
                Check {
                    scenario: "all enabled, fixed order",
                    input: (true, true, true),
                    expect: vec!["grpc_machines", "rest_machines", "rest_instances"],
                },
                Check {
                    scenario: "grpc alone",
                    input: (true, false, false),
                    expect: vec!["grpc_machines"],
                },
                Check {
                    scenario: "rest machines alone",
                    input: (false, true, false),
                    expect: vec!["rest_machines"],
                },
                Check {
                    scenario: "rest instances alone",
                    input: (false, false, true),
                    expect: vec!["rest_instances"],
                },
                Check {
                    scenario: "all disabled builds nothing",
                    input: (false, false, false),
                    expect: vec![],
                },
            ],
            |(grpc, rest_machines, rest_instances)| {
                build_probes(&config_with(grpc, rest_machines, rest_instances))
                    .expect("build succeeds")
                    .iter()
                    .map(|p| p.name())
                    .collect::<Vec<_>>()
            },
        );
    }
}
