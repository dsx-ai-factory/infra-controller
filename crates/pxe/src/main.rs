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

use std::fmt::Debug;
use std::net::SocketAddr;

use axum::middleware::{map_request, map_response};
use axum::{Router, ServiceExt};
use axum_client_ip::ClientIpSource;
use axum_template::engine::Engine;
use carbide_utils::SCOUT_FIRMWARE_SCRIPTS_DIR;
use clap::Parser;
use common::AppState;
use tera::Tera;
use tower_http::services::ServeDir;
use tower_layer::Layer;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

mod common;
mod config;
mod extractors;
mod metrics;
mod middleware;
mod routes;
mod rpc_error;

/// The URL prefix the static-file directory is served under. Anything building
/// a URL into that directory composes it from this, so the path served and the
/// path advertised cannot drift.
pub(crate) const STATIC_URL_PREFIX: &str = "/public";

#[derive(Parser, Debug)]
struct Args {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    version: bool,

    #[clap(short, long, default_value = "static")]
    static_dir: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Args::parse();
    if opts.version {
        // The --version flag writes the bare version to stdout by design (a
        // machine-readable value, not a log line), so it stays a println.
        println!("{}", carbide_version::version!());
        return Ok(());
    }

    setup_tracing()?;

    let static_path = std::path::Path::new(&opts.static_dir);
    if !&static_path.exists() {
        tracing::info!(
            static_path = %static_path.display(),
            "static path does not exist; creating directory"
        );

        match std::fs::create_dir_all(static_path) {
            Ok(_) => {
                tracing::info!(static_path = %static_path.display(), "created static directory")
            }
            Err(e) => tracing::error!(error = %e, "could not create static directory"),
        }
    }

    tracing::info!(version = %carbide_version::version!(), "starting carbide-pxe");
    let prometheus_handle = metrics::setup_prometheus();

    // The instrumentation framework's events resolve their instruments from
    // the global OTel meter, so it installs before the router (and any first
    // emit) exists. The returned setup owns the meter provider and must stay
    // alive for the process lifetime -- dropping it stops collection.
    let otel_metrics = metrics_endpoint::new_metrics_setup("carbide-pxe", "carbide", true)
        .expect("unable to install the OTel meter provider?");

    // Bind the log-events counter -- installed as a subscriber layer in
    // setup_tracing so it counts from startup -- to the meter now that the
    // provider exists, so carbide_log_events_total exports pxe's log volume and
    // error rate like every other fleet binary.
    carbide_instrument::log_events::register(&otel_metrics.meter);

    let runtime_config =
        config::RuntimeConfig::from_env().expect("unable to build runtime config?");

    let tera = Tera::new(format!("{}/**/*", runtime_config.template_directory).as_str())
        .expect("unable to build templating engine?");
    let socket_addr = SocketAddr::new(runtime_config.bind_address, runtime_config.bind_port);

    let app_state = AppState {
        engine: Engine::from(tera),
        static_dir: opts.static_dir.clone(),
        runtime_config,
        prometheus_handle,
        otel_registry: otel_metrics.registry.clone(),
    };

    let app = Router::new()
        .nest_service(
            "/public/scout-firmware-scripts",
            ServeDir::new(SCOUT_FIRMWARE_SCRIPTS_DIR)
                .with_buf_chunk_size(1024 * 1024 * 10 /* 10 MiB*/),
        )
        .nest_service(
            STATIC_URL_PREFIX,
            ServeDir::new(opts.static_dir.clone())
                .with_buf_chunk_size(1024 * 1024 * 10 /* 10 MiB*/),
        )
        // .layer(
        //     axum_response_cache::CacheLayer::with_lifespan(60 * 5)
        //         .body_limit(1024 * 1024 * 1024 * 5 /* 5 GiB*/),
        // )
        // this DOES make a minor difference in performance, but also consumes quite a lot of memory
        // we'd have to see if it's actually worthwhile in a real load test scenario
        .merge(routes::ipxe::get_router("/api/v0/pxe"))
        .merge(routes::cloud_init::get_router("/api/v0/cloud-init"))
        .merge(routes::tls::get_router("/api/v0/tls"))
        .route_layer(axum::middleware::from_fn(middleware::logging::logger))
        .layer(map_response(middleware::fix_content_length_header))
        .layer(middleware::metrics::MetricLayer::default())
        // This fetches the ClientIP from the SocketAddress.
        .layer(ClientIpSource::ConnectInfo.into_extension())
        .merge(routes::metrics::get_router("/metrics"))
        .with_state(app_state); // The order of the calls here matters --> we only want to cache the files on disk, nothing else, and we don't want /metrics to be included in our metrics.

    let request_normalizing_middleware = map_request(middleware::normalize_url);
    let final_app = request_normalizing_middleware.layer(app); // this one has to wrap all the others for the map_request to be able to affect routing

    let listener = metrics_endpoint::bind_tcp_listener(socket_addr)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, "unable to bind tcp listener");
            err
        })?;

    axum::serve(
        listener,
        final_app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Installs the tracing subscriber that emits logs in the fleet's logfmt
/// format, tagged with the `nico-pxe` component, plus the log-events counting
/// layer that feeds the fleet-standard `carbide_log_events_total` counter.
/// Matches the other carbide binaries: an `INFO` default with the usual
/// dependency caps, overridable via `RUST_LOG`. The counter is bound to the
/// meter by `log_events::register` in `main`, once the provider exists.
fn setup_tracing() -> Result<(), Box<dyn std::error::Error>> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("hyper=warn".parse()?)
        .add_directive("h2=warn".parse()?)
        .add_directive("tower=warn".parse()?)
        .add_directive("rustls=warn".parse()?)
        .add_directive("tokio_util::codec=warn".parse()?);

    // Counts every log line into carbide_log_events_total from startup; the
    // counts are exposed once main() installs the meter provider. The env
    // filter sits on the registry as a global filter so the counting layer and
    // the logfmt output see exactly the same events.
    let log_events = carbide_instrument::LogEventsMetric::new("nico-pxe");
    tracing_subscriber::registry()
        .with(log_events.layer())
        .with(
            logfmt::layer()
                .with_event_fields([logfmt::EventField::with_default("component", "nico-pxe")]),
        )
        .with(env_filter)
        .try_init()?;

    Ok(())
}
