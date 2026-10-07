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

mod command_line;
mod grpc_server;
use std::error::Error;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::Arc;

use ::rpc::forge_tls_client::ForgeClientConfig;
use carbide_dhcp_server::cache::{self, CacheEntry};
use carbide_dhcp_server::errors::DhcpError;
use carbide_dhcp_server::metrics::{
    DhcpPacketDropped, DhcpTimestampFileFailed, DhcpV6ListenerUnavailable, DhcpV6ReplySent,
    DhcpV6RequestDropped, DropReason, V6DropReason, record_v6_request_received,
};
use carbide_dhcp_server::modes::DhcpMode;
use carbide_dhcp_server::modes::controller::Controller;
use carbide_dhcp_server::modes::dpu::{Dpu, get_host_config};
use carbide_dhcp_server::{Config, packet_handler, packet_handler_v6, util};
use carbide_instrument::emit;
use carbide_rpc_utils::dhcp::{DhcpConfig, DhcpTimestamps, DhcpTimestampsFilePath};
use chrono::Utc;
use command_line::{Args, ServerMode};
use forge_tls::client_config::ClientCert;
use forge_tls::default::{default_client_cert, default_client_key, default_root_ca};
use grpc_server::{ControlRequest, run_grpc_server};
use lru::LruCache;
use metrics_endpoint::{MetricsEndpointConfig, new_metrics_setup, run_metrics_endpoint};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;
use util::{get_socket, get_socket_v6};

struct Server {
    socket: Arc<UdpSocket>,
}

/// Values shared by packets received on one DHCPv6 listener.
#[derive(Clone)]
struct V6ListenerContext {
    socket: Arc<UdpSocket>,
    config: Arc<Config>,
    handler: Arc<Box<dyn DhcpMode>>,
    interface: String,
    machine_cache: Arc<Mutex<LruCache<String, CacheEntry>>>,
    dhcp_timestamps: Arc<Mutex<DhcpTimestamps>>,
}

const MAX_PARALLEL_PACKET_HANDLING_ALLOWED: usize = 128;

/// Records why a DHCPv4 listener violated the generation-lifetime invariant.
#[derive(Debug)]
enum V4ListenerFailure {
    /// The listener returned before its generation was cancelled.
    Returned,
    /// The listener task panicked or was otherwise cancelled unexpectedly.
    Join(tokio::task::JoinError),
}

impl std::fmt::Display for V4ListenerFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Returned => {
                formatter.write_str("listener returned before generation cancellation")
            }
            Self::Join(error) => write!(formatter, "listener task failed: {error}"),
        }
    }
}

/// Run one generation of the DHCP server (all interfaces) until `cancel_token` is cancelled.
///
/// Each interface gets its own tokio task.  Inside every task the packet-receive
/// loop uses `tokio::select!` to watch both the UDP socket and the cancellation
/// token, so shutdown is prompt once `cancel_token.cancel()` is called from main.
async fn run_dhcp_server(args: Args, cancel_token: CancellationToken) {
    let config__ = match init(args.clone()).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "Failed to initialise DHCP server config");
            return;
        }
    };

    let dhcp_timestamps = Arc::new(Mutex::new({
        let dhcp_timestamps_path = if let ServerMode::Dpu = args.mode {
            DhcpTimestampsFilePath::HbnTmp
        } else {
            DhcpTimestampsFilePath::NotSet
        };
        let dhcp_timestamps_path_context = dhcp_timestamps_path.path_str().to_string();
        let d = DhcpTimestamps::new(dhcp_timestamps_path);

        // It looks like we can only expect the file to be present
        // if something has successfully DHCP'ed, after write() has been
        // called at least once.  That means there's a possible window of time
        // where the file might be _expected_ to not exist, but read() will complain
        // and pollute the logs. We could have read() skip NotFound errors, but that
        // could be misleading in other scenarios.  Let's just "init" the file.
        if let Err(e) = d.write() {
            emit(DhcpTimestampFileFailed::Initialization {
                dhcp_timestamps_path: dhcp_timestamps_path_context,
                error: e.to_string(),
            });
            return;
        }
        d
    }));

    // Each family has an independent packet-processing limit across all interfaces.
    let rate_limiter_ = Arc::new(tokio::sync::Semaphore::new(
        MAX_PARALLEL_PACKET_HANDLING_ALLOWED,
    ));
    let v6_rate_limiter_ = Arc::new(tokio::sync::Semaphore::new(
        MAX_PARALLEL_PACKET_HANDLING_ALLOWED,
    ));

    let mut v4_tasks = JoinSet::new();
    let mut v6_tasks = JoinSet::new();

    // Create a new socket for each interface.
    // In case of Controller, there will be only 1 interface.
    for interface in args.interfaces {
        let v6_interface = interface.clone();
        let v6_config = config__.clone();
        let v6_mode = args.mode.clone();
        let v6_timestamps = dhcp_timestamps.clone();
        let v6_rate_limiter = v6_rate_limiter_.clone();
        let v6_cancel = cancel_token.clone();
        let config_ = config__.clone();
        let args_mode = args.mode.clone();
        let listen_address = args.listen_addr;
        let dhcp_timestamps_ = dhcp_timestamps.clone();
        let rate_limiter = rate_limiter_.clone();
        let cancel = cancel_token.clone();

        v4_tasks.spawn(async move {
            let handler: Arc<Box<dyn DhcpMode>> = Arc::new(get_mode(&args_mode));

            let socket = get_socket(listen_address, interface.clone()).await;
            tracing::info!(
                %listen_address,
                interface_name = interface.as_str(),
                mode = ?handler,
                "DHCP server listening"
            );

            let mut server = Server {
                socket: Arc::new(socket),
            };

            // Machine cache is used only in Controller mode and Controller listens only on one
            // interface, so it is ok to initialize cache here.
            let machine_cache_ = Arc::new(Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
            )));

            // Listen on each interface and process it.
            // The select! monitors both the UDP socket and the cancellation token so that
            // the loop exits promptly when a config reload is triggered from the gRPC server.
            loop {
                let mut buf = [0; 1500];
                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!(
                            interface_name = interface.as_str(),
                            "DHCP server received cancellation, shutting down"
                        );
                        break;
                    }
                    result = server.socket.recv_from(&mut buf) => {
                        let (len, addr) = match result {
                            Ok((len, addr)) => (len, addr),
                            Err(err) => {
                                // We don't know after this read is failed, will we be able to read again
                                // from this socket? Mostly no. In this case, recreate the socket.
                                // We observed this fluctuation during admin to tenant network switch.
                                tracing::error!(
                                    %listen_address,
                                    interface_name = interface.as_str(),
                                    error = %err,
                                    "Socket receive failed"
                                );
                                // Try to close the existing socket.
                                drop(server.socket);
                                tracing::info!(
                                    %listen_address,
                                    interface_name = interface.as_str(),
                                    "Recreating the socket"
                                );
                                server.socket =
                                    Arc::new(get_socket(listen_address, interface.clone()).await);
                                continue;
                            }
                        };

                        // We never close this semaphore, so if an error is returned it should be
                        // TryAcquireError::NoPermits; Not checking explicitly.
                        let Ok(permit) = rate_limiter.clone().try_acquire_owned() else {
                            // drop packet.
                            emit(DhcpPacketDropped {
                                reason: DropReason::RateLimited,
                                error: "parallel packet handling limit reached".to_string(),
                            });
                            continue;
                        };

                        // Not a valid packet.
                        if len < MINIMUM_DHCP_PKT_SIZE {
                            emit(DhcpPacketDropped {
                                reason: DropReason::TooShort,
                                error: format!(
                                    "{len} bytes is below the {MINIMUM_DHCP_PKT_SIZE}-byte minimum"
                                ),
                            });
                            continue;
                        }

                        let config = config_.clone();
                        let mut machine_cache = machine_cache_.clone();
                        let iface = interface.clone();
                        let handler_ = handler.clone();
                        let dhcp_timestamps = dhcp_timestamps_.clone();
                        let socket = server.socket.clone();

                        tokio::spawn(async move {
                            process(
                                addr,
                                socket,
                                &buf,
                                config.clone(),
                                &**handler_,
                                &iface,
                                &mut machine_cache,
                                dhcp_timestamps,
                            )
                            .await;
                            drop(permit);
                        });
                    }
                }
            }
        });

        // Milestone 04 admits DHCPv6 in both modes; socket setup failure remains
        // nonfatal so an unavailable v6 stack cannot take down DHCPv4.
        v6_tasks.spawn(run_dhcp_v6_listener(
            v6_interface,
            v6_config,
            v6_mode,
            v6_cancel,
            v6_rate_limiter,
            v6_timestamps,
        ));
    }

    // Preserve optional IPv6 availability without hiding a failed IPv4 listener.
    if let Err(error) = supervise_listener_tasks(v4_tasks, v6_tasks, cancel_token).await {
        tracing::error!(
            error = %error,
            "DHCPv4 listener exited unexpectedly"
        );
    }
}

/// Supervises a generation while preserving the listeners' family-specific semantics.
///
/// A v4 listener must run until generation cancellation, so every earlier completion
/// is reported and losing the last v4 listener fails the generation. A normally
/// returning v6 task represents expected optional listener unavailability and does
/// not stop healthy v4 service.
async fn supervise_listener_tasks(
    mut v4_tasks: JoinSet<()>,
    mut v6_tasks: JoinSet<()>,
    cancel_token: CancellationToken,
) -> Result<(), V4ListenerFailure> {
    if v4_tasks.is_empty() {
        return Ok(());
    }

    let failure = loop {
        tokio::select! {
            biased;

            _ = cancel_token.cancelled() => break None,
            Some(result) = v4_tasks.join_next(), if !v4_tasks.is_empty() => {
                // Cancellation is intentionally clean even when a listener exits concurrently.
                if cancel_token.is_cancelled() {
                    break None;
                }

                let failure = match result {
                    Ok(()) => V4ListenerFailure::Returned,
                    Err(error) => V4ListenerFailure::Join(error),
                };
                if !v4_tasks.is_empty() {
                    tracing::error!(
                        error = %failure,
                        remaining_v4_listener_count = v4_tasks.len(),
                        "DHCPv4 listener exited unexpectedly"
                    );
                    continue;
                }

                cancel_token.cancel();
                v4_tasks.abort_all();
                v6_tasks.abort_all();
                break Some(failure);
            }
            Some(result) = v6_tasks.join_next(), if !v6_tasks.is_empty() => {
                if let Err(error) = result {
                    tracing::warn!(
                        error = %error,
                        "DHCPv6 listener exited unexpectedly"
                    );
                }
            }
        }
    };

    // Join every listener task. TODO(dhcp-reload): DHCPv4 and DHCPv6 packet tasks
    // remain detached, so they can retain old sockets and config and send stale
    // replies after reload.
    while v4_tasks.join_next().await.is_some() {}
    while v6_tasks.join_next().await.is_some() {}

    match failure {
        Some(failure) => Err(failure),
        None => Ok(()),
    }
}

/// Count one DHCPv6 datagram at ingress and apply the handler admission limit.
fn admit_v6_packet(
    packet: &[u8],
    source_address: SocketAddr,
    rate_limiter: &Arc<tokio::sync::Semaphore>,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    record_v6_request_received(packet, source_address);
    match rate_limiter.clone().try_acquire_owned() {
        Ok(permit) => Some(permit),
        Err(_) => {
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::RateLimited,
                error: "parallel packet handling limit reached".to_string(),
            });
            None
        }
    }
}

/// Enforce the UDP source ports assigned to DHCPv6 clients and relay agents.
fn validate_v6_source_port(packet: &[u8], source: &SocketAddrV6) -> Result<(), DhcpError> {
    let (sender, expected_port) = if packet.first().copied() == Some(carbide_dhcpv6::RELAY_FORWARD)
    {
        ("relay", dhcproto::v6::SERVER_PORT)
    } else {
        ("client", dhcproto::v6::CLIENT_PORT)
    };
    if source.port() != expected_port {
        return Err(DhcpError::UnexpectedDhcpV6SourcePort {
            sender,
            actual: source.port(),
            expected: expected_port,
        });
    }
    Ok(())
}

/// Run the independent DHCPv6 receive loop for one configured interface.
async fn run_dhcp_v6_listener(
    interface: String,
    config: Config,
    mode: ServerMode,
    cancel: CancellationToken,
    rate_limiter: Arc<tokio::sync::Semaphore>,
    dhcp_timestamps: Arc<Mutex<DhcpTimestamps>>,
) {
    let handler: Arc<Box<dyn DhcpMode>> = Arc::new(get_mode(&mode));
    // Controller mode accepts relay traffic; DPU mode retains its direct-only
    // trust boundary and therefore does not subscribe to relay discovery.
    let join_site_scoped_group = matches!(mode, ServerMode::Controller);
    let listen_address = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, dhcproto::v6::SERVER_PORT, 0, 0);

    // Socket retries remain interruptible when a server generation is cancelled.
    let socket_result = tokio::select! {
        _ = cancel.cancelled() => return,
        result = get_socket_v6(listen_address, &interface, join_site_scoped_group) => result,
    };
    let socket = match socket_result {
        Ok(socket) => Arc::new(socket),
        Err(error) => {
            // IPv4-only hosts are valid, so failure to establish the sibling
            // IPv6 listener must not take down the existing DHCPv4 service.
            emit(DhcpV6ListenerUnavailable::InitialSocketSetup {
                interface_name: interface,
                error: error.to_string(),
            });
            return;
        }
    };
    tracing::info!(
        %listen_address,
        interface_name = interface,
        mode = ?handler,
        "DHCPv6 server listening"
    );

    let Some(cache_size) = std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE) else {
        tracing::error!("DHCP machine cache size must be nonzero");
        return;
    };
    let machine_cache = Arc::new(Mutex::new(LruCache::new(cache_size)));
    let mut context = V6ListenerContext {
        socket,
        config: Arc::new(config),
        handler,
        interface,
        machine_cache,
        dhcp_timestamps,
    };

    // DHCPv6 has a four-byte base header, so it intentionally does not use
    // the DHCPv4 path's 236-byte BOOTP minimum. Keep one full-size UDP buffer
    // per listener so relay options cannot be silently truncated at Ethernet MTU.
    let mut buffer = vec![0; usize::from(u16::MAX)];
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::info!(
                    interface_name = context.interface,
                    "DHCPv6 server received cancellation, shutting down"
                );
                break;
            }
            result = context.socket.recv_from(&mut buffer) => {
                let (length, source) = match result {
                    Ok(received) => received,
                    Err(error) => {
                        tracing::error!(
                            interface_name = context.interface,
                            error = %error,
                            "DHCPv6 socket receive failed"
                        );
                        let recreated = tokio::select! {
                            _ = cancel.cancelled() => return,
                            result = get_socket_v6(
                                listen_address,
                                &context.interface,
                                join_site_scoped_group,
                            ) => result,
                        };
                        match recreated {
                            Ok(recreated) => context.socket = Arc::new(recreated),
                            Err(error) => {
                                emit(DhcpV6ListenerUnavailable::SocketRecreation {
                                    interface_name: context.interface.clone(),
                                    error: error.to_string(),
                                });
                                return;
                            }
                        }
                        continue;
                    }
                };

                let Some(permit) = admit_v6_packet(&buffer[..length], source, &rate_limiter) else {
                    continue;
                };

                let packet = buffer[..length].to_vec();
                let packet_context = context.clone();
                tokio::spawn(async move {
                    process_v6(source, &packet, packet_context).await;
                    drop(permit);
                });
            }
        }
    }
}

/// Initialises the tracing subscriber with per-crate log-level overrides.
fn setup_tracing() -> Result<(), Box<dyn Error>> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("tower=warn".parse().unwrap())
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("tokio_util::codec=warn".parse().unwrap())
        .add_directive("h2=warn".parse().unwrap())
        .add_directive("hickory_resolver::error=info".parse().unwrap())
        .add_directive("hickory_proto::xfer=info".parse().unwrap())
        .add_directive("hickory_resolver::name_server=info".parse().unwrap())
        .add_directive("hickory_proto=info".parse().unwrap());

    // Counts every log line into carbide_log_events_total from startup; the
    // counts are exposed once main() installs the meter provider. The env
    // filter sits on the registry as a global filter so the counting layer
    // and the logfmt output see exactly the same events.
    let log_events = carbide_instrument::LogEventsMetric::new("nico-dhcp");
    tracing_subscriber::registry()
        .with(log_events.layer())
        .with(
            logfmt::layer()
                .with_event_fields([logfmt::EventField::with_default("component", "nico-dhcp")]),
        )
        .with(env_filter)
        .try_init()?;
    Ok(())
}

/// Stages updated DHCP config YAML for an immediate reload.
///
/// Reads the current live config files and writes `_new` versions only when
/// the content actually differs, so the subsequent reload can detect whether
/// a restart is needed.
async fn handle_update_config(
    args: &Args,
    dhcp_yaml: String,
    host_yaml: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let new_dhcp = format!("{}_new", args.dhcp_config);
    let current_dhcp = tokio::fs::read_to_string(&args.dhcp_config)
        .await
        .unwrap_or_default();
    if current_dhcp != dhcp_yaml {
        tokio::fs::write(&new_dhcp, &dhcp_yaml)
            .await
            .map_err(|e| -> Box<dyn Error> { format!("write {new_dhcp}: {e}").into() })?;
        tracing::info!(path = new_dhcp.as_str(), "dhcp_config changed – staged");
    }

    if let (Some(yaml), Some(path)) = (host_yaml, &args.host_config) {
        let new_host = format!("{}_new", path);
        let current_host = tokio::fs::read_to_string(path).await.unwrap_or_default();
        if current_host != yaml {
            tokio::fs::write(&new_host, &yaml)
                .await
                .map_err(|e| -> Box<dyn Error> { format!("write {new_host}: {e}").into() })?;
            tracing::info!(path = new_host.as_str(), "host_config changed – staged");
        }
    }
    Ok(())
}

/// Promotes staged config files and (re)starts the DHCP server.
///
/// If no `_new` files exist and `force_start` is false the restart is skipped.
/// When `force_start` is true (e.g. after an explicit `StopServer`) the server
/// is started even if the config on disk has not changed.  Otherwise any running
/// server generation is cancelled, the `_new` files are renamed to their live
/// paths, and a fresh server generation is spawned.
async fn handle_reload(
    args: &Args,
    cancel_token: Option<CancellationToken>,
    dhcp_handle: Option<tokio::task::JoinHandle<()>>,
    force_start: bool,
) -> Result<
    (
        Option<CancellationToken>,
        Option<tokio::task::JoinHandle<()>>,
    ),
    Box<dyn Error>,
> {
    if args.interfaces.is_empty() {
        tracing::warn!("ReloadConfig: no interfaces configured yet, skipping start");
        return Ok((cancel_token, dhcp_handle));
    }

    let new_dhcp = format!("{}_new", args.dhcp_config);
    let has_new_dhcp = tokio::fs::try_exists(&new_dhcp).await.unwrap_or(false);
    let has_new_host = if let Some(host_path) = &args.host_config {
        tokio::fs::try_exists(format!("{}_new", host_path))
            .await
            .unwrap_or(false)
    } else {
        false
    };

    if !has_new_dhcp && !has_new_host && !force_start {
        tracing::debug!("ReloadConfig: no staged changes, skipping restart");
        return Ok((cancel_token, dhcp_handle));
    }

    // Stop any running server generation.
    if let (Some(ct), Some(h)) = (cancel_token, dhcp_handle) {
        tracing::info!("Stopping current DHCP server");
        ct.cancel();
        let _ = h.await;
        tracing::info!("DHCP server stopped");
    }

    // Atomically replace live config files.
    if has_new_dhcp {
        tokio::fs::rename(&new_dhcp, &args.dhcp_config)
            .await
            .map_err(|e| -> Box<dyn Error> {
                format!("rename {} -> {}: {e}", new_dhcp, args.dhcp_config).into()
            })?;
    }
    if let Some(host_path) = &args.host_config {
        let new_host = format!("{}_new", host_path);
        let exists = tokio::fs::try_exists(&new_host)
            .await
            .map_err(|e| -> Box<dyn Error> { format!("try_exists {new_host}: {e}").into() })?;
        if exists {
            tokio::fs::rename(&new_host, host_path)
                .await
                .map_err(|e| -> Box<dyn Error> {
                    format!("rename {new_host} -> {host_path}: {e}").into()
                })?;
        }
    }

    // Start new server generation.
    let ct = CancellationToken::new();
    let handle = tokio::spawn(run_dhcp_server(args.clone(), ct.clone()));
    tracing::info!("DHCP server (re)started with updated config");
    Ok((Some(ct), Some(handle)))
}

/// Runs the DHCP server under gRPC control.
///
/// Spawns the gRPC server as a background task, then enters the main control
/// loop.  The DHCP server is started immediately when the config file already
/// exists on disk; otherwise the first `ReloadConfig` call triggers the
/// initial start, avoiding a startup crash on a fresh node.
async fn run_with_grpc_control(
    mut args: Args,
    grpc_listen_addr: SocketAddr,
) -> Result<(), Box<dyn Error>> {
    // Apply default for host_config path when running in gRPC mode.
    args.host_config
        .get_or_insert_with(|| "/var/support/forge-dhcp/conf/host.yaml".to_string());

    // Ensure the config directory exists so that the first gRPC UpdateConfig call
    // can write files immediately without the directory being absent.
    if let Some(dir) = std::path::Path::new(&args.dhcp_config).parent()
        && !tokio::fs::try_exists(dir).await.unwrap_or(false)
    {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| -> Box<dyn Error> {
                format!("create_dir_all {}: {e}", dir.display()).into()
            })?;
        tracing::info!(path = %dir.display(), "Created config directory");
    }

    // Channel through which the gRPC handlers deliver control requests.
    // Capacity 4: allows a few queued UpdateConfig calls without blocking the gRPC caller.
    let (ctrl_tx, mut ctrl_rx) = tokio::sync::mpsc::channel::<ControlRequest>(4);

    tokio::spawn(async move {
        run_grpc_server(grpc_listen_addr, ctrl_tx).await;
    });

    // Both `cancel_token` and `dhcp_handle` are Option so the select! arm
    // that watches the handle pends forever while the server is not yet running.
    let mut cancel_token: Option<CancellationToken> = None;
    let mut dhcp_handle: Option<tokio::task::JoinHandle<()>> = None;

    if tokio::fs::try_exists(&args.dhcp_config)
        .await
        .unwrap_or(false)
        && !args.interfaces.is_empty()
    {
        tracing::info!("Config file and interfaces found at startup – starting DHCP server");
        let ct = CancellationToken::new();
        dhcp_handle = Some(tokio::spawn(run_dhcp_server(args.clone(), ct.clone())));
        cancel_token = Some(ct);
    } else {
        tracing::info!(
            "Config file or interfaces not ready at startup – \
             DHCP server will start after first ReloadConfig"
        );
    }

    loop {
        tokio::select! {
            // This arm pends forever while dhcp_handle is None, waiting for
            // gRPC messages until the first reload.
            result = async {
                match dhcp_handle.as_mut() {
                    Some(h) => h.await,
                    None => std::future::pending().await,
                }
            } => {
                match result {
                    Ok(()) => tracing::error!("DHCP server exited unexpectedly"),
                    Err(error) => tracing::error!(
                        error = ?error,
                        "DHCP server exited unexpectedly"
                    ),
                }
                return Ok(());
            }

            msg = ctrl_rx.recv() => {
                let Some(msg) = msg else {
                    tracing::error!("Control channel closed unexpectedly; terminating");
                    if let (Some(ct), Some(h)) = (cancel_token.take(), dhcp_handle.take()) {
                        ct.cancel();
                        let _ = h.await;
                    }
                    return Ok(());
                };

                match msg {
                    ControlRequest::UpdateAndReload { dhcp_yaml, host_yaml, interfaces } => {
                        args.interfaces = interfaces;
                        handle_update_config(&args, dhcp_yaml, host_yaml).await?;
                        // Force a start when the server is not currently running
                        // (stopped explicitly or never started) so that the server
                        // is (re)started even if the config on disk is unchanged.
                        let force = dhcp_handle.is_none();
                        let (ct, h) =
                            handle_reload(&args, cancel_token, dhcp_handle, force).await?;
                        cancel_token = ct;
                        dhcp_handle = h;
                    }
                    ControlRequest::Stop => {
                        if let (Some(ct), Some(h)) = (cancel_token.take(), dhcp_handle.take()) {
                            tracing::info!("StopServer: stopping DHCP server");
                            ct.cancel();
                            let _ = h.await;
                            tracing::info!("StopServer: DHCP server stopped; gRPC server remains up");
                        } else {
                            tracing::info!("StopServer: DHCP server was not running");
                        }
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    setup_tracing()?;

    let args = Args::load();

    // In gRPC mode the interfaces may be provided later via UpdateConfig, so
    // only validate the count when interfaces are already known at startup.
    if let ServerMode::Controller = args.mode
        && !args.interfaces.is_empty()
        && args.interfaces.len() != 1
    {
        return Err(
            DhcpError::MultipleInterfacesProvidedOneSupported(args.interfaces.len()).into(),
        );
    }

    // Install the global meter provider before the first packet is processed
    // so every emitted event exports, whether or not the scrape endpoint is
    // served below.
    let metrics_setup = new_metrics_setup("carbide-dhcp-server", "forge-system", true)
        .map_err(|e| format!("Failed to set up metrics: {e}"))?;
    carbide_instrument::log_events::register(&metrics_setup.meter);

    // Must keep meter_provider alive for the lifetime of the server;
    // dropping it shuts down the Prometheus exporter.
    let _metrics_guard = metrics_setup.meter_provider;

    if let Some(ref addr_str) = args.metrics_listen_addr {
        let metrics_listen_addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| format!("Invalid --metrics-listen-addr '{}': {}", addr_str, e))?;
        let metrics_config = MetricsEndpointConfig {
            address: metrics_listen_addr,
            registry: metrics_setup.registry,
            health_controller: Some(metrics_setup.health_controller),
            additional_prefix: None,
        };
        // The endpoint's /health and /ready report process liveness (the
        // default HealthController state), not packet-serving readiness --
        // don't point a DHCP-serving probe at them.
        tokio::spawn(async move {
            tracing::info!(metrics_address = %metrics_config.address, "Spawning metrics endpoint");
            if let Err(e) = run_metrics_endpoint(&metrics_config).await {
                tracing::error!(error = %e, "Metrics endpoint error");
            }
        });
    }

    if let Some(ref addr_str) = args.grpc_listen_addr {
        let grpc_listen_addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| format!("Invalid --grpc-listen-addr '{}': {}", addr_str, e))?;
        run_with_grpc_control(args, grpc_listen_addr).await?;
    } else {
        // No gRPC server: run the DHCP server directly.  The CancellationToken
        // is wired up inside run_dhcp_server but is never triggered, so
        // behaviour is identical to the original server.
        run_dhcp_server(args, CancellationToken::new()).await;
    }

    Ok(())
}

fn get_mode(args_mode: &ServerMode) -> Box<dyn DhcpMode> {
    match args_mode {
        ServerMode::Dpu => Box::new(Dpu {}),
        ServerMode::Controller => Box::new(Controller {}),
    }
}

async fn init(args: Args) -> Result<Config, DhcpError> {
    let forge_client_config = forge_client_config(&args)?;
    let f = tokio::fs::read_to_string(args.dhcp_config).await?;
    let dhcp_config: DhcpConfig = serde_yaml::from_str(&f)?;

    let host_config;
    if let ServerMode::Dpu = args.mode {
        host_config = get_host_config(args.host_config).await?;
    } else {
        host_config = None;
    };

    Ok(Config::new(
        dhcp_config,
        host_config,
        args.relay_response_port,
        forge_client_config,
    ))
}

fn forge_client_config(args: &Args) -> Result<ForgeClientConfig, DhcpError> {
    let root_ca_path = args
        .forge_root_ca_path
        .clone()
        .unwrap_or_else(|| default_root_ca().to_string());
    let client_cert = match (&args.client_cert_path, &args.client_key_path) {
        (Some(cert_path), Some(key_path)) => ClientCert {
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
        },
        (None, None) => ClientCert {
            cert_path: default_client_cert().to_string(),
            key_path: default_client_key().to_string(),
        },
        _ => {
            return Err(DhcpError::MissingArgument(
                "client_cert_path and client_key_path must be configured together".to_string(),
            ));
        }
    };

    Ok(ForgeClientConfig::new(root_ca_path, Some(client_cert)))
}

const MINIMUM_DHCP_PKT_SIZE: usize = 236;

#[tracing::instrument(skip_all)]
#[allow(clippy::too_many_arguments)]
async fn process(
    addr: SocketAddr,
    socket: Arc<UdpSocket>,
    buf: &[u8],
    config: Config,
    handler: &dyn DhcpMode,
    circuit_id: &str, // interface name
    machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
    dhcp_timestamps: Arc<Mutex<DhcpTimestamps>>,
) {
    if !addr.is_ipv4() {
        emit(DhcpPacketDropped {
            reason: DropReason::NotIpv4,
            error: format!("source address {addr} is not IPv4"),
        });
        return;
    }

    let Some(&bootp_op) = buf.first() else {
        emit(DhcpPacketDropped {
            reason: DropReason::TooShort,
            error: format!("0 bytes is below the {MINIMUM_DHCP_PKT_SIZE}-byte minimum"),
        });
        return;
    };

    // Keep raw source/opcode visibility when validation or decoding fails
    // before the structured request Event can be emitted.
    tracing::debug!(bootp_op, source_address = %addr, "Received DHCP packet");

    let packet = match packet_handler::process_packet(
        buf,
        addr,
        &config,
        circuit_id,
        handler,
        machine_cache,
    )
    .await
    {
        Ok(packet) => packet,
        Err(err) => {
            emit(DhcpPacketDropped {
                reason: DropReason::from(&err),
                error: err.to_string(),
            });
            return;
        }
    };

    let dest_address = handler.get_destination_address(&packet);
    if let Err(err) = packet.send(dest_address, socket).await {
        emit(DhcpPacketDropped {
            reason: DropReason::SendFailed,
            error: err,
        });
    }

    record_dhcp_timestamp(&config, dhcp_timestamps).await;
}

/// Process one DHCPv6 datagram and send its response to the exact UDP source.
#[tracing::instrument(skip_all)]
async fn process_v6(source: SocketAddr, packet: &[u8], mut context: V6ListenerContext) {
    let SocketAddr::V6(source) = source else {
        let error = format!("source address {source} is not IPv6");
        emit(DhcpV6RequestDropped {
            reason: V6DropReason::InvalidPacket,
            error,
        });
        return;
    };
    if let Err(error) = validate_v6_source_port(packet, &source) {
        emit(DhcpV6RequestDropped {
            reason: V6DropReason::from(&error),
            error: error.to_string(),
        });
        return;
    }

    tracing::debug!(source_address = %source, "Received DHCPv6 packet");
    let response = match packet_handler_v6::process_packet(
        packet,
        *source.ip(),
        &context.config,
        &context.interface,
        &**context.handler,
        &mut context.machine_cache,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::from(&error),
                error: error.to_string(),
            });
            return;
        }
    };

    // An indeterminate CONFIRM is intentionally discarded without counting
    // it as an invalid or dropped request.
    let Some(response) = response else {
        return;
    };

    tracing::debug!(destination_address = %source, "Sending DHCPv6 packet");
    match context
        .socket
        .send_to(response.encoded_packet(), source)
        .await
    {
        Ok(_) => emit(DhcpV6ReplySent {
            message_type: response.message_type,
            destination_address: source,
        }),
        Err(error) => emit(DhcpV6RequestDropped {
            reason: V6DropReason::SendFailed,
            error: error.to_string(),
        }),
    }
    record_dhcp_timestamp(&context.config, context.dhcp_timestamps.clone()).await;
}

/// Record that the DPU-side interface has served a DHCP request.
async fn record_dhcp_timestamp(config: &Config, dhcp_timestamps: Arc<Mutex<DhcpTimestamps>>) {
    let Some(host_config) = config.host_config() else {
        return;
    };

    let mut dhcp_timestamps = dhcp_timestamps.lock().await;
    dhcp_timestamps.add_timestamp(host_config.host_interface_id, Utc::now().to_rfc3339());
    if let Err(error) = dhcp_timestamps.write() {
        emit(DhcpTimestampFileFailed::Write {
            dhcp_timestamps_path: DhcpTimestampsFilePath::HbnTmp.path_str().to_string(),
            host_interface_id: host_config.host_interface_id.to_string(),
            error: error.to_string(),
        });
    }
}

#[cfg(test)]
mod test {
    use std::env;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;

    use carbide_dhcp_server::errors::DhcpError;
    use carbide_dhcp_server::modes::V6Outcome;
    use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
    use carbide_rpc_utils::dhcp::{DhcpTimestamps, DhcpTimestampsFilePath};
    use carbide_test_support::value_scenarios;
    use chrono::{DateTime, Utc};
    use dhcproto::v4::{DhcpOption, Message, MessageType, OptionCode};
    use dhcproto::v6::MessageType as MessageTypeV6;
    use dhcproto::{Decodable, Decoder, Encodable};
    use lru::LruCache;
    use rpc::forge::{DhcpDiscovery, DhcpRecord};
    use tempfile::TempDir;
    use tokio::net::UdpSocket;
    use tokio::sync::{Mutex, oneshot};
    use tokio::task::JoinSet;
    use tokio::time::{Duration, timeout};
    use tokio_util::sync::CancellationToken;
    use tonic::async_trait;

    use crate::cache::CacheEntry;
    use crate::command_line::{Args, ServerMode};
    use crate::{
        Config, DhcpMode, V4ListenerFailure, admit_v6_packet, cache, forge_client_config,
        handle_reload, init, packet_handler, process, supervise_listener_tasks,
        validate_v6_source_port,
    };

    const TEST_SOURCE_ADDRESS: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 68));
    const TEST_CLIENT_MAC: &[u8] = &[0x00, 0x1b, 0x63, 0x84, 0x45, 0xe6];
    const TEST_CLIENT_MAC_TEXT: &str = "00:1b:63:84:45:e6";

    #[derive(Debug)]
    struct TestArm {}

    #[async_trait]
    impl DhcpMode for TestArm {
        async fn discover_dhcp(
            &self,
            _discovery_request: DhcpDiscovery,
            _config: &Config,
            _machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
        ) -> Result<DhcpRecord, DhcpError> {
            Test::dhcp_record()
        }

        /// Return a deterministic relayed DHCPv6 record for binary-level tests.
        async fn discover_dhcp_v6(
            &self,
            _discovery_request: DhcpDiscovery,
            _config: &Config,
            _machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
        ) -> Result<V6Outcome, DhcpError> {
            Ok(V6Outcome::Stateful(Test::dhcp_record_v6()?))
        }

        // Packets received from DPU to API must be relayed.
        fn should_be_relayed(&self) -> bool {
            true
        }
    }

    #[derive(Debug)]
    struct Test {}

    impl Test {
        /// Return the deterministic DHCPv4 record used by packet-processing tests.
        fn dhcp_record() -> Result<DhcpRecord, DhcpError> {
            Ok(DhcpRecord {
                machine_id: Some(
                    "fm100dsbiu5ckus880v8407u0mkcensa39cule26im5gnpvmuufckacguc0"
                        .parse()
                        .unwrap(),
                ),
                machine_interface_id: Some("0fd6e9a3-06fc-4a22-ad29-aca299677b00".parse().unwrap()),
                segment_id: Some("55a2d74e-f9e1-49d5-bf99-be05171a5d75".parse().unwrap()),
                subdomain_id: Some("56a2d74e-f9e1-49d5-bf99-be05171a5d75".parse().unwrap()),
                fqdn: "seventeen-connecticut.dev3.frg.nvidia.com".to_string(),
                mac_address: "b8:3f:d2:90:9a:12".to_string(),
                address: "10.217.132.204".to_string(),
                mtu: 6000,
                prefix: "10.217.132.192/26".to_string(),
                gateway: Some("10.217.132.193".to_string()),
                booturl: None,
                last_invalidation_time: None,
                ntp_servers: vec!["1.2.3.4".to_string(), "5.6.7.8".to_string()],
            })
        }

        /// Return the deterministic DHCPv6 record used by packet-processing tests.
        fn dhcp_record_v6() -> Result<DhcpRecord, DhcpError> {
            Ok(DhcpRecord {
                address: "2001:db8::204".to_string(),
                prefix: "2001:db8::/64".to_string(),
                gateway: None,
                ntp_servers: vec!["2001:db8::123".to_string()],
                ..Self::dhcp_record()?
            })
        }
    }

    #[async_trait]
    impl DhcpMode for Test {
        async fn discover_dhcp(
            &self,
            _discovery_request: DhcpDiscovery,
            _config: &Config,
            _machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
        ) -> Result<DhcpRecord, DhcpError> {
            Test::dhcp_record()
        }

        /// Return a deterministic direct DHCPv6 record for binary-level tests.
        async fn discover_dhcp_v6(
            &self,
            _discovery_request: DhcpDiscovery,
            _config: &Config,
            _machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
        ) -> Result<V6Outcome, DhcpError> {
            Ok(V6Outcome::Stateful(Test::dhcp_record_v6()?))
        }

        fn should_be_relayed(&self) -> bool {
            false
        }
    }

    fn make_reload_args(td: &TempDir, interfaces: Vec<String>) -> Args {
        Args {
            interfaces,
            listen_addr: "0.0.0.0:67".parse().unwrap(),
            relay_response_port: 67,
            dhcp_config: td.path().join("dhcp.yaml").display().to_string(),
            host_config: Some(td.path().join("host.yaml").display().to_string()),
            forge_root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            mode: ServerMode::Dpu,
            grpc_listen_addr: None,
            metrics_listen_addr: None,
        }
    }

    /// Verifies direct clients and relay agents use their assigned DHCPv6 source ports.
    #[test]
    fn validates_dhcpv6_source_ports() {
        value_scenarios!(run = |(packet_type, source_port): (u8, u16)| {
                let source = SocketAddrV6::new(Ipv6Addr::LOCALHOST, source_port, 0, 0);
                validate_v6_source_port(&[packet_type], &source).is_ok()
            };
            "assigned source ports" {
                // A direct client sends from the DHCPv6 client port.
                (u8::from(MessageTypeV6::Solicit), dhcproto::v6::CLIENT_PORT) => true,
                // A relay agent sends Relay-Forward from the DHCPv6 server port.
                (carbide_dhcpv6::RELAY_FORWARD, dhcproto::v6::SERVER_PORT) => true,
            }
            "crossed source ports" {
                // Direct client traffic on the relay/server port is rejected.
                (u8::from(MessageTypeV6::Solicit), dhcproto::v6::SERVER_PORT) => false,
                // Relay traffic on the client port is rejected.
                (carbide_dhcpv6::RELAY_FORWARD, dhcproto::v6::CLIENT_PORT) => false,
            }
        );
    }

    /// Verifies both forms of last-listener v4 completion fail the generation.
    #[tokio::test]
    async fn listener_supervision_fails_when_the_last_v4_listener_exits() {
        // Exercise normal return and panic because Tokio reports them through different paths.
        for should_panic in [false, true] {
            let cancel_token = CancellationToken::new();
            let mut v4_tasks = JoinSet::new();
            let (completion_tx, completion_rx) = oneshot::channel();
            v4_tasks.spawn(async move {
                completion_tx
                    .send(())
                    .expect("supervision test waits for v4 completion");
                if should_panic {
                    panic!("synthetic v4 listener panic");
                }
            });
            completion_rx
                .await
                .expect("synthetic v4 listener reached its exit");

            // Keep a sibling pending so the test catches the former join_all masking behavior.
            let mut v6_tasks = JoinSet::new();
            v6_tasks.spawn(std::future::pending::<()>());

            // Verify supervision reports the exact failure form and tears down the generation.
            let result = timeout(
                Duration::from_secs(1),
                supervise_listener_tasks(v4_tasks, v6_tasks, cancel_token.clone()),
            )
            .await
            .expect("unexpected v4 completion must surface promptly");

            assert!(
                cancel_token.is_cancelled(),
                "unexpected v4 completion must cancel its generation"
            );
            match (should_panic, result) {
                (false, Err(V4ListenerFailure::Returned)) => {}
                (true, Err(V4ListenerFailure::Join(_))) => {}
                (_, other) => panic!("unexpected v4 supervision result: {other:?}"),
            }
        }
    }

    /// Verifies one failed v4 interface preserves healthy siblings until the last one exits.
    #[tokio::test]
    async fn listener_supervision_preserves_siblings_until_the_last_v4_listener_exits() {
        for (scenario, should_panic) in [
            // A listener can return after exhausting its bounded socket retries.
            ("normal return", false),
            // A listener panic arrives as a Tokio JoinError but has the same cardinality policy.
            ("panic", true),
        ] {
            let cancel_token = CancellationToken::new();
            let mut v4_tasks = JoinSet::new();
            let (first_exit_tx, first_exit_rx) = oneshot::channel();
            v4_tasks.spawn(async move {
                first_exit_tx
                    .send(())
                    .expect("supervision test waits for the first v4 listener");
                if should_panic {
                    panic!("synthetic v4 listener panic");
                }
            });
            first_exit_rx
                .await
                .expect("first synthetic v4 listener reached its exit");

            // Hold the healthy sibling until the partial failure has been observed.
            let (last_exit_tx, last_exit_rx) = oneshot::channel();
            v4_tasks.spawn(async move {
                last_exit_rx
                    .await
                    .expect("supervision test releases the last v4 listener");
            });

            let mut v6_tasks = JoinSet::new();
            v6_tasks.spawn(std::future::pending::<()>());

            let ((result, was_cancelled), logs) = capture_logs_async(async {
                let supervision =
                    supervise_listener_tasks(v4_tasks, v6_tasks, cancel_token.clone());
                tokio::pin!(supervision);

                assert!(
                    timeout(Duration::from_millis(100), &mut supervision)
                        .await
                        .is_err(),
                    "{scenario} from one v4 listener must preserve its healthy sibling"
                );
                assert!(
                    !cancel_token.is_cancelled(),
                    "{scenario} from one v4 listener must not cancel the generation"
                );

                last_exit_tx
                    .send(())
                    .expect("last synthetic v4 listener is waiting for release");
                let result = timeout(Duration::from_secs(1), supervision)
                    .await
                    .expect("last v4 completion must surface promptly");
                (result, cancel_token.is_cancelled())
            })
            .await;

            assert!(
                matches!(result, Err(V4ListenerFailure::Returned)),
                "last v4 listener should fail after first-listener {scenario}: {result:?}"
            );
            assert!(was_cancelled, "last v4 exit must cancel the generation");
            assert!(
                logs.iter().any(|entry| {
                    entry.message == "DHCPv4 listener exited unexpectedly"
                        && entry.field("remaining_v4_listener_count") == Some("1")
                }),
                "first-listener {scenario} must be logged as a partial failure"
            );
        }
    }

    /// Verifies explicit cancellation remains clean after partial v4 degradation.
    #[tokio::test]
    async fn listener_supervision_cancels_cleanly_after_partial_v4_failure() {
        let cancel_token = CancellationToken::new();
        let mut v4_tasks = JoinSet::new();
        v4_tasks.spawn(async {});

        // Keep both remaining families alive until the generation is intentionally cancelled.
        let v4_cancel = cancel_token.clone();
        v4_tasks.spawn(async move {
            v4_cancel.cancelled().await;
        });
        let mut v6_tasks = JoinSet::new();
        let v6_cancel = cancel_token.clone();
        v6_tasks.spawn(async move {
            v6_cancel.cancelled().await;
        });

        let mut supervision = tokio::spawn(supervise_listener_tasks(
            v4_tasks,
            v6_tasks,
            cancel_token.clone(),
        ));
        assert!(
            timeout(Duration::from_millis(100), &mut supervision)
                .await
                .is_err(),
            "partial v4 failure must keep the generation alive"
        );
        assert!(!cancel_token.is_cancelled());

        cancel_token.cancel();
        let result = timeout(Duration::from_secs(1), supervision)
            .await
            .expect("explicit cancellation must drain listener supervision")
            .expect("listener supervisor task must join");
        assert!(result.is_ok());
    }

    /// Verifies v6 exit is non-fatal and intentional generation cancellation remains clean.
    #[tokio::test]
    async fn listener_supervision_keeps_v4_running_after_v6_exit() {
        // Normal return models expected unavailability; panic models an unexpected v6 JoinError.
        for should_panic in [false, true] {
            let cancel_token = CancellationToken::new();

            // Keep v4 healthy until the generation is intentionally cancelled.
            let mut v4_tasks = JoinSet::new();
            let listener_cancel = cancel_token.clone();
            v4_tasks.spawn(async move {
                listener_cancel.cancelled().await;
            });

            // Hold v6 at a deterministic boundary until supervision is actively running.
            let mut v6_tasks = JoinSet::new();
            let (ready_tx, ready_rx) = oneshot::channel();
            let (release_tx, release_rx) = oneshot::channel();
            v6_tasks.spawn(async move {
                ready_tx
                    .send(())
                    .expect("supervision test waits for the v6 listener");
                release_rx
                    .await
                    .expect("supervision test releases the v6 listener");
                if should_panic {
                    panic!("synthetic v6 listener panic");
                }
            });

            let mut supervision = tokio::spawn(supervise_listener_tasks(
                v4_tasks,
                v6_tasks,
                cancel_token.clone(),
            ));
            ready_rx
                .await
                .expect("synthetic v6 listener reached the release boundary");

            // Release v6 while the supervisor is being polled; neither exit form may finish it.
            release_tx
                .send(())
                .expect("synthetic v6 listener is waiting for release");
            assert!(
                timeout(Duration::from_millis(100), &mut supervision)
                    .await
                    .is_err(),
                "v6 completion must not end the generation"
            );
            assert!(
                !cancel_token.is_cancelled(),
                "v6 completion must not cancel healthy v4 service"
            );

            // Explicit cancellation must drain the v4 task and return cleanly.
            cancel_token.cancel();
            let result = timeout(Duration::from_secs(1), supervision)
                .await
                .expect("intentional cancellation must finish promptly")
                .expect("listener supervision task must join");
            assert!(result.is_ok(), "intentional cancellation must be clean");
        }
    }

    /// A packet rejected by the DHCPv6 handler admission limit is counted as
    /// both received and dropped, preserving the metric subset relationship.
    #[test]
    fn rate_limited_v6_packet_is_counted_at_ingress() {
        let metrics = MetricsCapture::start();
        let packet = [u8::from(MessageTypeV6::Renew), 0, 0, 1];
        let rate_limiter = Arc::new(tokio::sync::Semaphore::new(0));

        assert!(
            admit_v6_packet(&packet, "[fe80::1]:546".parse().unwrap(), &rate_limiter,).is_none()
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_total",
                &[("message_type", "renew")]
            ),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_v6_requests_dropped_total",
                &[("reason", "rate_limited")]
            ),
            1.0
        );
    }

    /// Reload with no staged `_new` files must not start the server.
    #[tokio::test]
    async fn reload_skips_when_nothing_staged() {
        let td = TempDir::new().unwrap();
        let args = make_reload_args(&td, vec!["eth0".to_string()]);

        let (cancel_token, dhcp_handle) = handle_reload(&args, None, None, false).await.unwrap();

        assert!(cancel_token.is_none(), "no server should have been started");
        assert!(dhcp_handle.is_none(), "no server should have been started");
    }

    /// Reload with an empty interface list must return early without starting the server.
    #[tokio::test]
    async fn reload_skips_when_interfaces_empty() {
        let td = TempDir::new().unwrap();
        let args = make_reload_args(&td, vec![]);

        // Stage a `_new` file so that the only reason to skip is empty interfaces.
        let new_dhcp = format!("{}_new", args.dhcp_config);
        tokio::fs::write(&new_dhcp, "staged").await.unwrap();

        let (cancel_token, dhcp_handle) = handle_reload(&args, None, None, false).await.unwrap();

        assert!(cancel_token.is_none(), "no server should have been started");
        assert!(dhcp_handle.is_none(), "no server should have been started");
    }

    /// force_start=true must start the server even when no `_new` files are staged.
    #[tokio::test]
    async fn reload_force_start_with_no_staged_files() {
        let td = TempDir::new().unwrap();
        let args = make_reload_args(&td, vec!["eth0".to_string()]);

        // Write a live config so run_dhcp_server can initialise (it will fail to
        // bind a real socket in CI, but the important thing is that a JoinHandle
        // is returned, proving the server was attempted).
        tokio::fs::write(&args.dhcp_config, "# placeholder")
            .await
            .unwrap();

        let (cancel_token, dhcp_handle) = handle_reload(&args, None, None, true).await.unwrap();

        assert!(
            cancel_token.is_some(),
            "server should have been started with force_start"
        );
        assert!(
            dhcp_handle.is_some(),
            "server should have been started with force_start"
        );

        // Clean up the spawned task.
        if let (Some(ct), Some(h)) = (cancel_token, dhcp_handle) {
            ct.cancel();
            let _ = h.await;
        }
    }

    /// force_start=false must still skip when no `_new` files are staged,
    /// even when a live config exists on disk.
    #[tokio::test]
    async fn reload_no_force_start_with_no_staged_files() {
        let td = TempDir::new().unwrap();
        let args = make_reload_args(&td, vec!["eth0".to_string()]);
        tokio::fs::write(&args.dhcp_config, "# placeholder")
            .await
            .unwrap();

        let (cancel_token, dhcp_handle) = handle_reload(&args, None, None, false).await.unwrap();

        assert!(
            cancel_token.is_none(),
            "server must not start without staged files"
        );
        assert!(
            dhcp_handle.is_none(),
            "server must not start without staged files"
        );
    }

    /// Sending Stop over the control channel must cancel the running server and
    /// leave dhcp_handle as None, while the subsequent UpdateAndReload (with the
    /// server down) must force-start it again.
    #[tokio::test]
    async fn stop_then_update_restarts_server() {
        let td = TempDir::new().unwrap();
        let mut args = make_reload_args(&td, vec!["eth0".to_string()]);

        // Provide a live config so handle_reload can start a server task.
        let dhcp_yaml = "# placeholder dhcp";
        tokio::fs::write(&args.dhcp_config, dhcp_yaml)
            .await
            .unwrap();

        // Simulate a running server.
        let ct = CancellationToken::new();
        let ct_clone = ct.clone();
        let mut dhcp_handle: Option<tokio::task::JoinHandle<()>> =
            Some(tokio::spawn(async move { ct_clone.cancelled().await }));
        let mut cancel_token: Option<CancellationToken> = Some(ct);

        // --- StopServer ---
        if let (Some(ct), Some(h)) = (cancel_token.take(), dhcp_handle.take()) {
            ct.cancel();
            let _ = h.await;
        }
        assert!(
            cancel_token.is_none(),
            "cancel_token must be None after stop"
        );
        assert!(dhcp_handle.is_none(), "dhcp_handle must be None after stop");

        // --- UpdateAndReload with server down (force=true) ---
        // Stage a _new` config so handle_update_config has something to write,
        // then call handle_reload with force=true (the path taken when dhcp_handle is None).
        let new_dhcp_yaml = "# updated dhcp";
        tokio::fs::write(format!("{}_new", args.dhcp_config), new_dhcp_yaml)
            .await
            .unwrap();
        args.interfaces = vec!["eth0".to_string()];

        let force = dhcp_handle.is_none(); // true — server is down
        let (ct, h) = handle_reload(&args, cancel_token, dhcp_handle, force)
            .await
            .unwrap();

        assert!(ct.is_some(), "server should have been restarted");
        assert!(h.is_some(), "server should have been restarted");

        if let (Some(ct), Some(h)) = (ct, h) {
            ct.cancel();
            let _ = h.await;
        }
    }

    /// Stop when no server is running must be a no-op (no panic, handle stays None).
    #[tokio::test]
    async fn stop_when_server_not_running_is_noop() {
        let mut cancel_token: Option<CancellationToken> = None;
        let mut dhcp_handle: Option<tokio::task::JoinHandle<()>> = None;

        // Mirrors the Stop arm in run_with_grpc_control.
        if let (Some(ct), Some(h)) = (cancel_token.take(), dhcp_handle.take()) {
            ct.cancel();
            let _ = h.await;
        }

        assert!(cancel_token.is_none());
        assert!(dhcp_handle.is_none());
    }

    fn get_test_args() -> Args {
        let base_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

        Args {
            interfaces: vec!["eth0".to_string()],
            listen_addr: "0.0.0.0:67".parse().unwrap(),
            relay_response_port: 67,
            dhcp_config: base_path.join("conf/conf.yaml").display().to_string(),
            host_config: Some(
                base_path
                    .join("test/host_config.yaml")
                    .display()
                    .to_string(),
            ),
            forge_root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            mode: crate::command_line::ServerMode::Dpu,
            grpc_listen_addr: None,
            metrics_listen_addr: None,
        }
    }

    #[tokio::test]
    async fn test_init() {
        init(get_test_args()).await.unwrap();
    }

    #[test]
    fn forge_client_tls_paths_are_configurable() {
        let defaults = forge_client_config(&get_test_args()).unwrap();
        assert_eq!(defaults.root_ca_path, forge_tls::default::ROOT_CA);
        let default_identity = defaults.client_cert.unwrap();
        assert_eq!(default_identity.cert_path, forge_tls::default::CLIENT_CERT);
        assert_eq!(default_identity.key_path, forge_tls::default::CLIENT_KEY);

        let mut explicit = get_test_args();
        explicit.forge_root_ca_path = Some("/local/ca.crt".to_string());
        explicit.client_cert_path = Some("/local/client.crt".to_string());
        explicit.client_key_path = Some("/local/client.key".to_string());
        let configured = forge_client_config(&explicit).unwrap();
        assert_eq!(configured.root_ca_path, "/local/ca.crt");
        let configured_identity = configured.client_cert.unwrap();
        assert_eq!(configured_identity.cert_path, "/local/client.crt");
        assert_eq!(configured_identity.key_path, "/local/client.key");

        explicit.client_key_path = None;
        assert!(forge_client_config(&explicit).is_err());
    }

    #[tokio::test]
    async fn test_arm_non_relayed_packet() {
        let byte_stream =
            get_byte_stream(Ipv4Addr::new(0, 0, 0, 0), None, MessageType::Request, None);
        let handler: Box<dyn DhcpMode> = Box::new(TestArm {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        assert!(matches!(
            packet_handler::process_packet(
                &byte_stream,
                TEST_SOURCE_ADDRESS,
                &config,
                "vlan200",
                &*handler,
                &mut machine_cache,
            )
            .await,
            Err(DhcpError::NonRelayedPacket(..))
        ));
    }

    #[tokio::test]
    async fn test_arm_relayed_packet() {
        let byte_stream = get_byte_stream(
            Ipv4Addr::new(0, 0, 0, 0),
            Some(Ipv4Addr::from_str("10.217.5.41").unwrap()),
            MessageType::Request,
            None,
        );
        let handler: Box<dyn DhcpMode> = Box::new(TestArm {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        assert!(
            packet_handler::process_packet(
                &byte_stream,
                TEST_SOURCE_ADDRESS,
                &config,
                "vlan200",
                &*handler,
                &mut machine_cache,
            )
            .await
            .is_ok()
        );
    }

    /// A raw HTTP-client option 60 reaches the shared vendor-class parser
    /// before the standalone server builds its reply. The reply keeps the
    /// canonical client ID and uses the parsed architecture for option 67.
    #[tokio::test]
    async fn test_complete_http_boot_flow() {
        let byte_stream = get_byte_stream(
            Ipv4Addr::new(0, 0, 0, 0),
            Some(Ipv4Addr::from_str("10.217.5.41").unwrap()),
            MessageType::Request,
            Some(b"HTTPClient::7::"),
        );
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let mut args = get_test_args();
        args.relay_response_port = 6768;
        let config = init(args).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        let packet = packet_handler::process_packet(
            &byte_stream,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await
        .unwrap();

        assert_eq!(
            handler.get_destination_address(&packet),
            SocketAddrV4::new(Ipv4Addr::from([0x0a, 0xd9, 0x05, 0x29]), 6768)
        );
        let packet = Message::decode(&mut dhcproto::Decoder::new(packet.encoded_packet())).unwrap();

        assert_eq!(packet.yiaddr(), Ipv4Addr::from([10, 217, 132, 204]));
        assert_eq!(
            packet.opts().get(OptionCode::ClassIdentifier),
            Some(&DhcpOption::ClassIdentifier(b"HTTPClient".to_vec()))
        );
        assert_eq!(
            packet.opts().get(OptionCode::BootfileName),
            Some(&DhcpOption::BootfileName(
                b"http://10.217.126.17:8080/public/blobs/internal/x86_64/ipxe.efi".to_vec()
            ))
        );
    }

    /// A decoded packet writes bounded request details at INFO, the complete
    /// packet at DEBUG, and ticks the counter even when later processing fails.
    #[tokio::test]
    async fn process_packet_logs_and_counts_the_decoded_request() {
        // No other test in this binary processes an Inform, so this label's
        // delta is immune to tests running in parallel.
        let byte_stream =
            get_byte_stream(Ipv4Addr::new(0, 0, 0, 0), None, MessageType::Inform, None);
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        let expected_received_packet = Message::decode(&mut Decoder::new(&byte_stream)).unwrap();
        let expected_received_packet_text = expected_received_packet.to_string();

        let metrics = carbide_instrument::testing::MetricsCapture::start();
        let (result, logs) = capture_logs_async(packet_handler::process_packet(
            &byte_stream,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        ))
        .await;

        assert!(matches!(result, Err(DhcpError::UnhandledMessageType(..))));
        let request_log_index = logs
            .iter()
            .position(|entry| entry.metadata_name == "dhcp_server_request_received")
            .expect("the decoded request Event should write an INFO record");
        let request_log = &logs[request_log_index];
        assert_eq!(request_log.level, tracing::Level::INFO);
        assert_eq!(request_log.field("bootp_op"), Some("1"));
        assert_eq!(request_log.field("source_address"), Some("192.0.2.10:68"));
        assert_eq!(
            request_log.field("xid"),
            Some(expected_received_packet.xid().to_string().as_str())
        );
        assert_eq!(
            request_log.field("broadcast_flag"),
            Some(
                expected_received_packet
                    .flags()
                    .broadcast()
                    .to_string()
                    .as_str()
            )
        );
        assert_eq!(
            request_log.field("ciaddr"),
            Some(expected_received_packet.ciaddr().to_string().as_str())
        );
        assert_eq!(
            request_log.field("yiaddr"),
            Some(expected_received_packet.yiaddr().to_string().as_str())
        );
        assert_eq!(
            request_log.field("siaddr"),
            Some(expected_received_packet.siaddr().to_string().as_str())
        );
        assert_eq!(
            request_log.field("giaddr"),
            Some(expected_received_packet.giaddr().to_string().as_str())
        );
        assert_eq!(request_log.field("chaddr"), Some(TEST_CLIENT_MAC_TEXT));
        assert_eq!(request_log.field("received_packet"), None);

        let debug_log = logs
            .get(request_log_index + 1)
            .expect("the full-packet DEBUG record should immediately follow the Event");
        assert_eq!(debug_log.level, tracing::Level::DEBUG);
        assert_eq!(debug_log.message, "Received Packet");
        assert_eq!(
            debug_log.field("packet.received"),
            Some(expected_received_packet_text.as_str())
        );
        assert_eq!(
            metrics.counter_delta("carbide_dhcp_requests_total", &[("message_type", "inform")]),
            1.0
        );
    }

    /// A wire-provided hardware-address length cannot make the structured INFO
    /// field or full-packet DEBUG formatter index beyond BOOTP's fixed field.
    #[tokio::test]
    async fn process_packet_rejects_oversized_hardware_address_length() {
        let mut byte_stream =
            get_byte_stream(Ipv4Addr::new(0, 0, 0, 0), None, MessageType::Request, None);
        byte_stream[2] = 17;
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        let result = packet_handler::process_packet(
            &byte_stream,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await;

        assert!(matches!(
            result,
            Err(DhcpError::InvalidInput(error))
                if error == "DHCP hardware address length 17 exceeds the 16-byte BOOTP field"
        ));
    }

    #[tokio::test]
    async fn process_packet_rejects_an_empty_buffer() {
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        let result = packet_handler::process_packet(
            &[],
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await;

        assert!(matches!(
            result,
            Err(DhcpError::PacketDecodeFailure(
                dhcproto::error::DecodeError::NotEnoughBytes
            ))
        ));
    }

    /// A successful send writes bounded reply details at INFO and immediately
    /// follows them with the complete packet at DEBUG.
    #[tokio::test]
    async fn send_logs_bounded_reply_details_before_the_full_packet() {
        let byte_stream =
            get_byte_stream(Ipv4Addr::new(0, 0, 0, 0), None, MessageType::Request, None);
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        let packet = packet_handler::process_packet(
            &byte_stream,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await
        .unwrap();
        let expected_reply = Message::decode(&mut Decoder::new(packet.encoded_packet())).unwrap();
        let expected_reply_text = expected_reply.to_string();

        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(destination_address) = receiver.local_addr().unwrap() else {
            panic!("the IPv4 loopback receiver should have an IPv4 address");
        };
        let socket = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());

        let (result, logs) = capture_logs_async(packet.send(destination_address, socket)).await;
        result.unwrap();

        let reply_log_index = logs
            .iter()
            .position(|entry| entry.metadata_name == "dhcp_server_reply_sent")
            .expect("the successful send should write an INFO Event");
        let reply_log = &logs[reply_log_index];
        assert_eq!(reply_log.level, tracing::Level::INFO);
        assert_eq!(reply_log.field("message_type"), Some("ack"));
        assert_eq!(
            reply_log.field("destination_address"),
            Some(destination_address.to_string().as_str())
        );
        assert_eq!(
            reply_log.field("xid"),
            Some(expected_reply.xid().to_string().as_str())
        );
        assert_eq!(
            reply_log.field("broadcast_flag"),
            Some(expected_reply.flags().broadcast().to_string().as_str())
        );
        assert_eq!(
            reply_log.field("ciaddr"),
            Some(expected_reply.ciaddr().to_string().as_str())
        );
        assert_eq!(
            reply_log.field("yiaddr"),
            Some(expected_reply.yiaddr().to_string().as_str())
        );
        assert_eq!(
            reply_log.field("siaddr"),
            Some(expected_reply.siaddr().to_string().as_str())
        );
        assert_eq!(
            reply_log.field("giaddr"),
            Some(expected_reply.giaddr().to_string().as_str())
        );
        assert_eq!(reply_log.field("chaddr"), Some(TEST_CLIENT_MAC_TEXT));
        assert_eq!(reply_log.field("sent_packet"), None);

        let debug_log = logs
            .get(reply_log_index + 1)
            .expect("the full-packet DEBUG record should immediately follow the Event");
        assert_eq!(debug_log.level, tracing::Level::DEBUG);
        assert_eq!(debug_log.message, "Sent DHCP packet");
        assert_eq!(
            debug_log.field("packet.send"),
            Some(expected_reply_text.as_str())
        );
    }

    #[tokio::test]
    async fn test_complete_flow_with_valid_ciaddr() {
        let byte_stream = get_byte_stream(
            Ipv4Addr::new(10, 217, 132, 204),
            Some(Ipv4Addr::from_str("10.217.5.41").unwrap()),
            MessageType::Request,
            None,
        );
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        let packet = packet_handler::process_packet(
            &byte_stream,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await
        .unwrap();

        assert_eq!(
            handler.get_destination_address(&packet),
            SocketAddrV4::new(Ipv4Addr::from([10, 217, 5, 41]), 67)
        );

        let packet = Message::decode(&mut dhcproto::Decoder::new(packet.encoded_packet())).unwrap();

        assert_eq!(packet.yiaddr(), Ipv4Addr::from([10, 217, 132, 204]));
    }

    #[tokio::test]
    async fn test_send_metadata_to_agent() {
        let byte_stream =
            get_byte_stream(Ipv4Addr::new(0, 0, 0, 0), None, MessageType::Discover, None);
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let config = init(get_test_args()).await.unwrap();
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        // Remove any timestamps file left behind from a previous run.
        if std::fs::exists(DhcpTimestampsFilePath::Test.path_str()).unwrap() {
            std::fs::remove_file(DhcpTimestampsFilePath::Test.path_str()).unwrap();
        }

        // Try a read() to show that it will fail if the timestamps file
        // hasn't been initialized.
        let _ = DhcpTimestamps::new(DhcpTimestampsFilePath::Test)
            .read()
            .unwrap_err();

        let before_dhcp = Utc::now();
        let udp_socket_addr: SocketAddrV4 = "127.0.0.1:1236".parse().unwrap();
        let dhcp_timestamps = Arc::new(Mutex::new({
            let d = DhcpTimestamps::new(DhcpTimestampsFilePath::Test);
            // Init the file like we would do during live operation.
            d.write().unwrap();
            d
        }));

        // Try a read() to show that the "init" of the timestamps file was
        // successful.
        DhcpTimestamps::new(DhcpTimestampsFilePath::Test)
            .read()
            .unwrap();

        process(
            "1.2.3.4:0".parse().unwrap(),
            Arc::new(UdpSocket::bind(udp_socket_addr).await.unwrap()),
            &byte_stream,
            config.clone(),
            &*handler,
            "vlan100",
            &mut machine_cache,
            dhcp_timestamps.clone(),
        )
        .await;

        let dhcp_timestamps = dhcp_timestamps.lock().await;

        let timestamp = dhcp_timestamps
            .get_timestamp(&config.host_config().unwrap().host_interface_id)
            .unwrap();

        let dhcp_time: DateTime<Utc> = timestamp.parse().unwrap();
        assert!(before_dhcp < dhcp_time);

        let mut dhcp_timestamps_new = DhcpTimestamps::new(DhcpTimestampsFilePath::Test);
        dhcp_timestamps_new.read().unwrap();
        let file_timestamp: DateTime<Utc> = dhcp_timestamps_new
            .get_timestamp(&config.host_config().unwrap().host_interface_id)
            .unwrap()
            .parse()
            .unwrap();

        assert!(before_dhcp < file_timestamp)
    }

    #[tokio::test]
    async fn validate_test_host_config() {
        let config = init(get_test_args()).await.unwrap();

        let host_config = config.host_config().unwrap();
        assert_eq!(host_config.host_ip_addresses.len(), 2);
        assert!(host_config.host_ip_addresses["vlan200"].booturl.is_none());
    }

    fn get_byte_stream(
        ciaddr: Ipv4Addr,
        giaddr: Option<Ipv4Addr>,
        message_type: MessageType,
        class_identifier: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut msg = Message::new(
            ciaddr,
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(0, 0, 0, 0),
            TEST_CLIENT_MAC,
        );

        if let Some(giaddr) = giaddr {
            msg.set_giaddr(giaddr);
        }

        msg.opts_mut().insert(DhcpOption::MessageType(message_type));
        if let Some(class_identifier) = class_identifier {
            msg.opts_mut()
                .insert(DhcpOption::ClassIdentifier(class_identifier.to_vec()));
        }

        let mut encoded_packet = Vec::new();
        let mut e = dhcproto::Encoder::new(&mut encoded_packet);
        msg.encode(&mut e).unwrap();
        encoded_packet
    }

    #[tokio::test]
    async fn validate_basic_ack() {
        let packet = get_byte_stream(Ipv4Addr::new(0, 0, 0, 0), None, MessageType::Request, None);

        let config = init(get_test_args()).await.unwrap();
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        let encoded_packet = packet_handler::process_packet(
            &packet,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await
        .unwrap();

        assert_eq!(
            handler.get_destination_address(&encoded_packet),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, 68)
        );

        let packet = Message::decode(&mut Decoder::new(encoded_packet.encoded_packet())).unwrap();
        assert_eq!(
            packet.opts().get(OptionCode::MessageType).unwrap().clone(),
            DhcpOption::MessageType(MessageType::Ack)
        );
    }

    #[tokio::test]
    async fn validate_nak() {
        let packet = get_byte_stream(Ipv4Addr::new(10, 0, 0, 1), None, MessageType::Request, None);

        let config = init(get_test_args()).await.unwrap();
        let handler: Box<dyn DhcpMode> = Box::new(Test {});
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        let encoded_packet = packet_handler::process_packet(
            &packet,
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &*handler,
            &mut machine_cache,
        )
        .await
        .unwrap();

        let packet = Message::decode(&mut Decoder::new(encoded_packet.encoded_packet())).unwrap();
        assert_eq!(
            packet.opts().get(OptionCode::MessageType).unwrap().clone(),
            DhcpOption::MessageType(MessageType::Nak)
        );
    }
}
