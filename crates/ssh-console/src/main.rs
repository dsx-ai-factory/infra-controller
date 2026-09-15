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

use clap::Parser;
use ssh_console::config::{Config, ConfigError, Defaults};
use ssh_console::shutdown_handle::ShutdownHandle;
use tracing::metadata::LevelFilter;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    setup_logging(&cli);

    match cli.command {
        Command::Run(run_command) => {
            let spawn_handle = ssh_console::spawn((*run_command).try_into()?).await?;
            // Let the service run forever by awaiting the join handle, while holding onto the
            // shutdown handle.
            let (_shutdown_tx, join_handle) = spawn_handle.into_parts();
            join_handle.await.expect("ssh-console task panicked");
        }
        Command::DefaultRunConfig => {
            print!("{}", Config::default().into_annotated_config_file())
        }
    }

    Ok(())
}

#[derive(clap::Parser, Debug)]
struct Cli {
    #[clap(long, short, help = "Turn on debug loggging (same as RUST_LOG=debug)")]
    debug: bool,
    #[clap(subcommand)]
    command: Command,
}

#[derive(clap::Parser, Debug)]
enum Command {
    Run(Box<RunCommand>),
    #[clap(about = "Output a default TOML config file for use with run -c")]
    DefaultRunConfig,
}

#[derive(clap::Parser, Debug)]
struct RunCommand {
    #[clap(long, short, help = "Path to TOML configuration file")]
    config: Option<PathBuf>,
    #[clap(
        long,
        short,
        help = "Address to listen on, overriding configuration file"
    )]
    address: Option<String>,
    #[clap(
        long,
        short,
        help = "Address to listen on for prometheus metrics requests (HTTP), overriding configuration file"
    )]
    metrics_address: Option<String>,
    #[clap(
        long,
        help = "Address to listen on for the private console-log gRPC API"
    )]
    api_listen_address: Option<String>,
    #[clap(long, short = 'u', help = "Address of carbide-api (forge)")]
    forge_url: Option<http::Uri>,
    #[clap(
        long,
        env = "FORGE_ROOT_CA_PATH",
        help = format!("Default to FORGE_ROOT_CA_PATH environment variable or {}", Defaults::root_ca_path().display())
    )]
    forge_root_ca_path: Option<PathBuf>,
    #[clap(
        long,
        env = "CLIENT_CERT_PATH",
        help = format!("Client cert to use to talk to forge. Default to CLIENT_CERT_PATH environment variable or {}", Defaults::client_cert_path().display())
    )]
    client_cert_path: Option<PathBuf>,
    #[clap(
        long,
        env = "CLIENT_KEY_PATH",
        help = format!("Client cert to use to talk to forge. Default to CLIENT_CERT_PATH environment variable or {}", Defaults::client_key_path().display())
    )]
    client_key_path: Option<PathBuf>,
    #[clap(long, short = 'k', help = "Path to SSH host key")]
    host_key: Option<PathBuf>,
    #[clap(long, help = "Path to SSH authorized_keys file (non forge-rpc mode)")]
    authorized_keys: Option<PathBuf>,
    #[clap(long, short = 'g', action, help = "Include DPU consoles")]
    dpus: bool,
    #[clap(
        long,
        short = 'i',
        action,
        help = "Disable client auth enforcement. All incoming SSH connections will succeed."
    )]
    insecure: bool,
    #[clap(long, help = "Override port for SSH to BMCs")]
    bmc_ssh_port: Option<u16>,
    #[clap(long, help = "Override port for IPMI to BMCs")]
    ipmi_port: Option<u16>,
    #[clap(
        long,
        action,
        help = "Use insecure ciphers when connecting to IPMI (useful for ipmi_sim)"
    )]
    insecure_ipmi_ciphers: bool,
    #[clap(
        long,
        env = "OVERRIDE_BMC_SSH_HOST",
        help = "Override hostname for SSH to BMCs (useful for machine-a-tron mocks)"
    )]
    override_bmc_ssh_host: Option<String>,
}

impl TryInto<Config> for RunCommand {
    type Error = CliError;

    // Load the config file, or the default, allowing CLI flags to override the corresponding settings.
    fn try_into(self) -> Result<Config, Self::Error> {
        let mut config = if let Some(config_path) = self.config {
            Config::load(&config_path)?
        } else {
            Config::default()
        };

        if let Some(address) = self.address {
            config.listen_address =
                address
                    .parse()
                    .map_err(|error| CliError::InvalidListeningAddress {
                        addr: address,
                        error,
                    })?;
        }
        if let Some(metrics_address) = self.metrics_address {
            config.metrics_address =
                metrics_address
                    .parse()
                    .map_err(|error| CliError::InvalidMetricsAddress {
                        addr: metrics_address,
                        error,
                    })?;
        }
        if let Some(carbide_url) = self.forge_url {
            config.carbide_uri = carbide_url;
        }
        if let Some(host_key) = self.host_key {
            config.host_key_path = host_key;
        }
        if self.dpus {
            config.dpus = true;
        }
        if self.insecure {
            config.insecure = true;
        }
        if self.insecure_ipmi_ciphers {
            config.insecure_ipmi_ciphers = true;
        }
        if let Some(ipmi_port) = self.ipmi_port {
            config.override_ipmi_port = Some(ipmi_port);
        }
        if let Some(bmc_ssh_port) = self.bmc_ssh_port {
            config.override_bmc_ssh_port = Some(bmc_ssh_port);
        }
        if let Some(authorized_keys) = self.authorized_keys {
            config.authorized_keys_path = Some(authorized_keys);
        }
        if let Some(forge_root_ca_path) = self.forge_root_ca_path {
            config.forge_root_ca_path = forge_root_ca_path;
        }
        if let Some(client_cert_path) = self.client_cert_path {
            config.client_cert_path = client_cert_path;
        }
        if let Some(client_key_path) = self.client_key_path {
            config.client_key_path = client_key_path;
        }
        if let Some(api_listen_address) = self.api_listen_address {
            config.api_listen_address = api_listen_address.parse().map_err(|error| {
                CliError::InvalidApiListeningAddress {
                    addr: api_listen_address,
                    error,
                }
            })?;
        }
        if let Some(override_bmc_ssh_host) = self.override_bmc_ssh_host {
            config.override_bmc_ssh_host = Some(override_bmc_ssh_host);
        }

        Ok(config)
    }
}

#[derive(thiserror::Error, Debug)]
enum CliError {
    #[error("invalid listening address {addr}: {error}")]
    InvalidListeningAddress { addr: String, error: AddrParseError },
    #[error("invalid metrics address {addr}: {error}")]
    InvalidMetricsAddress { addr: String, error: AddrParseError },
    #[error("invalid API listening address {addr}: {error}")]
    InvalidApiListeningAddress { addr: String, error: AddrParseError },
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
}

fn setup_logging(cli: &Cli) {
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::util::SubscriberInitExt;

    let level = if cli.debug {
        Some(LevelFilter::DEBUG)
    } else {
        None
    };

    if let Err(e) = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::Layer::default().compact())
        .with(
            EnvFilter::builder()
                .with_default_directive(level.map(Into::into).unwrap_or(LevelFilter::INFO.into()))
                .from_env_lossy(),
        )
        .try_init()
    {
        panic!(
            "Failed to initialize trace logging for ssh-console. It's possible some earlier \
            code path has already set a global default log subscriber: {e}"
        );
    }
}
