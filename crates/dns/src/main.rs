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

use std::net::AddrParseError;
use std::path::PathBuf;

use carbide_dns::DnsServer;
use carbide_dns::config::{Config, ConfigError};
use clap::{CommandFactory, Parser};
use eyre::WrapErr;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::WithExportConfig;
use tonic::codegen::http::uri::InvalidUri;
use tracing::metadata::LevelFilter;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    let options = Options::parse();

    if options.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }
    let cmd = match options.command {
        None => {
            return Ok(Options::command().print_long_help()?);
        }
        Some(s) => s,
    };

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("tower=warn".parse()?)
        .add_directive("rustls=warn".parse()?)
        .add_directive("hyper=warn".parse()?)
        .add_directive("h2=warn".parse()?)
        .add_directive("tonic=warn".parse()?)
        .add_directive("opentelemetry_sdk=info".parse()?)
        .add_directive("opentelemetry_otlp=info".parse()?)
        .add_directive("hickory_proto=info".parse()?)
        .add_directive("hickory_resolver=info".parse()?)
        .add_directive("carbide_dns=info".parse()?)
        .add_directive("rpc=info".parse()?);

    match cmd {
        Command::Run(run_command) => {
            let config: Config = run_command.try_into()?;

            // Tracing is opt-in: an env var takes precedence over the config
            // file so operators can override it without redeploying, and no
            // endpoint (the default) disables trace export entirely.
            let otlp_endpoint =
                std::env::var(opentelemetry_otlp::OTEL_EXPORTER_OTLP_TRACES_ENDPOINT)
                    .ok()
                    .or_else(|| config.otlp_endpoint.as_ref().map(|uri| uri.to_string()));

            let otel_layer = match &otlp_endpoint {
                None => None,
                Some(endpoint) => {
                    let otlp_exporter = opentelemetry_otlp::SpanExporter::builder()
                        .with_tonic()
                        .with_endpoint(endpoint)
                        .build()?;

                    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                        .with_batch_exporter(otlp_exporter)
                        .with_resource(
                            opentelemetry_sdk::Resource::builder()
                                .with_attributes([opentelemetry::KeyValue::new(
                                    "service.name",
                                    "nico-dns",
                                )])
                                .build(),
                        )
                        .build();

                    Some(
                        tracing_opentelemetry::layer()
                            .with_tracer(tracer_provider.tracer("nico-dns")),
                    )
                }
            };

            let log_events = carbide_instrument::LogEventsMetric::new("nico-dns");
            tracing_subscriber::registry()
                .with(log_events.layer())
                .with(
                    logfmt::layer().with_event_fields([logfmt::EventField::with_default(
                        "component",
                        "nico-dns",
                    )]),
                )
                .with(env_filter)
                .with(otel_layer)
                .try_init()?;

            match &otlp_endpoint {
                Some(endpoint) => tracing::info!(%endpoint, "OpenTelemetry tracing enabled"),
                None => {
                    tracing::info!("OpenTelemetry tracing disabled: no OTLP endpoint configured")
                }
            }

            DnsServer::run(config)
                .await
                .wrap_err("failed to start DNS service")?;
        }
    }

    Ok(())
}

#[derive(Parser)]
pub struct Options {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    pub version: bool,

    #[clap(subcommand)]
    pub command: Option<Command>,
}

#[derive(Parser)]
pub enum Command {
    #[clap(about = "Start the DNS service")]
    Run(RunCommand),
}

#[derive(Parser)]
pub struct RunCommand {
    #[clap(long, short = 'f', help = "Path to the TOML configuration file")]
    config_file: Option<PathBuf>,

    #[clap(
        long,
        help = "Address for the DNS server to listen on (e.g., [::]:53)",
        visible_alias = "listen_address"
    )]
    pub listen: Option<String>,

    #[clap(
        long,
        help = "Address for the metrics server to listen on (e.g., [::]:8053)"
    )]
    pub metrics_listen_address: Option<String>,

    #[clap(short = 'u', long, help = "URI of the API server")]
    pub api_uri: Option<String>,

    #[clap(short = 'r', long, help = "Path to the root CA certificate")]
    pub root_ca_path: Option<PathBuf>,

    #[clap(
        short = 'c',
        long,
        help = "Path to the client certificate for the API server"
    )]
    pub client_cert_path: Option<PathBuf>,

    #[clap(short = 'k', long, help = "Path to the client key for the API server")]
    pub client_key_path: Option<PathBuf>,

    /// OTLP gRPC endpoint that traces are exported to. Trace export is
    /// disabled by default (this is unset). If the `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
    /// environment variable is set, its value takes precedence over this flag.
    #[clap(
        long,
        help = "OTLP gRPC endpoint that traces are exported to (e.g., http://otel-collector:4317). Defaults to unset, which disables trace export. Overridden by OTEL_EXPORTER_OTLP_TRACES_ENDPOINT."
    )]
    pub otlp_endpoint: Option<String>,

    // Backward compatibility alias
    #[clap(
        long,
        help = "DEPRECATED: Use --api-uri instead. Will be removed in future releases."
    )]
    pub carbide_url: Option<String>,
}

#[derive(thiserror::Error, Debug)]
pub enum CommandError {
    #[error("invalid listening address {addr}: {error}")]
    InvalidListeningAddress { addr: String, error: AddrParseError },
    #[error("invalid metrics address {addr}: {error}")]
    InvalidMetricsAddress { addr: String, error: AddrParseError },
    #[error("invalid URI: {uri}: {error}")]
    InvalidUri { uri: String, error: InvalidUri },
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
}

impl TryInto<Config> for RunCommand {
    type Error = CommandError;

    fn try_into(self) -> Result<Config, Self::Error> {
        let mut config = if let Some(config_path) = self.config_file {
            Config::load(&config_path)?
        } else {
            Config::default()
        };

        if let Some(listen_address) = self.listen {
            config.listen_address =
                listen_address
                    .parse()
                    .map_err(|error| CommandError::InvalidListeningAddress {
                        addr: listen_address,
                        error,
                    })?
        }

        if let Some(address) = self.metrics_listen_address {
            config.metrics_listen_address =
                address
                    .parse()
                    .map_err(|error| CommandError::InvalidMetricsAddress {
                        addr: address,
                        error,
                    })?
        }

        if let Some(carbide_uri) = self.api_uri {
            config.api_uri = carbide_uri
                .parse()
                .map_err(|error| CommandError::InvalidUri {
                    uri: carbide_uri,
                    error,
                })?
        }

        // Backward compatibility for carbide_url
        if let Some(carbide_url) = self.carbide_url {
            tracing::warn!("--carbide-url is deprecated; use --api-uri instead");
            config.api_uri = carbide_url
                .parse()
                .map_err(|error| CommandError::InvalidUri {
                    uri: carbide_url,
                    error,
                })?
        }

        if let Some(root_ca_path) = self.root_ca_path {
            config.root_ca_path = root_ca_path;
        }
        if let Some(client_cert_path) = self.client_cert_path {
            config.client_cert_path = client_cert_path;
        }
        if let Some(client_key_path) = self.client_key_path {
            config.client_key_path = client_key_path;
        }
        if let Some(otlp_endpoint) = self.otlp_endpoint {
            config.otlp_endpoint =
                Some(
                    otlp_endpoint
                        .parse()
                        .map_err(|error| CommandError::InvalidUri {
                            uri: otlp_endpoint,
                            error,
                        })?,
                );
        }

        Ok(config)
    }
}
