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

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use eyre::WrapErr;
use fmds::cfg::Options;
use fmds::grpc_server::FmdsGrpcServer;
use fmds::rest_server::get_fmds_router;
use fmds::state::FmdsState;
use fmds::{http_request_metrics, nic_init};
use forge_tls::client_config::ClientCert;
use rpc::fmds::fmds_config_service_server::FmdsConfigServiceServer;
use rpc::forge_tls_client::ForgeClientConfig;
use rpc::node_token_socket::SocketTokenSource;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tracing::metadata::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::prelude::*;
use tracing_subscriber::util::SubscriberInitExt;

pub fn subscriber() -> impl SubscriberInitExt {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("tower=warn".parse().unwrap())
        .add_directive("rustls=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("sqlx=info".parse().unwrap())
        .add_directive("tokio_util::codec=warn".parse().unwrap())
        .add_directive("h2=warn".parse().unwrap())
        .add_directive("hickory_resolver::error=info".parse().unwrap())
        .add_directive("hickory_proto::xfer=info".parse().unwrap())
        .add_directive("hickory_resolver::name_server=info".parse().unwrap())
        .add_directive("hickory_proto=info".parse().unwrap())
        .add_directive("netlink_proto=warn".parse().unwrap());
    let stdout_formatter = logfmt::layer()
        .with_event_fields([logfmt::EventField::with_default("component", "nico-fmds")]);
    let log_events = carbide_instrument::LogEventsMetric::new("nico-fmds");
    Box::new(
        tracing_subscriber::registry()
            .with(log_events.layer().with_filter(env_filter.clone()))
            .with(stdout_formatter.with_filter(env_filter)),
    )
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let subscriber = subscriber();
    subscriber
        .try_init()
        .expect("tracing_subscriber setup failed");
    tracing::error!("Starting fmds...");
    let options = Options::parse();

    if options.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }
    options.validate()?;

    tracing::info!(
        version = carbide_version::version!(),
        "Starting carbide-fmds"
    );

    nic_init::assign_address(&options.interface_name, options.interface_cidr).await?;
    // Custom IPv6 assignments retain operator-managed routing. Only the
    // additional fixed metadata endpoint has a known IPv6 HBN peer.
    if options.interface_cidr.is_ipv4() {
        nic_init::setup_metadata_routing(&options.interface_name, options.interface_cidr).await?;
    }
    if let Some(cidr) = options.interface_cidr_v6 {
        nic_init::assign_address(&options.interface_name, cidr.into()).await?;
        nic_init::setup_metadata_routing(&options.interface_name, cidr.into()).await?;
    }

    // Build ForgeClientConfig for phone_home. Either credential works alone:
    // the shared mTLS client cert, and/or node-auth bearer tokens fetched from
    // the dpu-agent's local API socket (#355) — the latter lets this pod run
    // without the machine private key mounted at all.
    let client_cert = match (&options.client_cert, &options.client_key) {
        (Some(cert_path), Some(key_path)) => Some(ClientCert {
            cert_path: cert_path.clone(),
            key_path: key_path.clone(),
        }),
        (None, None) => None,
        // Half a credential is a deployment bug; fail loudly instead of
        // silently running without mTLS.
        (Some(_), None) => {
            eyre::bail!("--client-cert was provided without --client-key")
        }
        (None, Some(_)) => {
            eyre::bail!("--client-key was provided without --client-cert")
        }
    };
    // Same class of deployment bug as half a client credential above, and it
    // deserves the same treatment: a token socket without a trust anchor
    // cannot authenticate the server, so the client would never be built and
    // phone_home would go quietly missing behind a generic warning.
    if options.root_ca.is_none() && options.node_token_socket.is_some() {
        eyre::bail!("--node-token-socket was provided without --root-ca");
    }
    let forge_client_config = match &options.root_ca {
        Some(root_ca) if client_cert.is_some() || options.node_token_socket.is_some() => {
            let mut config = ForgeClientConfig::new(root_ca.clone(), client_cert);
            if let Some(socket) = &options.node_token_socket {
                tracing::info!(
                    socket = %socket,
                    "fetching node-auth bearer tokens from the dpu-agent local API"
                );
                // Token mode usually runs without a client cert, which would
                // otherwise leave the channel on the dummy TLS verifier. The
                // bearer token is the client credential; the server still has
                // to prove itself against the root CA.
                config = config.with_token_provider(SocketTokenSource::spawn(socket.clone()));
            }
            Some(Arc::new(config))
        }
        _ => {
            tracing::warn!(
                "No TLS credentials provided; phone_home to carbide-api will be unavailable"
            );
            None
        }
    };

    let state = Arc::new(
        FmdsState::try_new(options.forge_api.clone(), forge_client_config)
            .map_err(|e| eyre::eyre!("failed to initialize FMDS state: {e}"))?,
    );

    let (prometheus_registry, http_request_metrics_state) = http_request_metrics::init()?;
    // init() installed the global meter provider, so the log-event counter can register now.
    carbide_instrument::log_events::register(&opentelemetry::global::meter("carbide-instrument"));
    let http_request_metrics_state = Arc::new(http_request_metrics_state);

    let metrics_address = options.metrics_address;
    let registry_for_metrics = prometheus_registry.clone();
    tokio::spawn(async move {
        let config = metrics_endpoint::MetricsEndpointConfig {
            address: metrics_address,
            registry: registry_for_metrics,
            health_controller: None,
            additional_prefix: None,
        };
        if let Err(err) = metrics_endpoint::run_metrics_endpoint(&config).await {
            tracing::error!(error = %err, "Prometheus metrics server error");
        }
    });

    // Build the REST service once so both the standard endpoint and the
    // compatibility endpoint expose exactly the same metadata API.
    let router = metadata_router(state.clone());
    let router =
        http_request_metrics::with_http_request_trace_layer(router, http_request_metrics_state);

    // Bind every requested endpoint before starting the service. Otherwise a
    // failed IPv6 bind could leave gRPC healthy while tenant metadata is down.
    let listeners = bind_rest_listeners(&options).await?;
    let mut rest_servers = spawn_rest_servers(listeners, router)?;

    // Start gRPC server for receiving config updates from agent
    let grpc_server = FmdsGrpcServer::new(state);

    tracing::info!(%options.grpc_address, "gRPC server listening");
    tokio::select! {
        result = tonic::transport::Server::builder()
            .add_service(FmdsConfigServiceServer::new(grpc_server))
            .serve(options.grpc_address) => result?,
        result = rest_servers.join_next() => {
            let result = result.ok_or_else(|| eyre::eyre!("no REST servers running"))?;
            result.wrap_err("REST server task failed")??;
            eyre::bail!("REST server stopped unexpectedly");
        }
    }

    Ok(())
}

/// Bind the configured REST endpoints without weakening an explicit IPv6
/// request to the metrics helper's IPv4-only fallback.
async fn bind_rest_listeners(options: &Options) -> eyre::Result<Vec<TcpListener>> {
    bind_rest_listeners_with(options, metrics_endpoint::bind_tcp_listener).await
}

/// Keep the caller's family check on the actual bound socket, including when
/// the shared listener helper falls back to IPv4 on an IPv4-only host.
async fn bind_rest_listeners_with<Fut>(
    options: &Options,
    mut bind: impl FnMut(SocketAddr) -> Fut,
) -> eyre::Result<Vec<TcpListener>>
where
    Fut: Future<Output = std::io::Result<TcpListener>>,
{
    let mut listeners = Vec::new();
    for address in std::iter::once(options.rest_address).chain(options.compatibility_rest_address) {
        let listener = bind(address)
            .await
            .wrap_err_with(|| format!("failed to bind REST listener on {address}"))?;
        if address.is_ipv6() && listener.local_addr()?.is_ipv4() {
            eyre::bail!("IPv6 is unavailable for REST listener on {address}");
        }
        listeners.push(listener);
    }
    Ok(listeners)
}

/// Both versioned paths expose the same metadata for cloud-init clients.
fn metadata_router(state: Arc<FmdsState>) -> axum::Router {
    axum::Router::new()
        .nest("/latest", get_fmds_router(state.clone()))
        .nest("/2009-04-04", get_fmds_router(state))
}

/// Own every REST server so a failed listener can terminate the service and
/// dropping the set cancels siblings instead of leaving detached servers.
fn spawn_rest_servers(
    listeners: Vec<TcpListener>,
    router: axum::Router,
) -> eyre::Result<JoinSet<eyre::Result<()>>> {
    let mut servers = JoinSet::new();
    for listener in listeners {
        let rest_address = listener.local_addr()?;
        let router = router.clone();
        servers.spawn(async move {
            tracing::info!(%rest_address, "REST server listening");
            axum_server::Server::<SocketAddr>::from_listener(listener)
                .serve(router.into_make_service())
                .await
                .wrap_err_with(|| format!("REST server on {rest_address} stopped"))
        });
    }
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use fmds::state::FmdsConfig;

    use super::*;

    /// The configured dual-stack listener serves the same metadata over a
    /// real IPv6 connection, while the IPv4 compatibility endpoint stays live.
    #[tokio::test]
    async fn metadata_is_served_over_ipv6_and_the_compatibility_listener() {
        // Probe the platform independently of FMDS so an IPv4-only test host
        // can skip transport assertions without hiding a broken FMDS listener.
        if let Err(error) = std::net::TcpListener::bind("[::1]:0") {
            let _ipv4 = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("IPv4 loopback must work before skipping IPv6 transport");
            eprintln!("Skipping IPv6 transport assertions: IPv6 loopback is unavailable: {error}");
            return;
        }
        let options = Options::try_parse_from([
            "carbide-fmds",
            "--rest-address",
            "[::]:0",
            "--compatibility-rest-address",
            "127.0.0.1:0",
        ])
        .expect("listener configuration");
        let state =
            Arc::new(FmdsState::try_new("https://api.test".into(), None).expect("FMDS state"));
        state.update_config(FmdsConfig {
            public_ipv4: None,
            public_ipv6: Some("2001:db8::17".parse().expect("host address")),
            hostname: "ipv6-tenant".into(),
            instance_name: None,
            sitename: None,
            instance_id: None,
            machine_id: None,
            user_data: String::new(),
            ib_devices: None,
            asn: 65000,
        });
        let listeners = bind_rest_listeners(&options).await.expect("REST listeners");
        let addresses = listeners
            .iter()
            .map(|listener| listener.local_addr().expect("bound address"))
            .collect::<Vec<_>>();
        let mut servers = spawn_rest_servers(listeners, metadata_router(state))
            .expect("start metadata listeners");
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("HTTP client");
        for (host, version) in [
            // A literal IPv6 URL proves transport, not just an IPv6 payload.
            (format!("[::1]:{}", addresses[0].port()), "latest"),
            // The primary wildcard must retain IPv4 on its existing port too.
            (format!("127.0.0.1:{}", addresses[0].port()), "latest"),
            // An independent compatibility port remains separately reachable.
            (format!("127.0.0.1:{}", addresses[1].port()), "2009-04-04"),
        ] {
            let response = client
                .get(format!("http://{host}/{version}/meta-data/hostname"))
                .send()
                .await
                .expect("metadata response");
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            assert_eq!(response.text().await.expect("metadata body"), "ipv6-tenant");
        }
        servers.shutdown().await;
    }

    /// A successful IPv4 fallback is still a failure for an explicitly IPv6
    /// REST endpoint. Inject the socket so this check also runs without IPv6.
    #[tokio::test]
    async fn ipv4_fallback_cannot_satisfy_an_ipv6_rest_listener() {
        let options = Options::try_parse_from(["carbide-fmds", "--rest-address", "[::]:0"])
            .expect("IPv6 listener configuration");
        let error = bind_rest_listeners_with(&options, |address| async move {
            assert_eq!(address, "[::]:0".parse().expect("requested address"));
            TcpListener::bind("127.0.0.1:0").await
        })
        .await
        .expect_err("IPv4-only listener cannot serve the requested IPv6 endpoint");
        assert_eq!(
            error.to_string(),
            "IPv6 is unavailable for REST listener on [::]:0"
        );
    }

    /// Binding the requested set fails on an occupied compatibility endpoint
    /// and preserves its address and I/O cause for the startup error.
    #[tokio::test]
    async fn occupied_compatibility_listener_fails_startup() {
        let reservation = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve port");
        let address = reservation
            .local_addr()
            .expect("reserved address")
            .to_string();
        let options = Options::try_parse_from([
            "carbide-fmds",
            "--rest-address",
            "127.0.0.1:0",
            "--compatibility-rest-address",
            &address,
        ])
        .expect("listener configuration");
        let error = bind_rest_listeners(&options)
            .await
            .expect_err("occupied compatibility port");
        assert!(error.to_string().contains(&address), "{error:?}");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .expect("bind error source")
                .kind(),
            std::io::ErrorKind::AddrInUse
        );
    }
}
