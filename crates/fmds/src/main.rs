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

use std::sync::Arc;

use clap::Parser;
use fmds::cfg::Options;
use fmds::grpc_server::FmdsGrpcServer;
use fmds::rest_server::get_fmds_router;
use fmds::state::FmdsState;
use fmds::{http_request_metrics, nic_init};
use forge_tls::client_config::ClientCert;
use rpc::fmds::fmds_config_service_server::FmdsConfigServiceServer;
use rpc::forge_tls_client::ForgeClientConfig;
use rpc::node_token_socket::SocketTokenSource;
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

    tracing::info!(
        version = carbide_version::version!(),
        "Starting carbide-fmds"
    );

    nic_init::assign_address(&options.interface_name, options.interface_cidr).await?;
    nic_init::setup_metadata_routing(&options.interface_name, options.interface_cidr).await?;

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
    let rest_state = state.clone();
    // We serve metadata under both /latest and /2009-04-04 for
    // compatibility with cloud-init, which uses the AWS EC2 instance
    // metadata API versioned path format.
    let router = axum::Router::new()
        .nest("/latest", get_fmds_router(rest_state.clone()))
        .nest("/2009-04-04", get_fmds_router(rest_state));
    let router =
        http_request_metrics::with_http_request_trace_layer(router, http_request_metrics_state);

    let rest_addresses =
        std::iter::once(options.rest_address).chain(options.compatibility_rest_address);
    for rest_address in rest_addresses {
        let router = router.clone();
        tokio::spawn(async move {
            let server = axum_server::Server::bind(rest_address);

            tracing::info!(%rest_address, "REST server listening");
            if let Err(err) = server.serve(router.into_make_service()).await {
                tracing::error!(%rest_address, error = %err, "REST server error");
            }
        });
    }

    // Start gRPC server for receiving config updates from agent
    let grpc_server = FmdsGrpcServer::new(state);

    tracing::info!(%options.grpc_address, "gRPC server listening");
    tonic::transport::Server::builder()
        .add_service(FmdsConfigServiceServer::new(grpc_server))
        .serve(options.grpc_address)
        .await?;

    Ok(())
}
