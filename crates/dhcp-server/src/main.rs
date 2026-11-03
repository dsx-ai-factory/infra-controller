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
mod server_identity;
use std::error::Error;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::Arc;

use ::rpc::forge_tls_client::ForgeClientConfig;
use carbide_dhcp_server::cache::{self, CacheEntry};
use carbide_dhcp_server::errors::DhcpError;
use carbide_dhcp_server::metrics::{
    DhcpPacketDropped, DhcpTimestampFileFailed, DhcpV6ListenerUnavailable, DhcpV6RequestDropped,
    DropReason, V6DropReason, record_v6_request_received,
};
use carbide_dhcp_server::modes::DhcpMode;
use carbide_dhcp_server::modes::controller::Controller;
use carbide_dhcp_server::modes::dpu::{Dpu, get_host_config};
use carbide_dhcp_server::{
    Config, ReplyStatus, packet_handler, packet_handler_v6, util,
    validate_stateful_lifetimes_for_host,
};
use carbide_instrument::emit;
use carbide_rpc_utils::dhcp::{DhcpConfig, DhcpTimestamps, DhcpTimestampsFilePath, ServiceConfig};
use chrono::Utc;
use command_line::{Args, ServerMode};
use forge_tls::client_config::ClientCert;
use forge_tls::default::{default_client_cert, default_client_key, default_root_ca};
use grpc_server::{ApplyError, ControlRequest, run_grpc_server};
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

/// Pauses one source-matched IPv4 packet task so tests can observe listener-owned drainage.
#[cfg(test)]
struct PacketTaskPause {
    source: SocketAddr,
    started: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

/// Holds the one-shot pause used to distinguish listener-owned packet work from detached tasks.
#[cfg(test)]
static IPV4_PACKET_TASK_PAUSE: std::sync::Mutex<Option<PacketTaskPause>> =
    std::sync::Mutex::new(None);

/// Pauses before UDP processing only for the source selected by the listener-ownership test.
#[cfg(test)]
async fn pause_ipv4_packet_task_for_test(source: SocketAddr) {
    let pause = {
        let mut configured = IPV4_PACKET_TASK_PAUSE.lock().unwrap();
        if configured
            .as_ref()
            .is_some_and(|pause| pause.source == source)
        {
            configured.take()
        } else {
            None
        }
    };
    if let Some(pause) = pause {
        let _ = pause.started.send(());
        let _ = pause.release.await;
    }
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

/// Records why a required listener exited before generation cancellation.
#[derive(Debug)]
enum ListenerFailure {
    /// The listener returned before its generation was cancelled.
    Returned,
    /// The listener task panicked or was otherwise cancelled unexpectedly.
    Join(tokio::task::JoinError),
}

impl std::fmt::Display for ListenerFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Returned => {
                formatter.write_str("listener returned before generation cancellation")
            }
            Self::Join(error) => write!(formatter, "listener task failed: {error}"),
        }
    }
}

/// Loads durable host activity when possible while keeping DHCP listeners available after
/// timestamp-file damage.
fn initialize_dhcp_timestamps(path: DhcpTimestampsFilePath) -> DhcpTimestamps {
    let path_context = path.path_str().to_string();
    let mut timestamps = DhcpTimestamps::new(path);

    // Timestamp history is advisory, so corruption must not take packet service down.
    if let Err(error) = timestamps.initialize_file() {
        emit(DhcpTimestampFileFailed::Initialization {
            dhcp_timestamps_path: path_context,
            error: error.to_string(),
        });
    }
    timestamps
}

/// Reaps completed packet tasks during normal service so the listener retains only bounded active
/// work.
fn reap_packet_tasks(packet_tasks: &mut JoinSet<()>, family: &str, interface: &str) {
    while let Some(result) = packet_tasks.try_join_next() {
        if let Err(error) = result {
            tracing::warn!(%error, family, interface_name = interface, "DHCP packet task failed");
        }
    }
}

/// Aborts and joins packet work before a listener returns so replacement cannot outlive an old
/// socket write.
async fn abort_and_join_packet_tasks(
    packet_tasks: &mut JoinSet<()>,
    family: &str,
    interface: &str,
) {
    packet_tasks.abort_all();
    while let Some(result) = packet_tasks.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            tracing::warn!(%error, family, interface_name = interface, "DHCP packet task failed");
        }
    }
}

/// Creates an IPv4 socket while allowing generation cancellation to interrupt setup retries,
/// preventing reloads from waiting on a retired interface.
async fn get_ipv4_socket_before_cancellation(
    listen_address: core::net::SocketAddrV4,
    interface: String,
    cancel: &CancellationToken,
) -> Option<Result<UdpSocket, DhcpError>> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        result = get_socket(listen_address, interface) => Some(result),
    }
}

/// Serve an immutable configuration until cancellation or the last required
/// listener exits, without rereading files changed by the agent.
async fn run_dhcp_generation(
    args: Args,
    cancel_token: CancellationToken,
    config__: Config,
    v6_port: u16,
) {
    // Attach retirement to every response from this immutable generation so reload can suppress in-flight sends.
    let config__ = Arc::new(config__.with_reply_retirement(cancel_token.clone()));
    let ipv4_enabled = match config__.ipv4() {
        Ok(ipv4) => ipv4.is_some(),
        Err(error) => {
            tracing::error!(%error, "Invalid DHCPv4 configuration");
            return;
        }
    };

    let dhcp_timestamps = Arc::new(Mutex::new(initialize_dhcp_timestamps(
        // Binary tests exercise live DPU generations without reading or replacing the host's production history.
        if cfg!(test) {
            DhcpTimestampsFilePath::NotSet
        } else if let ServerMode::Dpu = args.mode {
            DhcpTimestampsFilePath::HbnTmp
        } else {
            DhcpTimestampsFilePath::NotSet
        },
    )));

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
        let v6_config = Arc::clone(&config__);
        let v6_mode = args.mode.clone();
        let v6_timestamps = dhcp_timestamps.clone();
        let v6_rate_limiter = v6_rate_limiter_.clone();
        let v6_cancel = cancel_token.clone();
        let config_ = Arc::clone(&config__);
        let args_mode = args.mode.clone();
        let listen_address = args.listen_addr;
        let dhcp_timestamps_ = dhcp_timestamps.clone();
        let rate_limiter = rate_limiter_.clone();
        let cancel = cancel_token.clone();

        if ipv4_enabled {
            v4_tasks.spawn(async move {
                let handler: Arc<Box<dyn DhcpMode>> = Arc::new(get_mode(&args_mode));

                let Some(socket_result) = get_ipv4_socket_before_cancellation(
                    listen_address,
                    interface.clone(),
                    &cancel,
                )
                .await
                else {
                    return;
                };
                let socket = match socket_result {
                    Ok(socket) => socket,
                    Err(error) => {
                        tracing::error!(
                            %listen_address,
                            interface_name = interface.as_str(),
                            %error,
                            "DHCP socket setup failed"
                        );
                        return;
                    }
                };
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
                let mut packet_tasks = JoinSet::new();
                // Listen on each interface and process it.
                // The select! monitors both the UDP socket and the cancellation token so that
                // the loop exits promptly when a config reload is triggered from the gRPC server.
                loop {
                    let mut buf = [0; 1500];
                    tokio::select! {
                        biased;
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
                                    let Some(socket_result) = get_ipv4_socket_before_cancellation(
                                        listen_address,
                                        interface.clone(),
                                        &cancel,
                                    )
                                    .await
                                    else {
                                        break;
                                    };
                                    let socket = match socket_result {
                                        Ok(socket) => socket,
                                        Err(error) => {
                                            tracing::error!(
                                                %listen_address,
                                                interface_name = interface.as_str(),
                                                %error,
                                                "Socket recreation failed"
                                            );
                                            break;
                                        }
                                    };
                                    server.socket = Arc::new(socket);
                                    continue;
                                }
                            };
                            reap_packet_tasks(&mut packet_tasks, "IPv4", &interface);

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

                            let config = Arc::clone(&config_);
                            let mut machine_cache = machine_cache_.clone();
                            let iface = interface.clone();
                            let handler_ = handler.clone();
                            let dhcp_timestamps = dhcp_timestamps_.clone();
                            let socket = server.socket.clone();

                            packet_tasks.spawn(async move {
                                #[cfg(test)]
                                pause_ipv4_packet_task_for_test(addr).await;
                                process(
                                    addr,
                                    socket,
                                    &buf,
                                    config,
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
                // Cancellation closes admission before aborting and joining every accepted packet.
                abort_and_join_packet_tasks(&mut packet_tasks, "IPv4", &interface).await;
            });
        }

        // An unavailable IPv6 socket cannot take down enabled DHCPv4 service.
        // With DHCPv4 disabled, the generation instead needs a live v6 listener.
        v6_tasks.spawn(run_dhcp_v6_listener(
            v6_interface,
            v6_config,
            v6_mode,
            v6_cancel,
            v6_rate_limiter,
            v6_timestamps,
            v6_port,
        ));
    }

    // IPv6 keeps the generation alive when DHCPv4 is disabled.
    if let Err(error) = supervise_listener_tasks(v4_tasks, v6_tasks, cancel_token).await {
        tracing::error!(
            error = %error,
            "Required DHCP listener exited unexpectedly"
        );
    }
}

/// Supervises a generation while preserving the listeners' family-specific semantics.
///
/// An enabled v4 listener must run until generation cancellation, so every earlier completion
/// is reported and losing the last v4 listener fails the generation. A normally
/// returning v6 task represents expected optional listener unavailability and does
/// not stop healthy v4 service. With no IPv4 configuration, IPv6 listeners instead
/// keep the generation alive and losing the last one is a failure.
async fn supervise_listener_tasks(
    mut v4_tasks: JoinSet<()>,
    mut v6_tasks: JoinSet<()>,
    cancel_token: CancellationToken,
) -> Result<(), ListenerFailure> {
    let ipv6_required = v4_tasks.is_empty();
    if v4_tasks.is_empty() && v6_tasks.is_empty() {
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
                    Ok(()) => ListenerFailure::Returned,
                    Err(error) => ListenerFailure::Join(error),
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
                break Some(failure);
            }
            Some(result) = v6_tasks.join_next(), if !v6_tasks.is_empty() => {
                if cancel_token.is_cancelled() {
                    break None;
                }
                if ipv6_required {
                    let failure = match result {
                        Ok(()) => ListenerFailure::Returned,
                        Err(error) => ListenerFailure::Join(error),
                    };
                    tracing::error!(
                        error = %failure,
                        remaining_v6_listener_count = v6_tasks.len(),
                        "DHCPv6 listener exited unexpectedly"
                    );
                    if v6_tasks.is_empty() {
                        cancel_token.cancel();
                        break Some(failure);
                    }
                    continue;
                }
                if let Err(error) = result {
                    tracing::warn!(
                        error = %error,
                        "DHCPv6 listener exited unexpectedly"
                    );
                }
            }
        }
    };

    // Every normally exiting listener aborts and joins its packet tasks before returning.
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
    config: Arc<Config>,
    mode: ServerMode,
    cancel: CancellationToken,
    rate_limiter: Arc<tokio::sync::Semaphore>,
    dhcp_timestamps: Arc<Mutex<DhcpTimestamps>>,
    port: u16,
) {
    let handler: Arc<Box<dyn DhcpMode>> = Arc::new(get_mode(&mode));
    // Controller mode accepts relay traffic; DPU mode retains its direct-only
    // trust boundary and therefore does not subscribe to relay discovery.
    let join_site_scoped_group = matches!(mode, ServerMode::Controller);
    let listen_address = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0);

    // Socket retries remain interruptible when a server generation is cancelled.
    let socket_result = tokio::select! {
        _ = cancel.cancelled() => return,
        result = get_socket_v6(listen_address, &interface, join_site_scoped_group) => result,
    };
    let socket = match socket_result {
        Ok(socket) => Arc::new(socket),
        Err(error) => {
            // Supervision decides whether the remaining listeners can keep
            // serving the enabled address family after this interface fails.
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
        config,
        handler,
        interface,
        machine_cache,
        dhcp_timestamps,
    };
    let mut packet_tasks = JoinSet::new();

    // DHCPv6 has a four-byte base header, so it intentionally does not use
    // the DHCPv4 path's 236-byte BOOTP minimum. Keep one full-size UDP buffer
    // per listener so relay options cannot be silently truncated at Ethernet MTU.
    let mut buffer = vec![0; usize::from(u16::MAX)];
    'receive: loop {
        tokio::select! {
            biased;
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
                            _ = cancel.cancelled() => break 'receive,
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
                                break 'receive;
                            }
                        }
                        continue;
                    }
                };

                let Some(permit) = admit_v6_packet(&buffer[..length], source, &rate_limiter) else {
                    continue;
                };
                reap_packet_tasks(&mut packet_tasks, "IPv6", &context.interface);

                let packet = buffer[..length].to_vec();
                let packet_context = context.clone();
                packet_tasks.spawn(async move {
                    process_v6(source, &packet, packet_context).await;
                    drop(permit);
                });
            }
        }
    }
    // Cancellation closes admission before aborting and joining every accepted packet.
    abort_and_join_packet_tasks(&mut packet_tasks, "IPv6", &context.interface).await;
}

/// Initialises the tracing subscriber with per-crate log-level overrides and
/// OTLP span export. `main` shuts down the returned value to send the last spans.
fn setup_tracing() -> Result<carbide_instrument::otlp_tracing::Tracing, Box<dyn Error>> {
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

    let (span_layer, tracing) = carbide_instrument::otlp_tracing::setup(
        carbide_instrument::otlp_tracing::Config::new("nico-dhcp-server"),
    );

    // Counts every log line into carbide_log_events_total from startup; the
    // counts are exposed once main() installs the meter provider. Each log layer
    // carries the env filter, so the counting layer and the logfmt output see
    // exactly the same events and RUST_LOG does not limit span export.
    let log_events = carbide_instrument::LogEventsMetric::new("nico-dhcp");
    tracing_subscriber::registry()
        .with(log_events.layer().with_filter(env_filter.clone()))
        .with(span_layer)
        .with(
            logfmt::layer()
                .with_event_fields([logfmt::EventField::with_default("component", "nico-dhcp")])
                .with_filter(env_filter),
        )
        .try_init()?;

    tracing.report();

    Ok(tracing)
}

/// Retain the server identity and stage changed settings for a later reload.
/// Unchanged settings discard stale staged replacements. Omitted host settings
/// retain the live host file; validation requiring both files happens at reload.
async fn handle_update_config(
    args: &Args,
    dhcp_yaml: String,
    host_yaml: Option<String>,
) -> Result<(), ApplyError> {
    let mut config: DhcpConfig = serde_yaml::from_str(&dhcp_yaml)
        .map_err(|error| ApplyError::InvalidConfig(error.into()))?;
    config.dhcpv6_server_id = Some(
        server_identity::resolve(&args.dhcp_config, &config)
            .await
            .map_err(|error| match error {
                // Stored identity failures have file context. Only a fresh
                // candidate without an identity belongs to the RPC caller.
                DhcpError::Config(_) => ApplyError::InvalidConfig(error),
                error => ApplyError::Internal(error),
            })?,
    );
    config
        .validate()
        .map_err(|error| ApplyError::InvalidConfig(error.into()))?;
    if let Some(yaml) = &host_yaml {
        serde_yaml::from_str::<carbide_rpc_utils::dhcp::HostConfig>(yaml)
            .map_err(|error| ApplyError::InvalidConfig(error.into()))?;
    }
    // Retain the old identity in the candidate. Reload persists it after
    // validating both files, before replacing the live configuration.
    stage_config(
        &args.dhcp_config,
        &serde_yaml::to_string(&config).map_err(DhcpError::from)?,
    )
    .await?;

    if let Some(path) = &args.host_config {
        if let Some(yaml) = host_yaml {
            stage_config(path, &yaml).await?;
        } else {
            discard_staged(path).await?;
        }
    }
    Ok(())
}

async fn stage_config(path: &str, yaml: &str) -> Result<(), DhcpError> {
    let current = match tokio::fs::read_to_string(path).await {
        Ok(current) => Some(current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(DhcpError::ConfigFile {
                path: path.to_string(),
                source: Box::new(error.into()),
            });
        }
    };
    let staged = format!("{path}_new");
    if current.as_deref() == Some(yaml) {
        // A previous failed update must not be applied by this unchanged retry.
        discard_staged(path).await?;
    } else {
        tokio::fs::write(&staged, yaml)
            .await
            .map_err(|error| DhcpError::ConfigFile {
                path: staged,
                source: Box::new(error.into()),
            })?;
    }
    Ok(())
}

async fn discard_staged(path: &str) -> Result<(), DhcpError> {
    let staged = format!("{path}_new");
    match tokio::fs::remove_file(&staged).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DhcpError::ConfigFile {
            path: staged,
            source: Box::new(error.into()),
        }),
    }
}

/// Retires and joins the running generation before starting its validated immutable successor,
/// ensuring stale responses fail their final send guard.
async fn replace_generation(
    args: &Args,
    config: Config,
    cancel_token: &mut Option<CancellationToken>,
    dhcp_handle: &mut Option<tokio::task::JoinHandle<()>>,
    v6_port: u16,
) {
    if let (Some(ct), Some(h)) = (cancel_token.take(), dhcp_handle.take()) {
        tracing::info!("Stopping current DHCP server");
        ct.cancel();
        if let Err(error) = h.await {
            tracing::warn!(%error, "Previous DHCP generation failed during replacement");
        }
    }

    let ct = CancellationToken::new();
    let handle = tokio::spawn(run_dhcp_generation(
        args.clone(),
        ct.clone(),
        config,
        v6_port,
    ));
    tracing::info!("DHCP server (re)started with updated config");
    *cancel_token = Some(ct);
    *dhcp_handle = Some(handle);
}

/// Promotes staged config files and (re)starts the DHCP server.
///
/// If no `_new` files exist and `force_start` is false the restart is skipped.
/// When `force_start` is true (e.g. after an explicit `StopServer`) the server
/// is started even if the config on disk has not changed. Candidate files are
/// validated and promoted before replacing the running generation.
/// Rejection leaves that generation running with its immutable config.
async fn handle_reload_with_service_config(
    args: &Args,
    cancel_token: &mut Option<CancellationToken>,
    dhcp_handle: &mut Option<tokio::task::JoinHandle<()>>,
    force_start: bool,
    service_config: ServiceConfig,
    v6_port: u16,
) -> Result<(), DhcpError> {
    if args.interfaces.is_empty() {
        // Keep the running generation until a later update supplies interfaces
        // for the staged configuration.
        tracing::warn!("ReloadConfig: no interfaces configured yet, skipping start");
        return Ok(());
    }

    let new_dhcp = format!("{}_new", args.dhcp_config);
    let has_new_dhcp = tokio::fs::try_exists(&new_dhcp).await?;
    let has_new_host = if let Some(host_path) = &args.host_config {
        tokio::fs::try_exists(format!("{}_new", host_path)).await?
    } else {
        false
    };

    if !has_new_dhcp && !has_new_host && !force_start {
        tracing::debug!("ReloadConfig: no staged changes, skipping restart");
        return Ok(());
    }

    let candidate_dhcp = if has_new_dhcp {
        &new_dhcp
    } else {
        &args.dhcp_config
    };
    let candidate_host = args.host_config.as_ref().map(|path| {
        if has_new_host {
            format!("{path}_new")
        } else {
            path.clone()
        }
    });
    let config = load_config(args, candidate_dhcp, candidate_host, true)
        .await?
        .with_service_config(service_config);
    let identity = config.server_identifier()?;
    server_identity::persist(&args.dhcp_config, &identity).await?;

    // Both files and the saved identity are ready. Keep the old generation
    // running until promotion succeeds, so rejection does not stop service.
    let mut replacements = Vec::new();
    if has_new_dhcp {
        replacements.push(args.dhcp_config.clone());
    }
    if has_new_host && let Some(path) = &args.host_config {
        replacements.push(path.clone());
    }
    promote_configs(&replacements).await?;

    replace_generation(args, config, cancel_token, dhcp_handle, v6_port).await;
    Ok(())
}

/// Reloads test generations without service reservations so existing reload tests remain focused on
/// file and listener behavior.
#[cfg(test)]
async fn handle_reload(
    args: &Args,
    cancel_token: &mut Option<CancellationToken>,
    dhcp_handle: &mut Option<tokio::task::JoinHandle<()>>,
    force_start: bool,
    v6_port: u16,
) -> Result<(), DhcpError> {
    handle_reload_with_service_config(
        args,
        cancel_token,
        dhcp_handle,
        force_start,
        ServiceConfig::default(),
        v6_port,
    )
    .await
}

/// Restore earlier files if a later promotion fails; packet service still uses
/// its immutable old configuration throughout this operation.
async fn promote_configs(paths: &[String]) -> Result<(), DhcpError> {
    let mut previous = Vec::new();
    for path in paths {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(DhcpError::ConfigFile {
                    path: path.clone(),
                    source: Box::new(error.into()),
                });
            }
        };
        previous.push(bytes);
    }
    for (index, path) in paths.iter().enumerate() {
        if let Err(error) = tokio::fs::rename(format!("{path}_new"), path).await {
            let primary = DhcpError::ConfigFile {
                path: path.clone(),
                source: Box::new(error.into()),
            };
            for (old_path, bytes) in paths[..index].iter().zip(&previous[..index]) {
                let restored = match bytes {
                    Some(bytes) => tokio::fs::write(old_path, bytes).await,
                    None => tokio::fs::remove_file(old_path).await,
                };
                if let Err(restore_error) = restored {
                    return Err(DhcpError::ConfigRestore {
                        primary: Box::new(primary),
                        restore: Box::new(DhcpError::ConfigFile {
                            path: old_path.clone(),
                            source: Box::new(restore_error.into()),
                        }),
                    });
                }
            }
            return Err(primary);
        }
    }
    Ok(())
}

/// Keeps every field from one configuration replacement together while the control loop validates
/// and applies it.
struct ConfigReplacement {
    dhcp_yaml: String,
    host_yaml: Option<String>,
    interfaces: Vec<String>,
    service_config: ServiceConfig,
}

/// Stages one caller-validated replacement and activates its service reservations atomically with
/// the new listener generation.
async fn apply_update_with_service_config(
    args: &mut Args,
    replacement: ConfigReplacement,
    active_service_config: &mut ServiceConfig,
    active_interfaces: &mut Vec<String>,
    cancel_token: &mut Option<CancellationToken>,
    dhcp_handle: &mut Option<tokio::task::JoinHandle<()>>,
    v6_port: u16,
) -> Result<(), ApplyError> {
    let ConfigReplacement {
        dhcp_yaml,
        host_yaml,
        interfaces,
        service_config,
    } = replacement;
    let service_changed = *active_service_config != service_config;
    let mut candidate_args = args.clone();
    candidate_args.interfaces = interfaces;
    candidate_args
        .validate_interfaces()
        .map_err(ApplyError::InvalidConfig)?;
    handle_update_config(&candidate_args, dhcp_yaml, host_yaml).await?;

    // Requests from older Cores can omit listener inventory. Keep their file update staged, but
    // replace a changed service set on the interfaces still owned by the running generation.
    if candidate_args.interfaces.is_empty() {
        if service_changed {
            if dhcp_handle.is_some() {
                let mut generation_args = args.clone();
                generation_args.interfaces.clone_from(active_interfaces);
                let live_dhcp = generation_args.dhcp_config.clone();
                let live_host = generation_args.host_config.clone();
                let config = load_config(&generation_args, &live_dhcp, live_host, true)
                    .await
                    .map_err(ApplyError::Internal)?
                    .with_service_config(service_config.clone());
                replace_generation(&generation_args, config, cancel_token, dhcp_handle, v6_port)
                    .await;
            }
            *active_service_config = service_config;
        }
        *args = candidate_args;
        return Ok(());
    }

    // Service reservations live only in immutable generation config, so a service-only replacement must force restart.
    let force =
        dhcp_handle.is_none() || args.interfaces != candidate_args.interfaces || service_changed;
    handle_reload_with_service_config(
        &candidate_args,
        cancel_token,
        dhcp_handle,
        force,
        service_config.clone(),
        v6_port,
    )
    .await
    .map_err(|error| match error {
        DhcpError::InvalidDhcpV6Lifetimes { .. } => ApplyError::InvalidConfig(error),
        error => ApplyError::Internal(error),
    })?;
    active_interfaces.clone_from(&candidate_args.interfaces);
    *active_service_config = service_config;
    *args = candidate_args;
    Ok(())
}

/// Applies test updates with an empty service replacement so established control-loop tests retain
/// their original scope.
#[cfg(test)]
async fn apply_update(
    args: &mut Args,
    dhcp_yaml: String,
    host_yaml: Option<String>,
    interfaces: Vec<String>,
    cancel_token: &mut Option<CancellationToken>,
    dhcp_handle: &mut Option<tokio::task::JoinHandle<()>>,
    v6_port: u16,
) -> Result<(), ApplyError> {
    let mut active_service_config = ServiceConfig::default();
    let mut active_interfaces = args.interfaces.clone();
    apply_update_with_service_config(
        args,
        ConfigReplacement {
            dhcp_yaml,
            host_yaml,
            interfaces,
            service_config: ServiceConfig::default(),
        },
        &mut active_service_config,
        &mut active_interfaces,
        cancel_token,
        dhcp_handle,
        v6_port,
    )
    .await
}

/// Runs the DHCP server under gRPC control.
///
/// Spawns the gRPC server as a background task, then enters the main control
/// loop. The DHCP server starts immediately when valid configuration and
/// interfaces are available. An update can supply a missing configuration or
/// replace damaged YAML when a valid saved identity exists. Without that saved
/// identity, damaged live YAML prevents control startup too: we cannot recover
/// the existing DUID safely from a file we cannot read.
async fn run_with_grpc_control(
    mut args: Args,
    grpc_listen_addr: SocketAddr,
    v6_port: u16,
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

    if tokio::fs::try_exists(&args.dhcp_config).await? {
        pin_server_identity(&args).await?;
    }

    // Channel through which the gRPC handlers deliver control requests.
    // Four queued requests plus one active request bound accepted work. Update
    // admission is nonblocking; an accepted update waits for its apply result.
    let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::channel::<ControlRequest>(4);
    let retained_host_config_path = if matches!(args.mode, ServerMode::Dpu) {
        args.host_config.clone()
    } else {
        None
    };

    tokio::spawn(async move {
        run_grpc_server(grpc_listen_addr, ctrl_tx, retained_host_config_path).await;
    });

    run_control_loop(args, ctrl_rx, v6_port).await
}

/// Own the running generation and serialize accepted control requests.
/// An abandoned queued update has no effects; an application already underway
/// finishes even if its caller stops waiting.
async fn run_control_loop(
    mut args: Args,
    mut ctrl_rx: tokio::sync::mpsc::Receiver<ControlRequest>,
    v6_port: u16,
) -> Result<(), Box<dyn Error>> {
    // Both `cancel_token` and `dhcp_handle` are Option so the select! arm
    // that watches the handle pends forever while the server is not yet running.
    let mut cancel_token: Option<CancellationToken> = None;
    let mut dhcp_handle: Option<tokio::task::JoinHandle<()>> = None;
    let mut active_service_config = ServiceConfig::default();
    let mut active_interfaces = Vec::new();

    if tokio::fs::try_exists(&args.dhcp_config)
        .await
        .unwrap_or(false)
        && !args.interfaces.is_empty()
    {
        tracing::info!("Config file and interfaces found at startup – starting DHCP server");
        match init(args.clone()).await {
            Ok(config) => {
                let ct = CancellationToken::new();
                dhcp_handle = Some(tokio::spawn(run_dhcp_generation(
                    args.clone(),
                    ct.clone(),
                    config,
                    v6_port,
                )));
                cancel_token = Some(ct);
                active_interfaces.clone_from(&args.interfaces);
            }
            Err(error) => tracing::error!(
                %error,
                "Could not load DHCP startup configuration; waiting for an update"
            ),
        }
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
                        if let Err(error) = h.await {
                            tracing::warn!(%error, "DHCP generation failed during shutdown");
                        }
                    }
                    return Ok(());
                };

                match msg {
                    ControlRequest::UpdateAndReload {
                        dhcp_yaml,
                        host_yaml,
                        interfaces,
                        service_config,
                        applied,
                    } => {
                        // Skip abandoned queued work. Once apply begins, finish it
                        // even if the caller's deadline expires midway through.
                        if applied.is_closed() {
                            continue;
                        }
                        let result = apply_update_with_service_config(
                            &mut args,
                            ConfigReplacement {
                                dhcp_yaml,
                                host_yaml,
                                interfaces,
                                service_config,
                            },
                            &mut active_service_config,
                            &mut active_interfaces,
                            &mut cancel_token, &mut dhcp_handle, v6_port,
                        ).await;
                        if let Err(error) = &result {
                            tracing::error!(%error, "DHCP config update failed");
                        }
                        applied.send(result).ok();
                    }
                    ControlRequest::Stop => {
                        if let (Some(ct), Some(h)) = (cancel_token.take(), dhcp_handle.take()) {
                            tracing::info!("StopServer: stopping DHCP server");
                            ct.cancel();
                            if let Err(error) = h.await {
                                tracing::warn!(%error, "DHCP generation failed while stopping");
                            }
                            tracing::info!("StopServer: DHCP server stopped; gRPC server remains up");
                        } else {
                            tracing::info!("StopServer: DHCP server was not running");
                        }
                        active_service_config = ServiceConfig::default();
                        active_interfaces.clear();
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let tracing = setup_tracing()?;

    let args = Args::load();

    // Empty interfaces defer listener selection, including in preflight mode.
    args.validate_interfaces()?;

    if let Some(candidate) = &args.validate_config {
        load_config(&args, candidate, args.host_config.clone(), false).await?;
        return Ok(());
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

    let mut result: Result<(), Box<dyn Error>> = Ok(());
    if let Some(ref addr_str) = args.grpc_listen_addr {
        let grpc_listen_addr: SocketAddr = addr_str
            .parse()
            .map_err(|e| format!("Invalid --grpc-listen-addr '{}': {}", addr_str, e))?;
        result = run_with_grpc_control(args, grpc_listen_addr, dhcproto::v6::SERVER_PORT).await;
    } else {
        pin_server_identity(&args).await?;
        let config = init(args.clone()).await?;
        run_dhcp_generation(
            args,
            CancellationToken::new(),
            config,
            dhcproto::v6::SERVER_PORT,
        )
        .await;
    }

    // Send the spans the exporter has not batched yet, including on a failed run.
    tracing.shutdown().await;

    result
}

fn get_mode(args_mode: &ServerMode) -> Box<dyn DhcpMode> {
    match args_mode {
        ServerMode::Dpu => Box::new(Dpu {}),
        ServerMode::Controller => Box::new(Controller {}),
    }
}

async fn init(args: Args) -> Result<Config, DhcpError> {
    load_config(&args, &args.dhcp_config, args.host_config.clone(), false).await
}

async fn read_dhcp_config(live_path: &str, candidate_path: &str) -> Result<DhcpConfig, DhcpError> {
    let file_error = |source| DhcpError::ConfigFile {
        path: candidate_path.to_string(),
        source: Box::new(source),
    };
    let yaml = tokio::fs::read_to_string(candidate_path)
        .await
        .map_err(|error| file_error(error.into()))?;
    let mut config: DhcpConfig =
        serde_yaml::from_str(&yaml).map_err(|error| file_error(error.into()))?;
    config.dhcpv6_server_id = Some(server_identity::resolve(live_path, &config).await.map_err(
        |error| match error {
            DhcpError::Config(_) => file_error(error),
            error => error,
        },
    )?);
    config
        .validate()
        .map_err(|error| file_error(error.into()))?;
    Ok(config)
}

async fn pin_server_identity(args: &Args) -> Result<(), DhcpError> {
    let identity = match server_identity::read_persisted(&args.dhcp_config).await? {
        Some(identity) => identity,
        None => read_dhcp_config(&args.dhcp_config, &args.dhcp_config)
            .await?
            .server_identifier()?,
    };
    server_identity::persist(&args.dhcp_config, &identity).await
}

async fn load_config(
    args: &Args,
    candidate_dhcp: &str,
    candidate_host: Option<String>,
    allow_missing_host: bool,
) -> Result<Config, DhcpError> {
    let forge_client_config = forge_client_config(args)?;
    let dhcp_config = read_dhcp_config(&args.dhcp_config, candidate_dhcp).await?;

    let host_config;
    if let ServerMode::Dpu = args.mode {
        let host_path = candidate_host.clone();
        host_config = match get_host_config(candidate_host).await {
            Ok(host_config) => host_config,
            // Controlled replacements may omit host.yaml because service reservations are served
            // independently, while unmatched DPU requests fail closed without host data.
            Err(DhcpError::MissingArgument(_)) if allow_missing_host => None,
            Err(DhcpError::IoError(error))
                if allow_missing_host && error.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            Err(error) => {
                return Err(match host_path {
                    Some(path) => DhcpError::ConfigFile {
                        path,
                        source: Box::new(error),
                    },
                    None => error,
                });
            }
        };
    } else {
        host_config = None;
    };

    validate_stateful_lifetimes_for_host(&dhcp_config, host_config.as_ref())?;
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

// Roots a trace per packet: a DHCP client sends no trace context to continue.
#[tracing::instrument(skip_all, fields(source_address = %addr, circuit_id = %circuit_id))]
#[allow(clippy::too_many_arguments)]
async fn process(
    addr: SocketAddr,
    socket: Arc<UdpSocket>,
    buf: &[u8],
    config: Arc<Config>,
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
    let records_host_activity = packet.records_host_activity();
    let should_record = match packet.send(dest_address, socket).await {
        Ok(ReplyStatus::Sent) => records_host_activity,
        Ok(ReplyStatus::Retired | ReplyStatus::NoReply) => false,
        Err(err) => {
            emit(DhcpPacketDropped {
                reason: DropReason::SendFailed,
                error: err,
            });
            records_host_activity
        }
    };
    if should_record {
        record_dhcp_timestamp(&config, dhcp_timestamps).await;
    }
}

/// Process one DHCPv6 datagram and send its response to the exact UDP source.
// Roots a trace per packet, as the v4 path does.
#[tracing::instrument(skip_all, fields(source_address = %source, circuit_id = %context.interface))]
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
    let should_record = match response.send(&context.socket, source).await {
        Ok(ReplyStatus::Sent) => true,
        Ok(ReplyStatus::Retired | ReplyStatus::NoReply) => false,
        Err(error) => {
            emit(DhcpV6RequestDropped {
                reason: V6DropReason::SendFailed,
                error: error.to_string(),
            });
            true
        }
    };
    if should_record {
        record_dhcp_timestamp(&context.config, context.dhcp_timestamps.clone()).await;
    }
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
    use carbide_dhcp_server::modes::dpu::Dpu;
    use carbide_instrument::testing::{MetricsCapture, capture_logs_async};
    use carbide_rpc_utils::dhcp::{
        DhcpTimestamps, DhcpTimestampsFilePath, ServiceConfig, ServiceInterfaceReservations,
        ServiceLink, ServiceLinkV4, ServiceReservation,
    };
    use carbide_test_support::value_scenarios;
    use chrono::{DateTime, Utc};
    use dhcproto::v4::{DhcpOption, Message, MessageType, OptionCode};
    use dhcproto::v6::MessageType as MessageTypeV6;
    use dhcproto::{Decodable, Decoder, Encodable};
    use lru::LruCache;
    use rpc::forge::{DhcpDiscovery, DhcpRecord};
    use tempfile::TempDir;
    use tokio::net::UdpSocket;
    use tokio::sync::{Mutex, Notify, oneshot};
    use tokio::task::JoinSet;
    use tokio::time::{Duration, timeout};
    use tokio_util::sync::CancellationToken;
    use tonic::async_trait;

    use crate::cache::CacheEntry;
    use crate::command_line::{Args, ServerMode};
    use crate::{
        Config, DhcpMode, ListenerFailure, ReplyStatus, admit_v6_packet, cache,
        forge_client_config, handle_reload, init, packet_handler, process,
        supervise_listener_tasks, validate_v6_source_port,
    };

    const TEST_SOURCE_ADDRESS: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 68));
    const TEST_CLIENT_MAC: &[u8] = &[0x00, 0x1b, 0x63, 0x84, 0x45, 0xe6];
    const TEST_CLIENT_MAC_TEXT: &str = "00:1b:63:84:45:e6";

    /// Serializes tests that intentionally mutate the fixed test timestamp path.
    static TEST_TIMESTAMP_FILE_LOCK: Mutex<()> = Mutex::const_new(());

    /// Encodes one relayed service DISCOVER so live-listener tests can receive replies on an
    /// unprivileged loopback port.
    fn encode_service_discover(mac_address: [u8; 6], transaction_id: u32) -> Vec<u8> {
        let mut request = Message::new(
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::LOCALHOST,
            &mac_address,
        );
        request.set_xid(transaction_id);
        request
            .opts_mut()
            .insert(DhcpOption::MessageType(MessageType::Discover));
        let mut encoded = Vec::new();
        request
            .encode(&mut dhcproto::Encoder::new(&mut encoded))
            .unwrap();
        encoded
    }

    /// Signals each real IPv4 listener bind so replacement tests do not mistake startup latency for
    /// service silence.
    struct Ipv4ListenerReady {
        ready: Arc<Notify>,
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Ipv4ListenerReady {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            event.record(
                &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                    if field.name() == "message" && format!("{value:?}") == "DHCP server listening"
                    {
                        self.ready.notify_one();
                    }
                },
            );
        }
    }

    /// Per-packet span fields must identify the receiving interface even when validation drops
    /// the packet before DHCP decoding.
    #[tokio::test]
    async fn ipv6_packet_span_identifies_source_and_interface() {
        use tracing::instrument::WithSubscriber;
        use tracing_subscriber::prelude::*;

        struct PacketSpan(Arc<std::sync::Mutex<Vec<(String, String)>>>);

        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for PacketSpan {
            fn on_new_span(
                &self,
                attributes: &tracing::span::Attributes<'_>,
                _: &tracing::span::Id,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if attributes.metadata().name() == "process_v6" {
                    let mut fields = self.0.lock().unwrap();
                    attributes.record(
                        &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                            fields.push((field.name().to_string(), format!("{value:?}")));
                        },
                    );
                }
            }
        }

        carbide_instrument::testing::keep_callsites_enabled();
        let fields = Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(PacketSpan(fields.clone()));
        let context = super::V6ListenerContext {
            socket: Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap()),
            config: Arc::new(init(get_test_args()).await.unwrap()),
            handler: Arc::new(Box::new(Test {})),
            interface: "vlan200".to_string(),
            machine_cache: Arc::new(Mutex::new(LruCache::new(
                std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
            ))),
            dhcp_timestamps: Arc::new(Mutex::new(DhcpTimestamps::new(
                DhcpTimestampsFilePath::NotSet,
            ))),
        };
        let source = "[2001:db8::1]:547".parse().unwrap();
        // A direct SOLICIT from the server port is rejected before decoding or sending a reply.
        super::process_v6(source, &[u8::from(MessageTypeV6::Solicit)], context)
            .with_subscriber(subscriber)
            .await;

        let fields = fields.lock().unwrap();
        assert!(fields.contains(&("source_address".to_string(), source.to_string())));
        assert!(fields.contains(&("circuit_id".to_string(), "vlan200".to_string())));
    }

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
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            relay_response_port: 67,
            dhcp_config: td.path().join("dhcp.yaml").display().to_string(),
            host_config: None,
            forge_root_ca_path: None,
            client_cert_path: None,
            client_key_path: None,
            mode: ServerMode::Controller,
            grpc_listen_addr: None,
            metrics_listen_addr: None,
            validate_config: None,
        }
    }

    fn ipv6_only_config() -> carbide_rpc_utils::dhcp::DhcpConfig {
        carbide_rpc_utils::dhcp::DhcpConfig {
            dhcpv6_server_id: Some(
                carbide_rpc_utils::dhcp::DhcpV6ServerId::from_remote_id("test-dpu").unwrap(),
            ),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn ipv6_only_supervision_fails_when_its_last_listener_exits() {
        let cancel = CancellationToken::new();
        let mut v6_tasks = JoinSet::new();
        let (first_exit, first_exited) = oneshot::channel();
        v6_tasks.spawn(async move {
            first_exit.send(()).unwrap();
        });
        first_exited.await.unwrap();
        let (release, released) = oneshot::channel();
        v6_tasks.spawn(async move {
            released.await.unwrap();
        });
        let (result, logs) = capture_logs_async(async {
            let supervision = supervise_listener_tasks(JoinSet::new(), v6_tasks, cancel.clone());
            tokio::pin!(supervision);
            assert!(
                timeout(Duration::from_millis(100), &mut supervision)
                    .await
                    .is_err()
            );
            assert!(
                !cancel.is_cancelled(),
                "the second listener is still healthy"
            );
            release.send(()).unwrap();
            timeout(Duration::from_secs(1), supervision).await.unwrap()
        })
        .await;
        assert!(matches!(result, Err(ListenerFailure::Returned)));
        assert!(cancel.is_cancelled());
        assert!(
            logs.iter().any(|entry| {
                entry.message == "DHCPv6 listener exited unexpectedly"
                    && entry.field("remaining_v6_listener_count") == Some("1")
            }),
            "supervision must observe the first exit before the last listener exits"
        );
    }

    /// Exercise real socket creation for both configurations: IPv6-only must
    /// omit DHCPv4, while adding the IPv4 pair must still start its listener.
    #[test]
    fn generation_binds_only_the_configured_address_families() {
        use tracing_subscriber::layer::SubscriberExt;

        struct ListenerReady {
            ipv6: Arc<tokio::sync::Notify>,
            ipv4: Arc<std::sync::atomic::AtomicBool>,
        }
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ListenerReady {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                event.record(
                    &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                        if field.name() == "message" {
                            match format!("{value:?}").as_str() {
                                "DHCPv6 server listening" => self.ipv6.notify_one(),
                                "DHCP server listening" => {
                                    self.ipv4.store(true, std::sync::atomic::Ordering::Relaxed)
                                }
                                _ => {}
                            }
                        }
                    },
                );
            }
        }

        for ipv4_enabled in [false, true] {
            // Keep the subscriber on this runtime's only thread so listener-task
            // logs prove the real socket was established, not merely spawned.
            let ready = Arc::new(tokio::sync::Notify::new());
            let ipv4_bound = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let subscriber = tracing_subscriber::registry().with(ListenerReady {
                ipv6: ready.clone(),
                ipv4: ipv4_bound.clone(),
            });
            tracing::subscriber::with_default(subscriber, || {
                tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let directory = TempDir::new().unwrap();
                    let args = make_reload_args(&directory, vec!["lo".to_string()]);
                    let mut dhcp = ipv6_only_config();
                    if ipv4_enabled {
                        dhcp.carbide_dhcp_server = Some(Ipv4Addr::LOCALHOST);
                        dhcp.carbide_provisioning_server_ipv4 = Some(Ipv4Addr::LOCALHOST);
                    }
                    let config = Config::new(
                        dhcp,
                        None,
                        67,
                        forge_client_config(&args).unwrap(),
                    );
                    let cancel = CancellationToken::new();
                    // Port zero isolates the socket from other test binaries.
                    let generation = super::run_dhcp_generation(args, cancel.clone(), config, 0);
                    tokio::pin!(generation);
                    tokio::select! {
                        () = &mut generation => panic!("generation exited before its IPv6 listener bound"),
                        result = timeout(Duration::from_secs(5), ready.notified()) => {
                            result.expect("IPv6 listener did not become ready");
                        }
                    }
                    cancel.cancel();
                    timeout(Duration::from_secs(1), generation).await.unwrap();
                });
            });
            // Cancellation joins every listener. A mistakenly spawned v4 task
            // would still bind and log before observing cancellation, so it cannot
            // escape this assertion just by being scheduled after v6 readiness.
            assert_eq!(
                ipv4_bound.load(std::sync::atomic::Ordering::Relaxed),
                ipv4_enabled,
            );
        }
    }

    /// Verifies a retired generation skips IPv4 socket retries so replacement is not stalled by an
    /// unavailable interface.
    #[tokio::test]
    async fn cancelled_generation_does_not_wait_for_ipv4_socket_retries() {
        let directory = TempDir::new().unwrap();
        // An impossible Linux interface name would otherwise enter the two-second retry delay.
        let args = make_reload_args(&directory, vec!["missing/test".to_string()]);
        let mut dhcp = ipv6_only_config();
        dhcp.carbide_dhcp_server = Some(Ipv4Addr::LOCALHOST);
        dhcp.carbide_provisioning_server_ipv4 = Some(Ipv4Addr::LOCALHOST);
        let config = Config::new(dhcp, None, 67, forge_client_config(&args).unwrap());

        // Cancel before startup so socket setup and retirement are simultaneously eligible.
        let cancel = CancellationToken::new();
        cancel.cancel();

        // Retirement must complete before the first socket retry delay.
        timeout(
            Duration::from_secs(1),
            super::run_dhcp_generation(args, cancel, config, 0),
        )
        .await
        .expect("cancelled generation waited for IPv4 socket retries");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn failed_apply_retains_live_files_and_generation() {
        let empty_host =
            "host_interface_id: 11111111-1111-1111-1111-111111111111\nhost_ip_addresses: {}\n";
        let stateful_host = "host_interface_id: 11111111-1111-1111-1111-111111111111\nhost_ip_addresses:\n  lo:\n    fqdn: host.example.com\n    ipv6:\n      address: '2001:db8::2'\n      prefix: '2001:db8::/64'\n";
        let replacement_host =
            "host_interface_id: 22222222-2222-2222-2222-222222222222\nhost_ip_addresses: {}\n";
        for (candidate_host, blocked_path) in [
            ("invalid host YAML", None),
            // The DHCP candidate has zero lifetimes, which cannot serve this binding.
            (stateful_host, None),
            (empty_host, Some("dhcp.yaml.duid")),
            // Reject the host write after staging the DHCP replacement.
            (replacement_host, Some("host.yaml_new")),
        ] {
            let directory = TempDir::new().unwrap();
            let mut args = make_reload_args(&directory, vec!["old-interface".to_string()]);
            args.mode = ServerMode::Dpu;
            args.host_config = Some(directory.path().join("host.yaml").display().to_string());
            let live = serde_yaml::to_string(&ipv6_only_config()).unwrap();
            tokio::fs::write(&args.dhcp_config, &live).await.unwrap();
            tokio::fs::write(args.host_config.as_ref().unwrap(), empty_host)
                .await
                .unwrap();
            if let Some(path) = blocked_path {
                tokio::fs::create_dir(directory.path().join(path))
                    .await
                    .unwrap();
            }
            let cancel = CancellationToken::new();
            let running_cancel = cancel.clone();
            let mut token = Some(cancel.clone());
            let mut handle = Some(tokio::spawn(
                async move { running_cancel.cancelled().await },
            ));
            let mut candidate = ipv6_only_config();
            candidate.lease_time_secs = 1200;
            let candidate_yaml = serde_yaml::to_string(&candidate).unwrap();
            let result = super::apply_update(
                &mut args,
                candidate_yaml.clone(),
                Some(candidate_host.to_string()),
                vec!["lo".to_string()],
                &mut token,
                &mut handle,
                0,
            )
            .await;
            let error = result.unwrap_err();
            if candidate_host == stateful_host {
                assert!(matches!(
                    error,
                    super::ApplyError::InvalidConfig(DhcpError::InvalidDhcpV6Lifetimes { .. })
                ));
            } else if let Some(blocked) = blocked_path {
                let super::ApplyError::Internal(DhcpError::ConfigFile { path, source }) = &error
                else {
                    panic!("expected a storage error, got {error}");
                };
                assert_eq!(std::path::Path::new(path), directory.path().join(blocked));
                assert!(matches!(
                    source.as_ref(),
                    DhcpError::IoError(error) if error.kind() == std::io::ErrorKind::IsADirectory
                ));
            } else {
                assert!(matches!(
                    error,
                    super::ApplyError::InvalidConfig(DhcpError::SerdeYaml(_))
                ));
                for path in [&args.dhcp_config, args.host_config.as_ref().unwrap()] {
                    assert!(
                        !tokio::fs::try_exists(format!("{path}_new")).await.unwrap(),
                        "invalid host YAML must not stage {path}"
                    );
                }
            }
            assert!(!cancel.is_cancelled());
            assert!(!handle.as_ref().unwrap().is_finished());
            assert_eq!(args.interfaces, ["old-interface"]);
            if blocked_path != Some("dhcp.yaml.duid") {
                assert!(
                    !tokio::fs::try_exists(super::server_identity::path(&args.dhcp_config))
                        .await
                        .unwrap()
                );
            }
            assert_eq!(
                tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
                live
            );
            assert_eq!(
                tokio::fs::read_to_string(args.host_config.as_ref().unwrap())
                    .await
                    .unwrap(),
                empty_host
            );

            if blocked_path == Some("host.yaml_new") {
                assert_eq!(
                    tokio::fs::read_to_string(format!("{}_new", args.dhcp_config))
                        .await
                        .unwrap(),
                    candidate_yaml
                );
                tokio::fs::remove_dir(directory.path().join("host.yaml_new"))
                    .await
                    .unwrap();
                // Repair only the filesystem obstacle. The identical request
                // must apply on retry and replace the old running generation.
                super::apply_update(
                    &mut args,
                    candidate_yaml.clone(),
                    Some(candidate_host.to_string()),
                    vec!["lo".to_string()],
                    &mut token,
                    &mut handle,
                    0,
                )
                .await
                .unwrap();
                // Stop the replacement immediately because this retry case proves publication without needing live listeners.
                let replacement = handle.take().unwrap();
                replacement.abort();
                assert!(replacement.await.unwrap_err().is_cancelled());
                assert!(cancel.is_cancelled());
                assert_eq!(
                    tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
                    candidate_yaml
                );
                assert_eq!(
                    tokio::fs::read_to_string(args.host_config.as_ref().unwrap())
                        .await
                        .unwrap(),
                    candidate_host
                );
                assert_eq!(args.interfaces, ["lo"]);
            }
            token.unwrap().cancel();
            if let Some(original) = handle {
                original.await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn failed_second_promotion_restores_the_first_live_file() {
        let directory = TempDir::new().unwrap();
        let dhcp = directory.path().join("dhcp.yaml").display().to_string();
        let host = directory.path().join("host.yaml").display().to_string();
        tokio::fs::write(&dhcp, b"original DHCP").await.unwrap();
        tokio::fs::write(&host, b"original host").await.unwrap();
        tokio::fs::write(format!("{dhcp}_new"), b"replacement DHCP")
            .await
            .unwrap();

        // The first rename succeeds; the second has no staged source.
        let error = super::promote_configs(&[dhcp.clone(), host.clone()])
            .await
            .unwrap_err();
        assert!(matches!(&error, DhcpError::ConfigFile { path, source }
            if path == &host && matches!(source.as_ref(), DhcpError::IoError(error)
                if error.kind() == std::io::ErrorKind::NotFound)));
        assert!(!tokio::fs::try_exists(format!("{dhcp}_new")).await.unwrap());
        assert_eq!(tokio::fs::read(&dhcp).await.unwrap(), b"original DHCP");
        assert_eq!(tokio::fs::read(&host).await.unwrap(), b"original host");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn applied_ipv6_only_update_preserves_legacy_identity_on_disk() {
        use dhcproto::v6::{
            DhcpOption as V6Option, IANA, Message as V6Message, OptionCode as V6Code,
        };

        let directory = TempDir::new().unwrap();
        let mut args = make_reload_args(&directory, vec!["lo".to_string()]);
        args.mode = ServerMode::Dpu;
        args.host_config = Some(directory.path().join("host.yaml").display().to_string());
        let legacy = carbide_rpc_utils::dhcp::DhcpConfig {
            carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
            carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
            ..Default::default()
        };
        tokio::fs::write(&args.dhcp_config, serde_yaml::to_string(&legacy).unwrap())
            .await
            .unwrap();
        let (mut token, mut handle) = (None, None);
        let mut candidate = ipv6_only_config();
        candidate.dhcpv6_preferred_lifetime_secs = 300;
        candidate.dhcpv6_valid_lifetime_secs = 600;
        let host = "host_interface_id: 11111111-1111-1111-1111-111111111111\nhost_ip_addresses:\n  lo:\n    fqdn: host.example.com\n    ipv6:\n      address: '2001:db8::2'\n      prefix: '2001:db8::/64'\n";
        super::apply_update(
            &mut args,
            serde_yaml::to_string(&candidate).unwrap(),
            Some(host.to_string()),
            vec!["lo".to_string()],
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();
        // Stop the generation because persistence and packet reconstruction below do not require live listener sockets.
        let generation = handle.unwrap();
        generation.abort();
        token.unwrap().cancel();
        assert!(generation.await.unwrap_err().is_cancelled());
        let applied: carbide_rpc_utils::dhcp::DhcpConfig =
            serde_yaml::from_str(&tokio::fs::read_to_string(&args.dhcp_config).await.unwrap())
                .unwrap();
        assert!(applied.ipv4().unwrap().is_none());
        assert_eq!(
            applied.server_identifier().unwrap(),
            legacy.server_identifier().unwrap()
        );
        assert_eq!(
            tokio::fs::read(super::server_identity::path(&args.dhcp_config))
                .await
                .unwrap(),
            legacy.server_identifier().unwrap().as_bytes(),
        );
        assert_eq!(
            tokio::fs::read_to_string(args.host_config.as_ref().unwrap())
                .await
                .unwrap(),
            host
        );

        // Reopen the promoted files through the real DPU mode, so neither the
        // selected address nor the reply identity comes from an in-memory fixture.
        let config = init(args).await.unwrap();
        let mut request = V6Message::new(MessageTypeV6::Solicit);
        request.opts_mut().insert(V6Option::ClientId(
            [vec![0, 3, 0, 1], TEST_CLIENT_MAC.to_vec()].concat(),
        ));
        request.opts_mut().insert(V6Option::IANA(IANA {
            id: 1,
            t1: 0,
            t2: 0,
            opts: Default::default(),
        }));
        let mut cache = Arc::new(Mutex::new(LruCache::new(1.try_into().unwrap())));
        let packet = super::packet_handler_v6::process_packet(
            &request.to_vec().unwrap(),
            Ipv6Addr::LOCALHOST,
            &config,
            "lo",
            &carbide_dhcp_server::modes::dpu::Dpu {},
            &mut cache,
        )
        .await
        .unwrap()
        .unwrap();
        let response = V6Message::decode(&mut Decoder::new(packet.encoded_packet())).unwrap();
        assert_eq!(response.msg_type(), MessageTypeV6::Advertise);
        assert_eq!(
            response.opts().get(V6Code::ServerId),
            Some(&V6Option::ServerId(
                legacy.server_identifier().unwrap().as_bytes().to_vec()
            ))
        );
        let Some(V6Option::IANA(association)) = response.opts().get(V6Code::IANA) else {
            panic!("missing IA_NA");
        };
        let Some(V6Option::IAAddr(binding)) = association.opts.get(V6Code::IAAddr) else {
            panic!("missing assigned address");
        };
        assert_eq!(binding.addr, "2001:db8::2".parse::<Ipv6Addr>().unwrap());
    }

    #[tokio::test]
    async fn loaded_server_advertises_saved_identity_over_the_yaml_identity() {
        use carbide_rpc_utils::dhcp::DhcpV6ServerId;
        use dhcproto::v6::{
            DhcpOption as V6Option, IANA, Message as V6Message, OptionCode as V6Code,
        };

        let directory = TempDir::new().unwrap();
        let args = make_reload_args(&directory, vec!["lo".to_string()]);
        let saved =
            DhcpV6ServerId::try_from(b"\x00\x02\x00\x00\x16\x47\xc0\x00\x02\x01".to_vec()).unwrap();
        let mut candidate = ipv6_only_config();
        candidate.dhcpv6_preferred_lifetime_secs = 300;
        candidate.dhcpv6_valid_lifetime_secs = 600;
        assert_ne!(candidate.server_identifier().unwrap(), saved);
        let yaml = serde_yaml::to_string(&candidate).unwrap();
        tokio::fs::write(&args.dhcp_config, &yaml).await.unwrap();
        let saved_path = super::server_identity::path(&args.dhcp_config);
        tokio::fs::write(&saved_path, saved.as_bytes())
            .await
            .unwrap();
        let config = init(args.clone()).await.unwrap();

        let mut request = V6Message::new(MessageTypeV6::Solicit);
        request.opts_mut().insert(V6Option::ClientId(
            [vec![0, 3, 0, 1], TEST_CLIENT_MAC.to_vec()].concat(),
        ));
        request.opts_mut().insert(V6Option::IANA(IANA {
            id: 1,
            t1: 0,
            t2: 0,
            opts: Default::default(),
        }));
        let mut cache = Arc::new(Mutex::new(LruCache::new(1.try_into().unwrap())));
        let packet = super::packet_handler_v6::process_packet(
            &request.to_vec().unwrap(),
            Ipv6Addr::LOCALHOST,
            &config,
            "lo",
            &Test {},
            &mut cache,
        )
        .await
        .unwrap()
        .unwrap();
        let response = V6Message::decode(&mut Decoder::new(packet.encoded_packet())).unwrap();
        assert_eq!(response.msg_type(), MessageTypeV6::Advertise);
        assert_eq!(
            response.opts().get(V6Code::ServerId),
            Some(&V6Option::ServerId(saved.as_bytes().to_vec()))
        );
        assert_eq!(
            tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
            yaml
        );
        assert_eq!(
            tokio::fs::read(&saved_path).await.unwrap(),
            saved.as_bytes()
        );
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

    /// Verifies both forms of last-listener v4 completion cancel and await sibling listener cleanup
    /// before failure returns.
    #[tokio::test]
    async fn listener_supervision_fails_when_the_last_v4_listener_exits() {
        for (scenario, should_panic) in [
            // A normal return reports listener loss without a task failure.
            ("normal return", false),
            // A panic reports listener loss through Tokio's JoinError path.
            ("panic", true),
        ] {
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

            // Model the production v6 listener by completing its cleanup only after generation cancellation.
            let mut v6_tasks = JoinSet::new();
            let v6_cancel = cancel_token.clone();
            let (v6_cleanup_tx, mut v6_cleanup_rx) = oneshot::channel();
            v6_tasks.spawn(async move {
                v6_cancel.cancelled().await;
                v6_cleanup_tx.send(()).unwrap();
            });

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
            v6_cleanup_rx
                .try_recv()
                .expect("supervision must join v6 listener cleanup before returning");
            match (should_panic, result) {
                (false, Err(ListenerFailure::Returned)) => {}
                (true, Err(ListenerFailure::Join(_))) => {}
                (_, other) => {
                    panic!("unexpected v4 supervision result for {scenario}: {other:?}")
                }
            }
        }
    }

    /// Verifies one failed v4 interface preserves healthy siblings until the last one exits so a
    /// partial interface failure cannot interrupt remaining service.
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

            // Model the production v6 listener, which exits through cancellation cleanup after the last v4 listener fails.
            let mut v6_tasks = JoinSet::new();
            let v6_cancel = cancel_token.clone();
            v6_tasks.spawn(async move {
                v6_cancel.cancelled().await;
            });

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

                // Release the final healthy v4 listener so supervision must cancel and fail the generation.
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
                matches!(result, Err(ListenerFailure::Returned)),
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

        let (mut cancel_token, mut dhcp_handle) = (None, None);
        handle_reload(&args, &mut cancel_token, &mut dhcp_handle, false, 0)
            .await
            .unwrap();

        assert!(cancel_token.is_none(), "no server should have been started");
        assert!(dhcp_handle.is_none(), "no server should have been started");
    }

    /// Older Cores can defer listener selection; keep their staged update and live generation.
    #[tokio::test]
    async fn update_with_empty_interfaces_defers_reload() {
        let td = TempDir::new().unwrap();
        let mut args = make_reload_args(&td, vec!["lo".to_string()]);
        let mut config = ipv6_only_config();
        let live = serde_yaml::to_string(&config).unwrap();
        tokio::fs::write(&args.dhcp_config, &live).await.unwrap();
        config.carbide_api_url = Some("https://api.example.com".to_string());
        let candidate = serde_yaml::to_string(&config).unwrap();
        let cancel = CancellationToken::new();
        let running_cancel = cancel.clone();
        let mut token = Some(cancel.clone());
        let mut handle = Some(tokio::spawn(
            async move { running_cancel.cancelled().await },
        ));
        super::apply_update(
            &mut args,
            candidate.clone(),
            None,
            vec![],
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();
        assert!(args.interfaces.is_empty());
        assert!(!cancel.is_cancelled());
        assert!(!handle.as_ref().unwrap().is_finished());
        assert_eq!(
            tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
            live
        );
        assert_eq!(
            tokio::fs::read_to_string(format!("{}_new", args.dhcp_config))
                .await
                .unwrap(),
            candidate
        );
        super::apply_update(
            &mut args,
            candidate.clone(),
            None,
            vec!["lo".to_string()],
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();
        assert!(
            cancel.is_cancelled(),
            "supplying interfaces must apply the deferred replacement"
        );
        assert_eq!(
            tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
            candidate
        );
        assert!(
            !tokio::fs::try_exists(format!("{}_new", args.dhcp_config))
                .await
                .unwrap()
        );
        token.unwrap().cancel();
        handle.unwrap().await.unwrap();
    }

    /// Verifies an empty listener inventory clears active services while retaining remembered
    /// interfaces and deferring DHCP promotion, preventing an omitted inventory from retaining
    /// service access.
    #[tokio::test(flavor = "current_thread")]
    async fn empty_interface_update_clears_active_service_reservations() {
        use tracing_subscriber::layer::SubscriberExt;

        // Serialize the live OFFER producer and observe both real IPv4 listener binds on this runtime thread.
        let _metrics_guard = MetricsCapture::start();
        let listener_ready = Arc::new(Notify::new());
        let subscriber = tracing_subscriber::registry().with(Ipv4ListenerReady {
            ready: Arc::clone(&listener_ready),
        });
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);

        // Start a DPU generation with one active service reservation on an unprivileged loopback socket.
        let directory = TempDir::new().unwrap();
        let listener_reservation = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let SocketAddr::V4(listener_address) = listener_reservation.local_addr().unwrap() else {
            panic!("loopback listener must have an IPv4 address");
        };
        let reply_receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(reply_address) = reply_receiver.local_addr().unwrap() else {
            panic!("reply receiver must bind an IPv4 address");
        };
        let mut args = make_reload_args(&directory, vec!["lo".to_string()]);
        args.mode = ServerMode::Dpu;
        args.listen_addr = listener_address;
        args.relay_response_port = reply_address.port();
        let mut live_config = ipv6_only_config();
        live_config.carbide_dhcp_server = Some(Ipv4Addr::LOCALHOST);
        live_config.carbide_provisioning_server_ipv4 = Some(Ipv4Addr::LOCALHOST);
        let live = serde_yaml::to_string(&live_config).unwrap();
        tokio::fs::write(&args.dhcp_config, &live).await.unwrap();
        // Change only the API URL so staged DHCP data remains distinguishable from the service-only restart.
        live_config.carbide_api_url = Some("https://api.example.com".to_string());
        let candidate = serde_yaml::to_string(&live_config).unwrap();
        let old_cancel = CancellationToken::new();
        let mut token = Some(old_cancel.clone());
        let mut active_service_config =
            ServiceConfig::try_new(vec![ServiceInterfaceReservations {
                receiving_interface: "lo".to_string(),
                reservations: vec![ServiceReservation {
                    attachment_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
                    interface_ordinal: 0,
                    internal_uuid: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
                    mac_address: "02:00:00:00:00:01".parse().unwrap(),
                    link: ServiceLink::Ipv4(ServiceLinkV4 {
                        address: "192.0.2.11".parse().unwrap(),
                        prefix: "192.0.2.10/31".parse().unwrap(),
                        gateway: "192.0.2.10".parse().unwrap(),
                    }),
                }],
            }])
            .unwrap();
        let mut active_interfaces = args.interfaces.clone();
        let old_config = super::load_config(&args, &args.dhcp_config, None, true)
            .await
            .unwrap()
            .with_service_config(active_service_config.clone());
        drop(listener_reservation);
        let mut handle = Some(tokio::spawn(super::run_dhcp_generation(
            args.clone(),
            old_cancel.clone(),
            old_config,
            0,
        )));

        // Wait for the old listener and prove the reservation is active before removing it.
        timeout(Duration::from_secs(5), listener_ready.notified())
            .await
            .expect("initial IPv4 listener did not become ready");
        let reply_sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        reply_sender
            .send_to(
                &encode_service_discover([0x02, 0, 0, 0, 0, 1], 1),
                listener_address,
            )
            .await
            .unwrap();
        let mut response = [0; 1500];
        let (length, _) = timeout(
            Duration::from_secs(1),
            reply_receiver.recv_from(&mut response),
        )
        .await
        .expect("active reservation did not answer")
        .unwrap();
        let response = Message::decode(&mut Decoder::new(&response[..length])).unwrap();
        assert_eq!(response.xid(), 1);
        assert_eq!(response.yiaddr(), "192.0.2.11".parse::<Ipv4Addr>().unwrap());
        assert_eq!(response.opts().msg_type(), Some(MessageType::Offer));

        // Clear services while omitting the next listener inventory, which must not promote the staged file.
        super::apply_update_with_service_config(
            &mut args,
            super::ConfigReplacement {
                dhcp_yaml: candidate.clone(),
                host_yaml: None,
                interfaces: vec![],
                service_config: ServiceConfig::default(),
            },
            &mut active_service_config,
            &mut active_interfaces,
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();

        // Wait for the successor bind so silence below proves removal rather than listener startup latency.
        timeout(Duration::from_secs(5), listener_ready.notified())
            .await
            .expect("successor IPv4 listener did not become ready");

        // The applied state clears services, retains the remembered interface, and leaves the candidate file deferred.
        assert!(old_cancel.is_cancelled());
        assert_eq!(active_service_config, ServiceConfig::default());
        assert_eq!(active_interfaces, ["lo"]);
        assert!(args.interfaces.is_empty());
        assert_eq!(
            tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
            live
        );
        assert_eq!(
            tokio::fs::read_to_string(format!("{}_new", args.dhcp_config))
                .await
                .unwrap(),
            candidate
        );
        assert!(!token.as_ref().unwrap().is_cancelled());

        // Send the removed identity once through the successor and ignore any unrelated queued transaction.
        reply_sender
            .send_to(
                &encode_service_discover([0x02, 0, 0, 0, 0, 1], 2),
                listener_address,
            )
            .await
            .unwrap();
        let removed_response = timeout(Duration::from_millis(250), async {
            loop {
                let mut response = [0; 1500];
                let (length, _) = reply_receiver.recv_from(&mut response).await.unwrap();
                let response = Message::decode(&mut Decoder::new(&response[..length])).unwrap();
                if response.xid() == 2 {
                    break response;
                }
            }
        })
        .await;
        assert!(
            removed_response.is_err(),
            "cleared service reservation unexpectedly received a response"
        );

        // Stop the successor before its temporary files and sockets leave test scope.
        token.unwrap().cancel();
        handle.unwrap().await.unwrap();
    }

    #[tokio::test]
    async fn unchanged_config_discards_staging_but_new_interfaces_restart() {
        let directory = TempDir::new().unwrap();
        let mut args = make_reload_args(&directory, vec!["old-interface".to_string()]);
        args.host_config = Some(directory.path().join("host.yaml").display().to_string());
        let live = serde_yaml::to_string(&ipv6_only_config()).unwrap();
        tokio::fs::write(&args.dhcp_config, &live).await.unwrap();
        for path in [&args.dhcp_config, args.host_config.as_ref().unwrap()] {
            tokio::fs::write(format!("{path}_new"), "rejected candidate")
                .await
                .unwrap();
        }
        let cancel = CancellationToken::new();
        let running_cancel = cancel.clone();
        let mut token = Some(cancel.clone());
        let mut handle = Some(tokio::spawn(
            async move { running_cancel.cancelled().await },
        ));

        // Resubmitting the live config must not accidentally promote leftovers
        // from an earlier rejection, including an omitted host replacement.
        super::apply_update(
            &mut args,
            live.clone(),
            None,
            vec!["old-interface".to_string()],
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();
        assert!(!cancel.is_cancelled());
        for path in [&args.dhcp_config, args.host_config.as_ref().unwrap()] {
            assert!(!tokio::fs::try_exists(format!("{path}_new")).await.unwrap());
        }

        // Configuration equality does not cover a listener interface change.
        super::apply_update(
            &mut args,
            live.clone(),
            None,
            vec!["lo".to_string()],
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();
        assert!(cancel.is_cancelled());
        assert!(!token.as_ref().unwrap().is_cancelled());
        assert_eq!(args.interfaces, ["lo"]);
        assert_eq!(
            tokio::fs::read_to_string(&args.dhcp_config).await.unwrap(),
            live
        );
        token.unwrap().cancel();
        handle.unwrap().await.unwrap();
    }

    /// Verifies production replacement retires a removed client's prepared response while retaining
    /// the survivor, preventing the removed identity from responding after replacement.
    #[tokio::test]
    async fn service_replacement_retires_removed_client_and_keeps_survivor() {
        // Serialize this live OFFER producer with process-global metric-delta assertions in the binary.
        let _metrics_guard = MetricsCapture::start();

        // Reserve loopback transport while keeping files and interfaces unchanged so only service replacement triggers restart.
        let directory = TempDir::new().unwrap();
        let listener_reservation = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let SocketAddr::V4(listener_address) = listener_reservation.local_addr().unwrap() else {
            panic!("loopback listener must have an IPv4 address");
        };
        let reply_receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(reply_address) = reply_receiver.local_addr().unwrap() else {
            panic!("reply receiver must bind an IPv4 address");
        };
        let mut args = make_reload_args(&directory, vec!["lo".to_string()]);
        args.mode = ServerMode::Dpu;
        args.listen_addr = listener_address;
        args.relay_response_port = reply_address.port();
        // Enable IPv4 so the removed reservation can produce a real response before replacement.
        let mut live_config = ipv6_only_config();
        live_config.carbide_dhcp_server = Some(Ipv4Addr::LOCALHOST);
        live_config.carbide_provisioning_server_ipv4 = Some(Ipv4Addr::LOCALHOST);
        let live = serde_yaml::to_string(&live_config).unwrap();
        tokio::fs::write(&args.dhcp_config, &live).await.unwrap();
        let old_cancel = CancellationToken::new();
        let mut token = Some(old_cancel.clone());
        // The first reservation is removed and therefore drives the retirement assertion.
        let removed = ServiceReservation {
            attachment_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
            interface_ordinal: 0,
            internal_uuid: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
            mac_address: TEST_CLIENT_MAC_TEXT.parse().unwrap(),
            link: ServiceLink::Ipv4(ServiceLinkV4 {
                address: "192.0.2.11".parse().unwrap(),
                prefix: "192.0.2.10/31".parse().unwrap(),
                gateway: "192.0.2.10".parse().unwrap(),
            }),
        };
        // The adjacent reservation survives to prove replacement does not clear the whole slot.
        let survivor = ServiceReservation {
            attachment_id: "33333333-3333-3333-3333-333333333333".parse().unwrap(),
            interface_ordinal: 0,
            internal_uuid: "44444444-4444-4444-4444-444444444444".parse().unwrap(),
            mac_address: "02:00:00:00:00:02".parse().unwrap(),
            link: ServiceLink::Ipv4(ServiceLinkV4 {
                address: "192.0.2.13".parse().unwrap(),
                prefix: "192.0.2.12/31".parse().unwrap(),
                gateway: "192.0.2.12".parse().unwrap(),
            }),
        };
        let mut active_service_config =
            ServiceConfig::try_new(vec![ServiceInterfaceReservations {
                receiving_interface: "lo".to_string(),
                reservations: vec![removed.clone(), survivor.clone()],
            }])
            .unwrap();
        let replacement = ServiceConfig::try_new(vec![ServiceInterfaceReservations {
            receiving_interface: "lo".to_string(),
            reservations: vec![survivor.clone()],
        }])
        .unwrap();
        let mut active_interfaces = args.interfaces.clone();
        // Prepare one response from the same immutable snapshot passed to the old generation.
        let old_config = super::load_config(&args, &args.dhcp_config, None, true)
            .await
            .unwrap()
            .with_service_config(active_service_config.clone())
            .with_reply_retirement(old_cancel.clone());
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        let retiring_response = packet_handler::process_packet(
            &get_byte_stream(Ipv4Addr::UNSPECIFIED, None, MessageType::Discover, None),
            TEST_SOURCE_ADDRESS,
            &old_config,
            "lo",
            &Dpu {},
            &mut machine_cache,
        )
        .await
        .unwrap();
        drop(listener_reservation);
        let mut handle = Some(tokio::spawn(super::run_dhcp_generation(
            args.clone(),
            old_cancel.clone(),
            old_config,
            0,
        )));

        // Apply the one-client full replacement through the production update function.
        super::apply_update_with_service_config(
            &mut args,
            super::ConfigReplacement {
                dhcp_yaml: live,
                host_yaml: None,
                interfaces: vec!["lo".to_string()],
                service_config: replacement,
            },
            &mut active_service_config,
            &mut active_interfaces,
            &mut token,
            &mut handle,
            0,
        )
        .await
        .unwrap();

        // The update retires the old generation and records only the survivor in the applied service set.
        assert!(old_cancel.is_cancelled());
        assert!(
            active_service_config
                .get("lo", &removed.mac_address)
                .is_none()
        );
        assert_eq!(
            active_service_config
                .get("lo", &survivor.mac_address)
                .map(|reservation| reservation.internal_uuid),
            Some(survivor.internal_uuid)
        );
        assert!(!token.as_ref().unwrap().is_cancelled());

        // Send through the successor generation so the surviving reservation proves publication rather than only map mutation.
        let reply_sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let survivor_discover = encode_service_discover(survivor.mac_address.bytes(), 1);
        let survivor_offer = timeout(Duration::from_secs(5), async {
            loop {
                reply_sender
                    .send_to(&survivor_discover, listener_address)
                    .await
                    .unwrap();
                let mut buffer = [0; 1500];
                if let Ok(Ok((length, _))) = timeout(
                    Duration::from_millis(50),
                    reply_receiver.recv_from(&mut buffer),
                )
                .await
                {
                    let response = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                    if response.xid() == 1 {
                        break response;
                    }
                }
            }
        })
        .await
        .expect("successor generation did not answer the surviving reservation");
        assert_eq!(
            survivor_offer.yiaddr(),
            "192.0.2.13".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(survivor_offer.opts().msg_type(), Some(MessageType::Offer));

        // Once successor readiness is proven, the removed reservation must remain silent without host fallback.
        let removed_discover = encode_service_discover(removed.mac_address.bytes(), 2);
        reply_sender
            .send_to(&removed_discover, listener_address)
            .await
            .unwrap();
        let removed_response = timeout(Duration::from_millis(100), async {
            loop {
                let mut buffer = [0; 1500];
                let (length, _) = reply_receiver.recv_from(&mut buffer).await.unwrap();
                let response = Message::decode(&mut Decoder::new(&buffer[..length])).unwrap();
                if response.xid() == 2 {
                    break response;
                }
            }
        })
        .await;
        assert!(
            removed_response.is_err(),
            "removed reservation unexpectedly received a response"
        );

        // A real service response prepared from the removed snapshot retains its generation retirement token.
        let sender = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
        let retirement_receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(destination) = retirement_receiver.local_addr().unwrap() else {
            panic!("test receiver must bind an IPv4 address");
        };
        let status = retiring_response.send(destination, sender).await.unwrap();
        assert_eq!(status, ReplyStatus::Retired);
        let mut buffer = [0; 512];
        assert!(
            timeout(
                Duration::from_millis(10),
                retirement_receiver.recv_from(&mut buffer)
            )
            .await
            .is_err()
        );
        // Stop the successor before its temporary files and sockets leave test scope.
        token.unwrap().cancel();
        handle.unwrap().await.unwrap();
    }

    /// Verifies the packet-task drain helper aborts and joins a prepared service response so
    /// listener shutdown cannot return with stale transport work.
    #[tokio::test]
    async fn packet_task_drain_joins_prepared_service_response() {
        // The exact MAC and IPv4 link drive packet selection; the remaining identities only satisfy validation.
        let service_config = ServiceConfig::try_new(vec![ServiceInterfaceReservations {
            receiving_interface: "vlan200".to_string(),
            reservations: vec![ServiceReservation {
                attachment_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
                interface_ordinal: 0,
                internal_uuid: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
                mac_address: TEST_CLIENT_MAC_TEXT.parse().unwrap(),
                link: ServiceLink::Ipv4(ServiceLinkV4 {
                    address: "192.0.2.11".parse().unwrap(),
                    prefix: "192.0.2.10/31".parse().unwrap(),
                    gateway: "192.0.2.10".parse().unwrap(),
                }),
            }],
        }])
        .unwrap();
        let config = init(get_test_args())
            .await
            .unwrap()
            .with_service_config(service_config);
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));
        let response = packet_handler::process_packet(
            &get_byte_stream(Ipv4Addr::UNSPECIFIED, None, MessageType::Discover, None),
            TEST_SOURCE_ADDRESS,
            &config,
            "vlan200",
            &Dpu {},
            &mut machine_cache,
        )
        .await
        .unwrap();
        let sender = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(destination) = receiver.local_addr().unwrap() else {
            panic!("test receiver must bind an IPv4 address");
        };
        // Pause the accepted packet task before transport so shutdown must cancel rather than merely reap it.
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let metrics = MetricsCapture::start();
        let mut packet_tasks = JoinSet::new();
        packet_tasks.spawn(async move {
            started_tx.send(()).unwrap();
            release_rx.await.unwrap();
            response.send(destination, sender).await.unwrap();
        });
        started_rx.await.unwrap();

        // Run the same abort-and-join operation used after both listener receive loops stop admitting packets.
        super::abort_and_join_packet_tasks(&mut packet_tasks, "IPv4", "vlan200").await;

        // Joined cancellation drops the paused transport without a datagram or successful-reply accounting.
        assert!(packet_tasks.is_empty());
        assert!(release_tx.send(()).is_err());
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", "offer")]
            ),
            0.0
        );
        let mut buffer = [0; 512];
        assert!(
            timeout(Duration::from_millis(10), receiver.recv_from(&mut buffer))
                .await
                .is_err()
        );
    }

    /// Verifies the production IPv4 listener owns each admitted packet task so generation
    /// cancellation cannot leave detached transport work.
    #[tokio::test]
    async fn generation_cancellation_joins_listener_owned_packet_task() {
        // Reserve an unprivileged loopback port and load a service-only-capable DPU generation.
        let directory = TempDir::new().unwrap();
        let listener_reservation = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let SocketAddr::V4(listener_address) = listener_reservation.local_addr().unwrap() else {
            panic!("loopback listener must have an IPv4 address");
        };
        let mut args = make_reload_args(&directory, vec!["lo".to_string()]);
        args.mode = ServerMode::Dpu;
        args.listen_addr = listener_address;
        let mut dhcp_config = ipv6_only_config();
        dhcp_config.carbide_dhcp_server = Some(Ipv4Addr::LOCALHOST);
        dhcp_config.carbide_provisioning_server_ipv4 = Some(Ipv4Addr::LOCALHOST);
        tokio::fs::write(
            &args.dhcp_config,
            serde_yaml::to_string(&dhcp_config).unwrap(),
        )
        .await
        .unwrap();
        let config = super::load_config(&args, &args.dhcp_config, None, true)
            .await
            .unwrap();
        drop(listener_reservation);

        // Pause the exact packet admitted from this source inside the listener-owned JoinSet task.
        let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let source = sender.local_addr().unwrap();
        let (started_tx, mut started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        *super::IPV4_PACKET_TASK_PAUSE.lock().unwrap() = Some(super::PacketTaskPause {
            source,
            started: started_tx,
            release: release_rx,
        });
        let cancellation = CancellationToken::new();
        let generation = tokio::spawn(super::run_dhcp_generation(
            args,
            cancellation.clone(),
            config,
            0,
        ));
        let packet = get_byte_stream(Ipv4Addr::UNSPECIFIED, None, MessageType::Discover, None);
        timeout(Duration::from_secs(5), async {
            loop {
                sender.send_to(&packet, listener_address).await.unwrap();
                tokio::select! {
                    result = &mut started_rx => {
                        result.expect("listener packet task must reach the transport pause");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(10)) => {}
                }
            }
        })
        .await
        .expect("IPv4 listener did not admit the test packet");

        // Generation completion must abort and join the paused task rather than leave it detached.
        cancellation.cancel();
        timeout(Duration::from_secs(1), generation)
            .await
            .expect("generation did not drain its packet task")
            .unwrap();
        assert!(release_tx.send(()).is_err());
    }

    /// force_start=true must start the server even when no `_new` files are staged.
    #[tokio::test]
    async fn reload_force_start_with_no_staged_files() {
        let td = TempDir::new().unwrap();
        let args = make_reload_args(&td, vec!["eth0".to_string()]);

        tokio::fs::write(
            &args.dhcp_config,
            serde_yaml::to_string(&ipv6_only_config()).unwrap(),
        )
        .await
        .unwrap();

        let (mut cancel_token, mut dhcp_handle) = (None, None);
        handle_reload(&args, &mut cancel_token, &mut dhcp_handle, true, 0)
            .await
            .unwrap();

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

        let (mut cancel_token, mut dhcp_handle) = (None, None);
        handle_reload(&args, &mut cancel_token, &mut dhcp_handle, false, 0)
            .await
            .unwrap();

        assert!(
            cancel_token.is_none(),
            "server must not start without staged files"
        );
        assert!(
            dhcp_handle.is_none(),
            "server must not start without staged files"
        );
    }

    /// The actual Stop request must let an identical update restart service,
    /// while an identical update to a running generation must leave it alone.
    /// No manual staging or force flag may hide a broken control-loop decision.
    #[tokio::test]
    async fn control_loop_restarts_after_stop_with_unchanged_configuration() {
        for (scenario, candidate, expected_identity) in [
            (
                "IPv6-only explicit identity",
                ipv6_only_config(),
                b"\0\x02\0\0\x16\x47dpu:test-dpu".to_vec(),
            ),
            (
                "complete IPv4 pair with legacy identity",
                carbide_rpc_utils::dhcp::DhcpConfig {
                    carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
                    carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
                    ..Default::default()
                },
                vec![0, 2, 0, 0, 0x16, 0x47, 192, 0, 2, 1],
            ),
        ] {
            let td = TempDir::new().unwrap();
            let args = make_reload_args(&td, vec!["lo".to_string()]);
            let live_path = args.dhcp_config.clone();
            let dhcp_yaml = serde_yaml::to_string(&candidate).unwrap();
            let mut expected = candidate;
            expected.dhcpv6_server_id = Some(
                carbide_rpc_utils::dhcp::DhcpV6ServerId::try_from(expected_identity.clone())
                    .unwrap(),
            );
            let expected_yaml = serde_yaml::to_string(&expected).unwrap();
            let (sender, receiver) = tokio::sync::mpsc::channel(4);
            let exercise = async {
                // Stop before any generation exists is also a valid no-op.
                sender.send(super::ControlRequest::Stop).await.unwrap();
                // Initial apply, identical apply after Stop, then an identical
                // running update: only the first two may start a generation.
                for stop_before_update in [false, true, false] {
                    if stop_before_update {
                        sender.send(super::ControlRequest::Stop).await.unwrap();
                    }
                    let (applied, completed) = oneshot::channel();
                    sender
                        .send(super::ControlRequest::UpdateAndReload {
                            dhcp_yaml: dhcp_yaml.clone(),
                            host_yaml: None,
                            interfaces: vec!["lo".to_string()],
                            service_config: ServiceConfig::default(),
                            applied,
                        })
                        .await
                        .unwrap();
                    completed.await.unwrap().unwrap();
                    assert_eq!(
                        tokio::fs::read_to_string(&live_path).await.unwrap(),
                        expected_yaml,
                        "{scenario}"
                    );
                    assert_eq!(
                        tokio::fs::read(super::server_identity::path(&live_path))
                            .await
                            .unwrap(),
                        expected_identity,
                        "{scenario}"
                    );
                    assert!(
                        !tokio::fs::try_exists(format!("{live_path}_new"))
                            .await
                            .unwrap(),
                        "{scenario}"
                    );
                }
                drop(sender);
            };
            let ((result, ()), logs) = capture_logs_async(async {
                timeout(Duration::from_secs(5), async {
                    tokio::join!(super::run_control_loop(args, receiver, 0), exercise)
                })
                .await
                .expect("control requests did not finish")
            })
            .await;
            result.unwrap();
            assert_eq!(
                logs.iter()
                    .filter(|entry| entry.message == "DHCP server (re)started with updated config")
                    .count(),
                2,
                "{scenario}: initial and post-Stop apply must start; the running repeat must not",
            );
        }
    }

    /// Expired queued work must never write files or start a generation. A later
    /// rejected request is the ordering barrier proving the owner consumed it.
    #[tokio::test]
    async fn control_loop_skips_abandoned_queued_updates() {
        let directory = TempDir::new().unwrap();
        let args = make_reload_args(&directory, vec!["lo".to_string()]);
        let live_path = args.dhcp_config.clone();
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let (abandoned, caller) = oneshot::channel();
        drop(caller);
        sender
            .send(super::ControlRequest::UpdateAndReload {
                dhcp_yaml: serde_yaml::to_string(&ipv6_only_config()).unwrap(),
                host_yaml: None,
                interfaces: vec!["lo".to_string()],
                service_config: ServiceConfig::default(),
                applied: abandoned,
            })
            .await
            .unwrap();
        let (applied, completed) = oneshot::channel();
        sender
            .send(super::ControlRequest::UpdateAndReload {
                dhcp_yaml: "[".to_string(),
                host_yaml: None,
                interfaces: vec!["lo".to_string()],
                service_config: ServiceConfig::default(),
                applied,
            })
            .await
            .unwrap();
        let exercise = async {
            assert!(matches!(
                completed.await.unwrap(),
                Err(super::ApplyError::InvalidConfig(DhcpError::SerdeYaml(_)))
            ));
            for path in [
                &live_path,
                &format!("{live_path}_new"),
                &super::server_identity::path(&live_path),
            ] {
                assert!(
                    !tokio::fs::try_exists(path).await.unwrap(),
                    "unexpected file: {path}"
                );
            }
            drop(sender);
        };
        let (result, ()) = timeout(Duration::from_secs(5), async {
            tokio::join!(super::run_control_loop(args, receiver, 0), exercise)
        })
        .await
        .expect("control loop did not reject the barrier request");
        result.unwrap();
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
            validate_config: None,
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

    /// Verifies recorded host activity survives generation initialization and later writes merge
    /// with retained history so restart cannot erase lifecycle evidence.
    #[tokio::test]
    async fn host_activity_survives_generation_initialization() {
        let _timestamp_file_guard = TEST_TIMESTAMP_FILE_LOCK.lock().await;
        // Serialize this host OFFER with zero-delta assertions over the process-global reply metric.
        let _metrics_guard = MetricsCapture::start();
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
            let mut d = DhcpTimestamps::new(DhcpTimestampsFilePath::Test);
            // Init the file like we would do during live operation.
            d.initialize_file().unwrap();
            d
        }));

        // Try a read() to show that the "init" of the timestamps file was
        // successful.
        DhcpTimestamps::new(DhcpTimestampsFilePath::Test)
            .read()
            .unwrap();

        // Process one host DISCOVER to record the activity that generation initialization must preserve.
        process(
            "1.2.3.4:0".parse().unwrap(),
            Arc::new(UdpSocket::bind(udp_socket_addr).await.unwrap()),
            &byte_stream,
            Arc::new(config.clone()),
            &*handler,
            "vlan100",
            &mut machine_cache,
            dhcp_timestamps.clone(),
        )
        .await;

        // Confirm the DISCOVER updated both the in-memory map and its durable timestamp file.
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

        assert!(before_dhcp < file_timestamp);

        // Reinitialize as a new generation would, with an empty in-memory map.
        let mut restarted_timestamps = DhcpTimestamps::new(DhcpTimestampsFilePath::Test);
        restarted_timestamps.initialize_file().unwrap();

        // Existing host activity must survive in the replacement generation's write state.
        assert_eq!(
            restarted_timestamps.get_timestamp(&config.host_config().unwrap().host_interface_id),
            dhcp_timestamps_new.get_timestamp(&config.host_config().unwrap().host_interface_id)
        );

        // Writing a different host after restart must merge with, rather than replace, the loaded activity.
        let additional_host = "22222222-2222-2222-2222-222222222222".parse().unwrap();
        restarted_timestamps.add_timestamp(additional_host, "2026-10-21T00:00:00Z".to_string());
        restarted_timestamps.write().unwrap();
        let mut merged_timestamps = DhcpTimestamps::new(DhcpTimestampsFilePath::Test);
        merged_timestamps.read().unwrap();

        // The durable map retains both the pre-restart host and the new observation.
        assert_eq!(
            merged_timestamps.get_timestamp(&config.host_config().unwrap().host_interface_id),
            dhcp_timestamps_new.get_timestamp(&config.host_config().unwrap().host_interface_id)
        );
        assert_eq!(
            merged_timestamps
                .get_timestamp(&additional_host)
                .map(String::as_str),
            Some("2026-10-21T00:00:00Z")
        );
    }

    /// Verifies malformed timestamp history degrades to an empty writable map because advisory
    /// activity must not prevent listener startup.
    #[tokio::test]
    async fn malformed_timestamp_history_is_nonfatal() {
        let _timestamp_file_guard = TEST_TIMESTAMP_FILE_LOCK.lock().await;
        // Install invalid durable JSON at the path used by the test timestamp backend.
        let path = DhcpTimestampsFilePath::Test.path_str();
        if std::fs::exists(path).unwrap() {
            std::fs::remove_file(path).unwrap();
        }
        std::fs::write(path, b"not timestamp JSON").unwrap();

        // Initialize through the generation helper that owns the nonfatal failure policy.
        let (mut timestamps, logs) = capture_logs_async(async {
            super::initialize_dhcp_timestamps(DhcpTimestampsFilePath::Test)
        })
        .await;

        // The failure remains observable while the returned map can recover on its next host write.
        assert!(logs.iter().any(|entry| {
            entry.metadata_name == "dhcp_timestamp_file_failed"
                && entry.field("operation") == Some("initialize")
        }));
        let host_interface_id = "11111111-1111-1111-1111-111111111111".parse().unwrap();
        assert!(timestamps.get_timestamp(&host_interface_id).is_none());
        // Record one host after recovery to prove the reset timestamp map remains writable.
        timestamps.add_timestamp(host_interface_id, "2026-10-21T00:00:00Z".to_string());
        timestamps.write().unwrap();
        let mut recovered = DhcpTimestamps::new(DhcpTimestampsFilePath::Test);
        recovered.read().unwrap();
        assert_eq!(
            recovered
                .get_timestamp(&host_interface_id)
                .map(String::as_str),
            Some("2026-10-21T00:00:00Z")
        );
        std::fs::remove_file(path).unwrap();
    }

    /// Verifies a service ACK traverses binary packet processing without updating host activity so
    /// service traffic cannot extend the tenant-host lifecycle.
    #[tokio::test]
    async fn service_packets_do_not_record_host_activity() {
        // Attach a service reservation to an existing host interface so an erroneous timestamp update is observable.
        let service_config = ServiceConfig::try_new(vec![ServiceInterfaceReservations {
            receiving_interface: "vlan200".to_string(),
            reservations: vec![ServiceReservation {
                attachment_id: "11111111-1111-1111-1111-111111111111".parse().unwrap(),
                interface_ordinal: 0,
                internal_uuid: "22222222-2222-2222-2222-222222222222".parse().unwrap(),
                mac_address: TEST_CLIENT_MAC_TEXT.parse().unwrap(),
                link: ServiceLink::Ipv4(ServiceLinkV4 {
                    address: "192.0.2.11".parse().unwrap(),
                    prefix: "192.0.2.10/31".parse().unwrap(),
                    gateway: "192.0.2.10".parse().unwrap(),
                }),
            }],
        }])
        .unwrap();
        // Route the ACK to a bound loopback relay socket so send success is independent of runner routing.
        let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let SocketAddr::V4(relay_destination) = receiver.local_addr().unwrap() else {
            panic!("the loopback relay must bind an IPv4 address");
        };
        let mut args = get_test_args();
        args.relay_response_port = relay_destination.port();
        let config = init(args)
            .await
            .unwrap()
            .with_service_config(service_config);
        let host_interface_id = config.host_config().unwrap().host_interface_id;
        let timestamps = Arc::new(Mutex::new(DhcpTimestamps::new(
            DhcpTimestampsFilePath::NotSet,
        )));
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        // Process an address-matched REQUEST whose chaddr resolves to the service reservation.
        let (_, logs) = capture_logs_async(process(
            TEST_SOURCE_ADDRESS,
            Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
            &get_byte_stream(
                "192.0.2.11".parse().unwrap(),
                Some(*relay_destination.ip()),
                MessageType::Request,
                None,
            ),
            Arc::new(config),
            &Dpu {},
            "vlan200",
            &mut machine_cache,
            timestamps.clone(),
        ))
        .await;

        // A sent-reply Event proves the timestamp assertion cannot pass because packet processing failed early.
        assert!(logs.iter().any(|entry| {
            entry.metadata_name == "dhcp_server_reply_sent"
                && entry.field("message_type") == Some("ack")
        }));

        // Service traffic must remain absent from the host activity record used by lifecycle decisions.
        assert!(
            timestamps
                .lock()
                .await
                .get_timestamp(&host_interface_id)
                .is_none()
        );
    }

    /// Verifies a retired host response cannot advance host activity because obsolete generations
    /// must not influence lifecycle decisions.
    #[tokio::test]
    async fn retired_host_response_does_not_record_activity() {
        // Retire an otherwise valid host generation before its DISCOVER reaches the final socket write.
        let cancellation = CancellationToken::new();
        let config = init(get_test_args())
            .await
            .unwrap()
            .with_reply_retirement(cancellation.clone());
        let host_interface_id = config.host_config().unwrap().host_interface_id;
        let timestamps = Arc::new(Mutex::new(DhcpTimestamps::new(
            DhcpTimestampsFilePath::NotSet,
        )));
        let mut machine_cache = Arc::new(Mutex::new(LruCache::new(
            std::num::NonZeroUsize::new(cache::MACHINE_CACHE_SIZE).unwrap(),
        )));

        // Retire the generation before processing so the final send cannot record success or host activity.
        cancellation.cancel();

        // Process through the binary send and timestamp path rather than calling the packet transport directly.
        let metrics = MetricsCapture::start();
        let (_, logs) = capture_logs_async(process(
            TEST_SOURCE_ADDRESS,
            Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()),
            &get_byte_stream(Ipv4Addr::UNSPECIFIED, None, MessageType::Discover, None),
            Arc::new(config),
            &Test {},
            "vlan200",
            &mut machine_cache,
            timestamps.clone(),
        ))
        .await;

        // Retirement suppresses reply accounting and the host activity side effect together.
        assert!(
            logs.iter()
                .all(|entry| entry.metadata_name != "dhcp_server_reply_sent")
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_dhcp_replies_sent_total",
                &[("message_type", "offer")]
            ),
            0.0
        );
        assert!(
            timestamps
                .lock()
                .await
                .get_timestamp(&host_interface_id)
                .is_none()
        );
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
        // The fixture deliberately gives DHCP and PXE different addresses.
        // Pin both wire fields so optional configuration cannot swap them.
        assert_eq!(packet.siaddr(), Ipv4Addr::new(10, 217, 126, 17));
        assert_eq!(
            packet.opts().get(OptionCode::ServerIdentifier),
            Some(&DhcpOption::ServerIdentifier(Ipv4Addr::new(
                10, 217, 126, 16
            ))),
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
        // NAK construction is separate from the ordinary reply builder.
        assert_eq!(packet.siaddr(), Ipv4Addr::new(10, 217, 126, 17));
        assert_eq!(
            packet.opts().get(OptionCode::ServerIdentifier),
            Some(&DhcpOption::ServerIdentifier(Ipv4Addr::new(
                10, 217, 126, 16
            ))),
        );
    }
}
