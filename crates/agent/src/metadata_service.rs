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

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use eyre::WrapErr;
use forge_dpu_agent_utils::FMDS_IPV6_ADDRESS;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::instance_metadata_endpoint::{InstanceMetadataRouterStateImpl, get_fmds_router};
use crate::instrumentation::{AgentMetricsState, WithTracingLayer};

/// Bind FMDS before reporting startup success, and keep its task with the agent's
/// other services. An enabled IPv6 endpoint must not fall back to an IPv4 listener.
pub(super) async fn spawn_metadata_service(
    metadata_service_address: String,
    metrics_state: Arc<AgentMetricsState>,
    state: Arc<InstanceMetadataRouterStateImpl>,
    require_ipv6: bool,
    background_tasks: &mut JoinSet<()>,
    cancel_token: CancellationToken,
) -> eyre::Result<SocketAddr> {
    let listen_address: SocketAddr = metadata_service_address
        .parse()
        .wrap_err("invalid metadata service address")?;
    if require_ipv6
        && !matches!(listen_address, SocketAddr::V6(address)
            if address.ip().is_unspecified() || *address.ip() == FMDS_IPV6_ADDRESS)
    {
        eyre::bail!(
            "IPv6 FMDS requires a metadata service listener on [::] or [{FMDS_IPV6_ADDRESS}]"
        );
    }
    let listener = metrics_endpoint::bind_tcp_listener(listen_address)
        .await
        .wrap_err_with(|| format!("failed to bind metadata service at {listen_address}"))?;
    spawn_bound_metadata_service(
        listener,
        metrics_state,
        state,
        require_ipv6,
        background_tasks,
        cancel_token,
    )
}

/// Check the socket returned by the shared binder before registering a service;
/// its IPv4 fallback cannot satisfy an explicitly enabled IPv6 endpoint.
fn spawn_bound_metadata_service(
    listener: tokio::net::TcpListener,
    metrics_state: Arc<AgentMetricsState>,
    state: Arc<InstanceMetadataRouterStateImpl>,
    require_ipv6: bool,
    background_tasks: &mut JoinSet<()>,
    cancel_token: CancellationToken,
) -> eyre::Result<SocketAddr> {
    let bound_address = listener.local_addr()?;
    if require_ipv6 && !bound_address.is_ipv6() {
        eyre::bail!("IPv6 FMDS listener fell back to IPv4 at {bound_address}");
    }

    let router = Router::new()
        .nest(
            "/latest",
            get_fmds_router(state.clone()).with_tracing_layer(metrics_state.clone()),
        )
        .nest(
            "/2009-04-04",
            get_fmds_router(state).with_tracing_layer(metrics_state),
        );
    let handle = axum_server::Handle::new();
    let server = axum_server::Server::<SocketAddr>::from_listener(listener).handle(handle.clone());
    background_tasks.spawn(async move {
        let serving = server.serve(router.into_make_service());
        tokio::pin!(serving);
        let result = tokio::select! {
            result = &mut serving => result,
            () = cancel_token.cancelled() => {
                // Stop accepting requests immediately; in-flight requests get at
                // most five seconds so metadata cannot hold up agent shutdown.
                handle.graceful_shutdown(Some(Duration::from_secs(5)));
                serving.await
            }
        };
        if let Err(error) = result {
            tracing::error!(%error, listen_address = %bound_address, "Metadata service exited");
        }
    });

    tracing::info!(listen_address = %bound_address, "Metadata service listening");
    Ok(bound_address)
}

#[cfg(test)]
mod tests {
    use carbide_host_support::agent_config::MachineIdentityConfig;
    use http_body_util::{BodyExt, Empty};
    use hyper::body::Bytes;
    use hyper_util::rt::TokioIo;
    use rpc::forge_tls_client::ForgeClientConfig;

    use super::*;
    use crate::instrumentation::create_metrics;
    use crate::periodic_config_fetcher::{InstanceMetadata, PublicAddresses};

    /// Give the real router distinctive local metadata without making a Core RPC.
    fn metadata_state() -> Arc<InstanceMetadataRouterStateImpl> {
        let state = Arc::new(
            InstanceMetadataRouterStateImpl::new(
                "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg"
                    .parse()
                    .expect("DPU ID"),
                "http://127.0.0.1:1".to_string(),
                Arc::new(ForgeClientConfig::default()),
                MachineIdentityConfig::default(),
            )
            .expect("metadata state"),
        );
        let version = "V1-T1666644937952267".parse().expect("config version");
        state.update_instance_data(Some(Arc::new(InstanceMetadata {
            public_addresses: PublicAddresses::default(),
            hostname: "local-dpu-host".to_string(),
            instance_name: None,
            sitename: None,
            instance_id: None,
            machine_id: None,
            user_data: String::new(),
            ib_devices: None,
            config_version: version,
            network_config_version: version,
            extension_service_version: version,
        })));
        // The router serves instance metadata only after network config is present.
        state.update_network_configuration(Some(Arc::new(
            rpc::forge::ManagedHostNetworkConfigResponse::default(),
        )));
        state
    }

    /// Both address families must reach the production FMDS router, and cancelling
    /// the agent must finish the server and close its keep-alive connections.
    #[tokio::test]
    async fn ipv6_metadata_listener_serves_both_families_and_stops() {
        if !ipv6_loopback_available() {
            return;
        }
        let mut tasks = JoinSet::new();
        let cancel = CancellationToken::new();
        let address = spawn_metadata_service(
            "[::]:0".to_string(),
            create_metrics(opentelemetry::global::meter("metadata-listener-test")),
            metadata_state(),
            true,
            &mut tasks,
            cancel.clone(),
        )
        .await
        .expect("IPv6 metadata listener");
        let mut connections = JoinSet::new();
        let mut clients = Vec::new();
        for (host, path) in [
            // IPv6-only guests use the same current metadata API as IPv4 guests.
            ("[::1]", "latest"),
            // The dual-stack wildcard keeps the IPv4 compatibility API available.
            ("127.0.0.1", "2009-04-04"),
        ] {
            let client = tokio::time::timeout(Duration::from_secs(5), async {
                let stream = tokio::net::TcpStream::connect(format!("{host}:{}", address.port()))
                    .await
                    .expect("metadata connection");
                let (mut client, connection) =
                    hyper::client::conn::http1::handshake(TokioIo::new(stream))
                        .await
                        .expect("HTTP handshake");
                connections.spawn(async move {
                    connection.await.expect("metadata connection shutdown");
                });
                let request = hyper::Request::builder()
                    .uri(format!("/{path}/meta-data/hostname"))
                    .header(hyper::header::HOST, host)
                    .body(Empty::<Bytes>::new())
                    .expect("metadata request");
                let response = client
                    .send_request(request)
                    .await
                    .expect("metadata response");
                assert_eq!(response.status(), hyper::StatusCode::OK);
                assert_eq!(
                    response
                        .into_body()
                        .collect()
                        .await
                        .expect("metadata body")
                        .to_bytes(),
                    "local-dpu-host"
                );
                assert!(
                    !client.is_closed(),
                    "metadata connection stays open until cancellation"
                );
                client
            })
            .await
            .expect("metadata request deadline");
            clients.push(client);
        }

        // Keep the clients alive so only server shutdown closes these connections.
        // Rebinding the released port races with other tests claiming it.
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(6), async {
            tasks.join_all().await;
            connections.join_all().await;
        })
        .await
        .expect("metadata shutdown");
        assert!(clients.iter().all(|client| client.is_closed()));
    }

    /// Startup errors must reach the caller before a background task is accepted;
    /// otherwise the agent can report an enabled metadata endpoint that cannot listen.
    #[tokio::test]
    async fn metadata_startup_rejects_unreachable_ipv6_and_occupied_port() {
        let reservation = if ipv6_loopback_available() {
            Some(
                tokio::net::TcpListener::bind("[::1]:0")
                    .await
                    .expect("reserved listener"),
            )
        } else {
            None
        };
        for (address, expected_error) in [
            // A configured IPv4 bind cannot satisfy an enabled IPv6 endpoint.
            (
                "127.0.0.1:0".to_string(),
                "IPv6 FMDS requires a metadata service listener",
            ),
            // IPv6 loopback is not reachable through HBN's metadata endpoint.
            (
                "[::1]:0".to_string(),
                "IPv6 FMDS requires a metadata service listener",
            ),
        ]
        .into_iter()
        .chain(reservation.as_ref().map(|reservation| {
            // Keep the reservation open so the bind failure is deterministic.
            (
                format!(
                    "[::]:{}",
                    reservation.local_addr().expect("reserved address").port()
                ),
                "failed to bind metadata service",
            )
        })) {
            let mut tasks = JoinSet::new();
            let error = spawn_metadata_service(
                address,
                create_metrics(opentelemetry::global::meter("metadata-listener-test")),
                metadata_state(),
                true,
                &mut tasks,
                CancellationToken::new(),
            )
            .await
            .expect_err("metadata startup must fail");
            assert!(error.to_string().contains(expected_error), "{error:#}");
            assert!(tasks.is_empty());
        }
    }

    /// A real IPv4 socket returned by fallback must never become a successful
    /// IPv6 service, even though binding that socket itself succeeded.
    #[tokio::test]
    async fn metadata_rejects_a_bound_ipv4_fallback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("IPv4 fallback socket");
        let address = listener.local_addr().expect("fallback address");
        let mut tasks = JoinSet::new();
        let error = spawn_bound_metadata_service(
            listener,
            create_metrics(opentelemetry::global::meter("metadata-listener-test")),
            metadata_state(),
            true,
            &mut tasks,
            CancellationToken::new(),
        )
        .expect_err("IPv4 fallback must be rejected");
        assert!(error.to_string().contains("fell back to IPv4"), "{error:#}");
        assert!(tasks.is_empty());
        tokio::net::TcpListener::bind(address)
            .await
            .expect("rejected socket released");
    }

    /// Probe IPv6 independently of the binder under test so an IPv4 fallback
    /// regression cannot disguise itself as an unavailable test environment.
    fn ipv6_loopback_available() -> bool {
        match std::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0)) {
            Ok(_) => true,
            Err(error) => {
                let _listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .expect("IPv4 loopback must be available");
                eprintln!("Skipping IPv6 socket checks: IPv6 loopback unavailable: {error}");
                false
            }
        }
    }
}
