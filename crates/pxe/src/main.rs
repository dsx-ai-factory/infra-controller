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
use std::fmt::Debug;
use std::net::SocketAddr;

use axum::Router;
use axum::middleware::{map_request, map_response};
use axum_client_ip::ClientIpSource;
use axum_template::engine::Engine;
use carbide_utils::SCOUT_FIRMWARE_SCRIPTS_DIR;
use clap::Parser;
use common::AppState;
use tera::Tera;
use tower_http::services::ServeDir;
use tower_layer::Layer;

mod common;
mod config;
mod extractors;
mod metrics;
mod middleware;
mod routes;
mod rpc_error;
mod server;

#[derive(Parser, Debug)]
struct Args {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    pub version: bool,

    #[clap(short, long, default_value = "static")]
    static_dir: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = Args::parse();
    if opts.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }

    let static_path = std::path::Path::new(&opts.static_dir);
    if !&static_path.exists() {
        println!(
            "Static path {} does not exist. Creating directory",
            &static_path.display()
        );

        match std::fs::create_dir_all(static_path) {
            Ok(_) => println!("Directory {}, created", &static_path.display()),
            Err(e) => eprintln!("Could not create directory: {e}"),
        }
    }

    println!("Start carbide-pxe version {}", carbide_version::version!());
    let prometheus_handle = metrics::setup_prometheus();

    let runtime_config =
        config::RuntimeConfig::from_env().expect("unable to build runtime config?");

    let tera = Tera::new(format!("{}/**/*", runtime_config.template_directory).as_str())
        .expect("unable to build templating engine?");
    let socket_addr = SocketAddr::new(runtime_config.bind_address, runtime_config.bind_port);

    let app_state = AppState {
        engine: Engine::from(tera),
        runtime_config,
        prometheus_handle,
    };

    let app = Router::new()
        .nest_service(
            "/public/scout-firmware-scripts",
            ServeDir::new(SCOUT_FIRMWARE_SCRIPTS_DIR)
                .with_buf_chunk_size(1024 * 1024 * 10 /* 10 MiB*/),
        )
        .nest_service(
            "/public",
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

    let listener = tokio::net::TcpListener::bind(socket_addr)
        .await
        .map_err(|err| {
            eprintln!("unable to bind to tcp listener with error: {err}");
            err
        })?;

    println!(
        "serving http listen_address={socket_addr} header_read_timeout_seconds={}",
        server::HEADER_READ_TIMEOUT.as_secs()
    );
    server::serve(listener, final_app, server::HEADER_READ_TIMEOUT).await?;

    Ok(())
}
