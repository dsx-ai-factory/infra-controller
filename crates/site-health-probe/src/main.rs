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

mod api;
mod census;
mod clock;
mod config;
#[cfg(test)]
mod e2e_tests;
mod framework;
mod gate;
mod logging;
mod metrics;
mod nico;
mod probes;
mod readers;
mod rest;
mod snapshot;
#[cfg(test)]
mod soak_tests;
mod state;
mod state_metrics;
mod store;
mod summary;

use std::sync::Arc;
use std::time::SystemTime;

use carbide_site_health_probe_api::Family;
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

    let process_start = SystemTime::now();

    // The global meter stays unset on purpose: instrumented clients in linked
    // crates would otherwise add series beyond the probe's documented set.
    let metrics_setup = metrics_endpoint::new_metrics_setup("site-health-probe", "nico", false)?;
    // The controller defaults to ready; this process is only ready once the
    // pipeline below is running.
    metrics_setup.health_controller.set_ready(false);
    let sink = Arc::new(metrics::Metrics::new(&metrics_setup.meter));

    let gates = gate::Gates::new(&cfg.state.caps);
    gates.count_calls(
        &metrics_setup
            .meter
            .u64_counter("carbide_site_health_probe_calls")
            .with_description(
                "Calls to each API (nico-api, nico-rest-api, metrics-store) by call and result \
                 (ok, error, timeout, pushback, cancelled). cancelled is a started call whose \
                 future was dropped before it returned, e.g. by the probe's own timeout.",
            )
            .build(),
    );
    let store = Arc::new(state::store::SnapshotStore::new(&cfg.state.paging));
    if cfg.state.enabled {
        metrics_setup
            .registry
            .register(Box::new(state_metrics::StateCollector::new(
                Arc::clone(&store),
                process_start,
                cfg.state.nico_api.host_read,
            )))
            .wrap_err("register state metrics")?;
    }
    let state_sink = Arc::new(state_metrics::CountingSink::new(
        Arc::clone(&store),
        &metrics_setup.meter,
    ));

    let mut enabled = build_probes(&cfg, &gates).wrap_err("build probes")?;
    let (readers, families) = build_readers(
        &cfg,
        &gates,
        state_sink,
        Arc::clone(&store) as _,
        process_start,
    )
    .wrap_err("build state readers")?;
    enabled.extend(readers);
    if enabled.is_empty() {
        return Err(eyre!(
            "no probes enabled — refusing to run a monitor that monitors nothing"
        ));
    }
    let names: Vec<&str> = enabled.iter().map(|p| p.name()).collect();
    tracing::info!(probes = ?names, metrics_listen = %listen_addr, "starting");

    // Three shutdown stages: probes and readers stop and drain first, then
    // the /v1 API, the metrics endpoint last — so the final results of a
    // shutdown still land in /metrics-served state for as long as the
    // endpoint lives.
    let stop_probes = CancellationToken::new();
    let stop_api = CancellationToken::new();
    let stop_server = CancellationToken::new();

    let mut api_server = if cfg.state.enabled {
        let api_addr = cfg.api_listen_addr()?;
        let listener = bind(api_addr).await.wrap_err("bind the /v1 API")?;
        tracing::info!(api_listen = %listener.local_addr()?, "serving /v1");
        let metrics_store = store::PromStore::new(
            rest::client::new_http_client("")?,
            &cfg.state.metrics_store.url,
            Arc::clone(&gates.metrics_store),
        )?;
        let summaries = summary::SummaryState::new(
            Arc::new(metrics_store),
            &cfg.state.metrics_store,
            Arc::new(clock::now),
        );
        let router =
            api::router(api::ApiState::new(Arc::clone(&store), &cfg.state).with_summary(summaries));
        let stop_api = stop_api.clone();
        Some(tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stop_api.cancelled_owned())
                .await
        }))
    } else {
        None
    };

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
    // With state readers on, the pod is ready once each has published one
    // complete sweep of every family, and it stays ready: a later failed
    // read keeps that family's last snapshot.
    let readiness = tokio::spawn({
        let health = metrics_setup.health_controller.clone();
        let all_ready = store.wait_all_ready(&families);
        async move {
            all_ready.await;
            health.set_ready(true);
            tracing::info!("ready");
        }
    });

    let mut sigterm = signal(SignalKind::terminate()).wrap_err("install SIGTERM handler")?;
    let mut sigint = signal(SignalKind::interrupt()).wrap_err("install SIGINT handler")?;

    let mut scheduler_ended = false;
    let mut api_ended = false;
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
        joined = async {
            match api_server.as_mut() {
                Some(task) => task.await,
                None => std::future::pending().await,
            }
        } => {
            api_ended = true;
            Some(match joined {
                Ok(Ok(())) => eyre!("/v1 API server exited unexpectedly"),
                Ok(Err(err)) => eyre!("/v1 API server: {err}"),
                Err(err) => eyre!("/v1 API server task: {err}"),
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
    // API, then the endpoint is cancelled and joined, so the final results
    // still land in served /metrics state and no task is left mid-shutdown.
    // Tasks that already ended in the select above must not be polled again.
    tracing::info!("shutting down");
    readiness.abort();
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
    stop_api.cancel();
    if !api_ended
        && let Some(task) = api_server
        && let Err(err) = task
            .await
            .map_err(|e| eyre!("/v1 API server task: {e}"))
            .and_then(|joined| joined.map_err(|e| eyre!("/v1 API server shutdown: {e}")))
        && shutdown.is_ok()
    {
        shutdown = Err(err);
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

/// Binds the /v1 API. A bare-port address is the IPv6 wildcard, which falls
/// back to IPv4 where the node has no IPv6, like the metrics listener.
async fn bind(addr: std::net::SocketAddr) -> eyre::Result<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => Ok(listener),
        Err(err) if addr.ip() == std::net::Ipv6Addr::UNSPECIFIED => {
            tracing::warn!(error = %err, "no IPv6 wildcard, binding IPv4");
            let v4 = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, addr.port()));
            Ok(tokio::net::TcpListener::bind(v4).await?)
        }
        Err(err) => Err(err.into()),
    }
}

/// Assembles the enabled probe set from configuration. nico-api (gRPC) and
/// nico-rest-api probes come from distinct modules, each owning its
/// transport/auth machinery. Every probe calls its API through that API's
/// gate.
fn build_probes(
    cfg: &config::Config,
    gates: &gate::Gates,
) -> eyre::Result<Vec<Arc<dyn framework::Probe>>> {
    let mut out: Vec<Arc<dyn framework::Probe>> = Vec::new();
    if cfg.probes.grpc_machines.enabled {
        out.push(Arc::new(probes::nicoapi::MachinesProbe::new(
            cfg.probes.grpc_machines.clone(),
            Arc::clone(&gates.nico_api),
        )));
    }
    if cfg.probes.rest_machines.enabled {
        out.push(Arc::new(probes::restapi::new_machines_probe(
            cfg.probes.rest_machines.clone(),
            Arc::clone(&gates.rest),
        )?));
    }
    if cfg.probes.rest_instances.enabled {
        out.push(Arc::new(probes::restapi::new_instances_probe(
            cfg.probes.rest_instances.clone(),
            Arc::clone(&gates.rest),
        )?));
    }
    Ok(out)
}

/// A set of scheduled probes and readers.
type ProbeSet = Vec<Arc<dyn framework::Probe>>;

/// Builds the enabled state readers, publishing into `sink`, starts their
/// families' sweep counters at 0 and lists the families readiness waits for. Each API's readers share its gate with the
/// latency probes.
fn build_readers(
    cfg: &config::Config,
    gates: &gate::Gates,
    sink: Arc<state_metrics::CountingSink>,
    census: Arc<dyn snapshot::CensusView>,
    process_start: SystemTime,
) -> eyre::Result<(ProbeSet, Vec<Family>)> {
    use readers::devices::{DEVICE_FAMILIES, DevicesReader};
    use readers::rest_inventory::{Inventory, RestInventoryReader};
    use rest::auth::Auth;
    use rest::client::{RestClient, new_http_client};

    let mut out: ProbeSet = Vec::new();
    let mut families = Vec::new();
    let state = &cfg.state;
    if !state.enabled {
        return Ok((out, families));
    }
    let schedule = &state.readers;

    let nico: Arc<dyn nico::NicoConnector> =
        Arc::new(nico::grpc::GrpcConnector::new(state.nico_api.clone()));
    if schedule.devices.enabled {
        out.push(Arc::new(DevicesReader::new(
            schedule.devices.clone(),
            state.nico_api.batch_size,
            Arc::clone(&nico),
            Arc::clone(&gates.nico_api),
            Arc::clone(&sink) as _,
            process_start,
        )));
        families.extend(DEVICE_FAMILIES);
    }
    if schedule.rest_inventory.enabled {
        let rest = &state.rest;
        let http = new_http_client(&rest.ca)?;
        out.push(Arc::new(RestInventoryReader::new(
            schedule.rest_inventory.clone(),
            Arc::new(RestClient::new(
                http.clone(),
                &rest.target,
                Arc::clone(&gates.rest),
            )?),
            Inventory {
                provider_auth: Arc::new(Auth::new(&rest.provider.auth, http)),
                provider_org: rest.provider.org.clone(),
                site_id: state.site_id.clone(),
            },
            census,
            Arc::clone(&sink) as _,
            process_start,
        )));
        families.extend([Family::RestTotals, Family::RestSiteMatch]);
    }
    sink.start_counters(&families);
    Ok((out, families))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::config::{Config, GrpcProbe, Probes, RestProbe};

    fn config_with(grpc: bool, rest_machines: bool, rest_instances: bool) -> Config {
        Config {
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
            ..Config::default()
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
                let cfg = config_with(grpc, rest_machines, rest_instances);
                build_probes(&cfg, &gate::Gates::new(&cfg.state.caps))
                    .expect("build succeeds")
                    .iter()
                    .map(|p| p.name())
                    .collect::<Vec<_>>()
            },
        );
    }

    /// Each enabled reader is built, readiness waits for all of its
    /// families, and only those families' sweep counters start, at 0.
    #[test]
    fn build_readers_follow_the_schedule() {
        let mut cfg = Config::default();
        cfg.state.enabled = true;
        cfg.state.rest.target = "https://nico-rest-api:8388".to_string();
        let gates = gate::Gates::new(&cfg.state.caps);
        let store = Arc::new(state::store::SnapshotStore::new(&cfg.state.paging));
        let names = |cfg: &Config| {
            let setup = metrics_endpoint::new_metrics_setup("site-health-probe", "test", false)
                .expect("metrics setup");
            let sink = Arc::new(state_metrics::CountingSink::new(
                Arc::clone(&store),
                &setup.meter,
            ));
            let (readers, families) = build_readers(
                cfg,
                &gates,
                sink,
                Arc::clone(&store) as _,
                SystemTime::now(),
            )
            .expect("readers build");
            let names: Vec<&str> = readers.iter().map(|r| r.name()).collect();
            // (family, result) of every sweep counter series, all at 0.
            let mut sweeps = Vec::new();
            for family in setup.registry.gather() {
                if family.name() != "carbide_site_health_probe_sweeps_total" {
                    continue;
                }
                for metric in family.get_metric() {
                    assert_eq!(metric.counter.value(), 0.0);
                    let label = |name: &str| {
                        metric
                            .get_label()
                            .iter()
                            .find(|l| l.name() == name)
                            .map(|l| l.value().to_string())
                            .expect("label")
                    };
                    sweeps.push((label("family"), label("result")));
                }
            }
            sweeps.sort();
            let mut want: Vec<(String, String)> = families
                .iter()
                .flat_map(|f| {
                    let family = serde_json::to_value(f)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .expect("family name");
                    ["complete", "incomplete"].map(|r| (family.clone(), r.to_string()))
                })
                .collect();
            want.sort();
            assert_eq!(sweeps, want, "two zero series per enabled family only");
            (names, families.len())
        };
        assert_eq!(names(&cfg), (vec!["devices", "rest_inventory"], 10));
        cfg.state.readers.rest_inventory.enabled = false;
        assert_eq!(names(&cfg), (vec!["devices"], 8));
        cfg.state.readers.rest_inventory.enabled = true;
        cfg.state.readers.devices.enabled = false;
        assert_eq!(names(&cfg), (vec!["rest_inventory"], 2));
        cfg.state.enabled = false;
        assert_eq!(names(&cfg), (vec![], 0));
    }
}
