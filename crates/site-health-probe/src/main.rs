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
mod state;
mod state_metrics;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the metrics store history moves to the scale test runner"
    )
)]
mod store;
#[expect(
    dead_code,
    reason = "the metrics store history moves to the scale test runner"
)]
mod summary;

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use carbide_site_health_probe_api::Family;
use clap::Parser;
use eyre::{WrapErr, eyre};
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long requests in flight on the /v1 API may finish at shutdown. The
/// pod's grace period (30 s by default) also covers the probes' drain before
/// and the metrics endpoint after.
const API_DRAIN: Duration = Duration::from_secs(10);
/// How long the /v1 connections get to close once their requests are cut
/// short, and again once their I/O is closed, before the server is stopped.
const API_CLOSE: Duration = Duration::from_secs(1);

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
                "Calls to each API (nico-api, nico-rest-api) by call and result \
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
    let api_shutdown = ApiShutdown::default();
    let stop_server = CancellationToken::new();

    let mut api_server = if cfg.state.enabled {
        let api_addr = cfg.api_listen_addr()?;
        let listener = bind(api_addr).await.wrap_err("bind the /v1 API")?;
        tracing::info!(api_listen = %listener.local_addr()?, "serving /v1");
        let router = api::router(api::ApiState::new(Arc::clone(&store), &cfg.state));
        Some(serve_api(
            listener,
            router,
            stop_api.clone(),
            api_shutdown.clone(),
        ))
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
    stop_task(readiness).await;
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
        && let Err(err) = drain(task, API_DRAIN, &api_shutdown)
            .await
            .map_err(|e| eyre!("/v1 API server task: {e}"))
            .and_then(|joined| {
                joined
                    .unwrap_or(Ok(()))
                    .map_err(|e| eyre!("/v1 API server shutdown: {e}"))
            })
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

/// Binds the /v1 API like the metrics listener: a bare-port address is the
/// dual-stack IPv6 wildcard, which falls back to IPv4 only where the node
/// has no IPv6. A port in use stays an error.
async fn bind(addr: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    metrics_endpoint::bind_tcp_listener(addr).await
}

/// Aborts `task` and waits until it no longer runs: an abort alone lets a
/// task in the middle of a poll finish that poll.
async fn stop_task(task: JoinHandle<()>) {
    task.abort();
    let _ = task.await;
}

/// How the /v1 server's work ends at shutdown, past its graceful drain.
#[derive(Clone, Default)]
struct ApiShutdown {
    /// Cuts every request short, its body included.
    requests: CancellationToken,
    /// Fails every read and write of every connection, so even a reply its
    /// client never reads ends.
    connections: CancellationToken,
}

/// Serves the /v1 API until `stop`, then finishes the requests in flight
/// until `shutdown` ends them.
fn serve_api(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    stop: CancellationToken,
    shutdown: ApiShutdown,
) -> JoinHandle<std::io::Result<()>> {
    let router = api::cancellable(router, shutdown.requests);
    let listener = api::closable::ClosableListener::new(listener, shutdown.connections);
    tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stop.cancelled_owned())
            .await
    })
}

/// Waits up to `limit` for the stopped server `task` to finish its
/// requests. Then it cuts the requests short and, [`API_CLOSE`] later,
/// closes the connections' I/O, each time waiting [`API_CLOSE`] for the
/// server to end. `None` when even that did not end it, and it was stopped.
async fn drain<T>(
    mut task: JoinHandle<T>,
    limit: Duration,
    shutdown: &ApiShutdown,
) -> Result<Option<T>, tokio::task::JoinError> {
    if let Ok(joined) = tokio::time::timeout(limit, &mut task).await {
        return joined.map(Some);
    }
    tracing::warn!(
        ?limit,
        "/v1 requests still in flight at shutdown, cutting them short"
    );
    shutdown.requests.cancel();
    if let Ok(joined) = tokio::time::timeout(API_CLOSE, &mut task).await {
        return joined.map(Some);
    }
    tracing::warn!("/v1 connections still open at shutdown, closing them");
    shutdown.connections.cancel();
    if let Ok(joined) = tokio::time::timeout(API_CLOSE, &mut task).await {
        return joined.map(Some);
    }
    tracing::warn!("/v1 server still running at shutdown, stopping it");
    task.abort();
    match task.await {
        Err(err) if err.is_cancelled() => Ok(None),
        joined => joined.map(Some),
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
            schedule.devices.enabled.then_some(census),
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

    /// A bind whose port is in use fails: an explicit address always, and
    /// the wildcard without moving to IPv4.
    #[tokio::test]
    async fn api_bind_keeps_port_in_use() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let err = bind(held.local_addr().expect("address"))
            .await
            .expect_err("the port is in use");
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);

        // An IPv6-only listener on the wildcard, which an IPv4 bind would
        // not see.
        let holder = || -> std::io::Result<socket2::Socket> {
            let socket = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None)?;
            socket.set_only_v6(true)?;
            socket
                .bind(&std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)).into())?;
            socket.listen(1)?;
            Ok(socket)
        };
        let holder = match holder() {
            Ok(holder) => holder,
            Err(err) => {
                eprintln!("skipping the IPv6 wildcard case, no IPv6 here: {err}");
                return;
            }
        };
        let port = holder
            .local_addr()
            .ok()
            .and_then(|a| a.as_socket())
            .expect("address")
            .port();
        let err = bind(std::net::SocketAddr::from((
            std::net::Ipv6Addr::UNSPECIFIED,
            port,
        )))
        .await
        .expect_err("the port is in use");
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }

    /// `stop_task` returns only once its task no longer runs, even when the
    /// task is in the middle of a poll: run() relies on it so a stopped
    /// readiness task cannot mark the pod ready after shutdown starts.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_task_waits_out_a_running_poll() {
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (polling, polled) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let task = tokio::spawn({
            let ready = Arc::clone(&ready);
            async move {
                // All families ready; the poll is still running.
                polling.send(()).expect("test waits");
                released.recv().expect("test releases");
                ready.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        tokio::task::spawn_blocking(move || polled.recv().expect("task polls"))
            .await
            .expect("join");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            release.send(()).expect("task waits");
        });
        stop_task(task).await;
        ready.store(false, std::sync::atomic::Ordering::SeqCst);
        releaser.join().expect("releaser");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!ready.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// A server that does not end even after its requests are cut short is
    /// stopped, and that is not a shutdown error.
    #[tokio::test(start_paused = true)]
    async fn api_drain_is_bounded() {
        let started = tokio::time::Instant::now();
        let shutdown = ApiShutdown::default();
        let stuck = tokio::spawn(std::future::pending::<()>());
        assert!(matches!(drain(stuck, API_DRAIN, &shutdown).await, Ok(None)));
        assert_eq!(started.elapsed(), API_DRAIN + API_CLOSE * 2);
        assert!(shutdown.requests.is_cancelled() && shutdown.connections.is_cancelled());

        let shutdown = ApiShutdown::default();
        let done = tokio::spawn(async { 7 });
        assert!(matches!(
            drain(done, API_DRAIN, &shutdown).await,
            Ok(Some(7))
        ));
        assert!(!shutdown.requests.is_cancelled(), "nothing to cut short");
    }

    struct SetOnDrop(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for SetOnDrop {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Reads what the server sends until it closes the connection.
    async fn until_closed(client: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;

        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
            .await
            .expect("the server closes the connection")
            .expect("read");
        String::from_utf8_lossy(&reply).into_owned()
    }

    /// At shutdown, a request whose body never finishes and a handler that
    /// never returns are cut short with 503, both connections close with the
    /// clients still open, and the server ends on its own.
    #[tokio::test]
    async fn api_shutdown_cuts_stalled_requests_short() {
        use tokio::io::AsyncWriteExt;

        let cfg = Config::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let router = api::router(api::ApiState::new(
            Arc::new(state::store::SnapshotStore::new(&cfg.state.paging)),
            &cfg.state,
        ))
        .route(
            "/test/pending",
            axum::routing::get({
                let entered = Arc::clone(&entered);
                let dropped = Arc::clone(&dropped);
                move || async move {
                    let _guard = SetOnDrop(dropped);
                    entered.notify_one();
                    std::future::pending::<()>().await
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let stop = CancellationToken::new();
        let shutdown = ApiShutdown::default();
        let server = serve_api(listener, router, stop.clone(), shutdown.clone());

        let mut stalled = tokio::net::TcpStream::connect(addr).await.expect("connect");
        stalled
            .write_all(
                b"POST /v1/units/query HTTP/1.1\r\nhost: probe\r\n\
                  content-type: application/json\r\ncontent-length: 100\r\n\r\n{\"type\"",
            )
            .await
            .expect("partial request");
        let mut pending = tokio::net::TcpStream::connect(addr).await.expect("connect");
        pending
            .write_all(b"GET /test/pending HTTP/1.1\r\nhost: probe\r\n\r\n")
            .await
            .expect("pending request");
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("the pending request reaches its handler");
        // Let the server read the stalled request's head.
        tokio::time::sleep(Duration::from_millis(200)).await;

        stop.cancel();
        let joined = drain(server, Duration::from_millis(300), &shutdown).await;
        assert!(
            matches!(joined, Ok(Some(Ok(())))),
            "the server ends once its requests are cut short: {joined:?}"
        );
        assert!(
            dropped.load(std::sync::atomic::Ordering::SeqCst),
            "the pending handler stopped"
        );
        for client in [&mut stalled, &mut pending] {
            let reply = until_closed(client).await;
            assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
            assert!(reply.contains("shutting down"), "{reply}");
        }
    }

    /// At shutdown, an HTTP/2 client that keeps its connection open with a
    /// zero window, so no reply body can reach it, still loses its
    /// connection, and the server ends on its own.
    #[tokio::test]
    async fn api_shutdown_closes_a_connection_that_never_reads() {
        use hyper_util::rt::{TokioExecutor, TokioIo};

        let cfg = Config::default();
        let router = api::router(api::ApiState::new(
            Arc::new(state::store::SnapshotStore::new(&cfg.state.paging)),
            &cfg.state,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let stop = CancellationToken::new();
        let shutdown = ApiShutdown::default();
        let server = serve_api(listener, router, stop.clone(), shutdown.clone());

        let tcp = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let (mut sender, connection) =
            hyper::client::conn::http2::Builder::new(TokioExecutor::new())
                .initial_stream_window_size(0)
                .handshake(TokioIo::new(tcp))
                .await
                .expect("HTTP/2 handshake");
        let connection = tokio::spawn(connection);
        // The reply's head arrives; its body waits for a window that never
        // opens.
        let reply = sender
            .send_request(
                axum::http::Request::get(format!("http://{addr}/v1/count?type=host&stage=ready"))
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("reply head");
        assert_eq!(reply.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);

        stop.cancel();
        let joined = drain(server, Duration::from_millis(300), &shutdown).await;
        assert!(
            matches!(joined, Ok(Some(Ok(())))),
            "the server ends once its connections close: {joined:?}"
        );
        assert!(shutdown.connections.is_cancelled());
        let _ = tokio::time::timeout(Duration::from_secs(5), connection)
            .await
            .expect("the client's connection ends")
            .expect("join");
        drop((sender, reply));
    }

    /// At shutdown, an HTTP/1 client that sent its request and never reads
    /// the reply, so the server blocks writing a body larger than the socket
    /// buffers, loses its connection within the drain bound, and the server
    /// ends on its own.
    #[tokio::test]
    async fn api_shutdown_closes_an_http1_connection_that_never_reads() {
        use tokio::io::AsyncWriteExt;

        let cfg = Config::default();
        let written = Arc::new(tokio::sync::Notify::new());
        let router = api::router(api::ApiState::new(
            Arc::new(state::store::SnapshotStore::new(&cfg.state.paging)),
            &cfg.state,
        ))
        .route(
            "/test/large",
            axum::routing::get({
                let written = Arc::clone(&written);
                move || async move {
                    written.notify_one();
                    vec![b'x'; 64 << 20]
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("address");
        let stop = CancellationToken::new();
        let shutdown = ApiShutdown::default();
        let server = serve_api(listener, router, stop.clone(), shutdown.clone());

        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(b"GET /test/large HTTP/1.1\r\nhost: probe\r\n\r\n")
            .await
            .expect("request");
        tokio::time::timeout(Duration::from_secs(5), written.notified())
            .await
            .expect("the request reaches its handler");
        // Let the server fill the socket buffers and block on the write.
        tokio::time::sleep(Duration::from_millis(200)).await;

        stop.cancel();
        let started = tokio::time::Instant::now();
        let joined = drain(server, Duration::from_millis(300), &shutdown).await;
        assert!(
            matches!(joined, Ok(Some(Ok(())))),
            "the server ends once its connections close: {joined:?}"
        );
        assert!(
            shutdown.connections.is_cancelled(),
            "only closing the connection's I/O ends the blocked write"
        );
        assert!(
            started.elapsed() < Duration::from_millis(300) + API_CLOSE * 2 + Duration::from_secs(1),
            "shutdown took {:?}",
            started.elapsed()
        );
        drop(client);
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
