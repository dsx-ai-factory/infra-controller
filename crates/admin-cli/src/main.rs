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

// CLI enums variants can be rather large, we are ok with that.
#![allow(clippy::large_enum_variant)]
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

use std::fs::File;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge_api_client::ForgeApiClient;
use ::rpc::forge_tls_client::{ApiConfig, ForgeClientConfig, RetryConfig};
use cfg::cli_options::{CliCommand, CliOptions};
use clap::CommandFactory;
use errors::CarbideCliResult;
use forge_tls::client_config::{
    get_api_url, get_client_cert_info, get_config_from_file, get_proxy_info, get_root_ca_path,
};
use measured_boot::ToTable;
use serde::Serialize;
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::errors::CarbideCliError;
use crate::rpc::ApiClient;

/// Ceiling on the server-verifying attempt of `version`, so that a failure
/// reaches the unverified fallback quickly.
const VERIFIED_VERSION_TIMEOUT: Duration = Duration::from_secs(10);

mod admission_retry;
mod async_write;
mod attestation;
mod backend;
mod bmc_machine;
mod bmc_role;
mod boot_interface;
mod boot_override;
mod browse;
mod cfg;
mod component_manager;
mod compute_allocation;
mod credential;
mod debug_bundle;
mod devenv;
mod domain;
mod dpa;
mod dpf;
mod dpu;
mod dpu_remediation;
mod errors;
mod expected_machines;
mod expected_nvlink_domain;
mod expected_power_shelf;
mod expected_rack;
mod expected_rack_group;
mod expected_switch;
mod extension_service;
mod firmware;
mod generate_docs;
mod generate_man;
mod generate_shell_complete;
mod health_utils;
mod host;
mod ib_partition;
mod instance;
mod instance_type;
mod inventory;
mod ip;
mod ipxe_template;
mod jump;
mod machine;
mod machine_interfaces;
mod machine_validation;
mod managed_host;
mod managed_switch;
mod metadata;
mod mlx;
mod network_devices;
mod network_security_group;
mod network_segment;
mod nvl_domain;
mod nvl_logical_partition;
mod nvl_partition;
mod nvlink_nmxc_endpoints;
mod operating_system;
mod os_image;
mod ping;
mod power_shelf;
mod rack;
mod redfish;
mod resource_pool;
mod rms;
mod route_server;
mod rpc;
mod scout_stream;
mod secrets;
mod set;
mod site_explorer;
mod site_prefix;
mod sku;
mod spx_maintenance;
mod spx_partition;
mod ssh;
mod switch;
mod table_utils;
mod tenant;
mod tenant_keyset;
#[cfg(test)]
mod test_expected_component_patch;
#[cfg(test)]
mod test_support;
mod tpm_ca;
mod trim_table;
mod version;
mod vpc;
mod vpc_peering;
mod vpc_prefix;

fn invalid_machine_id() -> String {
    "INVALID_MACHINE".to_string()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> color_eyre::Result<()> {
    // This is a user-facing CLI, so keep error output to the message chain.
    // The source-code `Location:` block and the "run with RUST_BACKTRACE"
    // footer are developer noise on an expected failure (a missing flag, an
    // unreachable BMC); a backtrace is still captured when RUST_BACKTRACE is set.
    color_eyre::config::HookBuilder::default()
        .display_location_section(false)
        .display_env_section(false)
        .install()?;

    let config = CliOptions::load();
    if config.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }
    if config.format == OutputFormat::Csv
        && matches!(
            config.commands.as_ref(),
            Some(CliCommand::Vpc(
                vpc::Cmd::RoutingState(_)
                    | vpc::Cmd::ChangeRoutingProfile(_)
                    | vpc::Cmd::ReleaseInactiveVni(_)
            ))
        )
    {
        CliOptions::command()
            .error(
                clap::error::ErrorKind::ArgumentConflict,
                "--format csv is not supported for VPC routing commands; use ascii-table, json, or yaml",
            )
            .exit();
    }
    if let Some(CliCommand::Vpc(command)) = &config.commands
        && command.requires_interactive_confirmation()
        && !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal())
    {
        CliOptions::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "--if-version-match is required unless stdin and stderr are terminals; supply the original observed version for scripts or repeated requests",
            )
            .exit();
    }
    let file_config = get_config_from_file();

    // Log level is set from, in order of preference:
    // 1. `--debug N` on cmd line
    // 2. RUST_LOG environment variable
    // 3. Level::Info
    let mut env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
        .add_directive("tower=warn".parse()?)
        .add_directive("rustls=warn".parse()?)
        .add_directive("hyper=info".parse()?)
        .add_directive("h2=warn".parse()?);
    if config.debug != 0 {
        env_filter = env_filter.add_directive(
            match config.debug {
                1 => LevelFilter::DEBUG,
                _ => LevelFilter::TRACE,
            }
            .into(),
        );
    }
    tracing_subscriber::registry()
        .with(fmt::Layer::default().compact().with_writer(std::io::stderr))
        .with(env_filter)
        .try_init()?;

    if let Some(CliCommand::Redfish(ref ra)) = config.commands {
        // Redfish talks straight to a BMC, so it's handled here — before the
        // API client is built — rather than via the ctx-based dispatch below.
        // (Browsing a Redfish tree through the API server is a separate
        // top-level command, `browse redfish`, which does not need a BMC.)
        // --address is clap-`required` on RedfishAction; --username/--password
        // are optional.
        return redfish::action(ra.clone()).await;
    }
    if let Some(CliCommand::Rms(ref rms)) = config.commands {
        // do rms same as redfish above
        return rms::action(rms.clone(), &config).await;
    }

    let url = get_api_url(config.api_url, file_config.as_ref());
    // A root CA from the flag, env var or config file is one the user chose
    // for this server; an auto-discovered default may not match it.
    let root_ca_explicit = config.root_ca_path.is_some()
        || file_config
            .as_ref()
            .is_some_and(|f| f.root_ca_path.is_some());
    let root_ca_path = get_root_ca_path(config.root_ca_path, file_config.as_ref());

    let command = match config.commands {
        None => {
            return Ok(CliOptions::command().print_long_help()?);
        }
        Some(s) => s,
    };

    // `version` calls forge/Version which allows anonymous access, so no
    // client cert is needed. All other commands authenticate via the admin
    // CLI client cert (trusted-certificate principal).
    let is_version = matches!(command, CliCommand::Version(_));
    let client_cert = if is_version {
        None
    } else {
        Some(get_client_cert_info(
            config.client_cert_path,
            config.client_key_path,
            file_config.as_ref(),
        ))
    };

    let proxy = get_proxy_info()?;

    let mut client_config = ForgeClientConfig::new(root_ca_path, client_cert);
    client_config.socks_proxy(proxy);
    // Without a client cert, `ForgeClientConfig::new` disables server
    // verification. `version` verifies the server only if the user explicitly
    // supplied a usable root CA; an auto-discovered default CA may not be the
    // issuer for this server, and a missing or malformed CA must not make
    // `version` fail. Even a usable CA may not match the server, so the
    // verified attempt is capped and backed by an unverified retry: version
    // info is never withheld over certificates.
    let mut version_fallback = None;
    let mut verified_retry = None;
    if is_version && root_ca_explicit {
        if client_config.root_ca_is_usable() {
            let mut verified = client_config.clone().require_tls_enforcement();
            if verified.enforce_tls {
                // The default connect retries and 60 x 10s retry loop would
                // stall the fallback for minutes.
                verified.request_timeout = Some(VERIFIED_VERSION_TIMEOUT);
                verified.connect_retries_max = Some(0);
                version_fallback = Some(version::VerificationFallback {
                    api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
                        &url,
                        &client_config,
                    ))),
                    root_ca_path: client_config.root_ca_path.clone(),
                });
                verified_retry = Some(RetryConfig {
                    retries: 0,
                    interval: Duration::from_secs(1),
                });
                client_config = verified;
            }
        } else if std::path::Path::new(&client_config.root_ca_path).exists() {
            tracing::warn!(
                path = %client_config.root_ca_path,
                "root CA contains no valid certificate; server certificate will not be verified"
            );
        } else {
            tracing::debug!(
                path = %client_config.root_ca_path,
                "root CA not found; server certificate will not be verified"
            );
        }
    }

    let mut api_config = ApiConfig::new(&url, &client_config);
    if let Some(retry_config) = verified_retry {
        api_config = api_config.with_retry_config(retry_config);
    }
    let ctx = RuntimeContext {
        api_client: ApiClient(ForgeApiClient::new(&api_config)),
        config: RuntimeConfig {
            format: config.format,
            request_timeout: client_config.request_timeout,
            page_size: config.internal_page_size,
            extended: config.extended,
            cloud_unsafe_op: config.cloud_unsafe_op,
            sort_by: config.sort_by,
        },
        output_file: get_output_file_or_stdout(config.output.as_deref()).await?,
    };

    let command = match (command, version_fallback) {
        (CliCommand::Version(opts), Some(fallback)) => {
            return Ok(version::run_verified_first(opts, ctx, fallback).await?);
        }
        (command, _) => command,
    };

    // Command to talk to Carbide API.
    match command {
        CliCommand::Attestation(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Backend(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::BmcMachine(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::BootInterface(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::BootOverride(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Credential(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ComponentManager(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ComputeAllocation(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::DevEnv(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Domain(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Dpa(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Dpu(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::DpuRemediation(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExpectedMachine(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExpectedPowerShelf(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExpectedRack(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExpectedRackGroup(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExpectedNvlinkDomain(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExpectedSwitch(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ExtensionService(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Firmware(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::GenerateCliDocs(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::GenerateMan(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::GenerateShellComplete(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Host(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::IbPartition(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Instance(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::InstanceType(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Inventory(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Ip(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Jump(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::LogicalPartition(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Machine(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::MachineInterfaces(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::MachineValidation(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ManagedHost(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ManagedSwitch(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Mlx(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::NetworkDevice(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::NetworkSecurityGroup(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::NetworkSegment(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::NvlinkNmxcEndpoints(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::NvlDomain(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::NvlPartition(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::SpxPartition(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::SpxMaintenance(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::IpxeTemplate(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::OsImage(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::OperatingSystem(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Ping(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::PowerShelf(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Rack(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ResourcePool(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::RouteServer(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::ScoutStream(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Secrets(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Set(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Ssh(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::SiteExplorer(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::SitePrefix(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Sku(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Switch(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Tenant(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::TenantKeySet(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::TpmCa(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::TrimTable(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Version(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Vpc(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::VpcPeering(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::VpcPrefix(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Dpf(cmd) => cmd.dispatch(ctx).await?,
        CliCommand::Browse(cmd) => cmd.dispatch(ctx).await?,
        // Redfish is handled before the API client is built (see above).
        CliCommand::Redfish(_) => unreachable!("redfish is dispatched before client init"),
        _ => return Err(eyre::eyre!("unsupported command")),
    }

    Ok(())
}

async fn get_output_file_or_stdout(
    output_filename: Option<&str>,
) -> Result<Box<dyn tokio::io::AsyncWrite + Unpin>, CarbideCliError> {
    let output: Box<dyn tokio::io::AsyncWrite + Unpin> = if let Some(filename) = output_filename {
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(filename)
            .await?;
        Box::new(file)
    } else {
        Box::new(tokio::io::stdout())
    };
    Ok(output)
}

pub(crate) trait IntoOnlyOne<T> {
    fn into_only_one_or_else<E, F: FnOnce(usize) -> E>(self, f: F) -> Result<T, E>;
}

impl<T> IntoOnlyOne<T> for Vec<T> {
    fn into_only_one_or_else<E, F: FnOnce(usize) -> E>(self, f: F) -> Result<T, E> {
        if self.len() != 1 {
            return Err(f(self.len()));
        }
        let Some(first) = self.into_iter().next() else {
            return Err(f(0));
        };
        Ok(first)
    }
}

/// Destination is an enum used to determine whether CLI output is going
/// to a file path or stdout.
enum Destination {
    Path(String),
    Stdout(),
}

/// cli_output is the generic function implementation used by the OutputResult
/// trait, allowing callers to pass a Serialize-derived struct and have it
/// print in either JSON or YAML.
fn cli_output<T: Serialize + ToTable>(
    input: T,
    format: &OutputFormat,
    destination: Destination,
) -> CarbideCliResult<()> {
    let output = match format {
        OutputFormat::Json => serde_json::to_string_pretty(&input)?,
        OutputFormat::Yaml => serde_yaml::to_string(&input)?,
        OutputFormat::AsciiTable => input
            .into_table()
            .map_err(|e| CarbideCliError::GenericError(e.to_string()))?,
        OutputFormat::Csv => {
            return Err(CarbideCliError::GenericError(String::from(
                "CSV not supported for measurement commands (yet)",
            )));
        }
    };

    match destination {
        Destination::Path(path) => {
            let mut file = File::create(path)?;
            file.write_all(output.as_bytes())?
        }
        Destination::Stdout() => println!("{output}"),
    }

    Ok(())
}
