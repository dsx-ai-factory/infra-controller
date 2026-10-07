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
use std::future::Future;
use std::io::{Cursor, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use ::rpc::DiscoveryInfo;
use ::rpc::forge_tls_client::ForgeClientConfig;
use ::rpc::machine_discovery::{DmiData, DpuData};
use carbide_host_support::agent_config::AgentConfig;
use carbide_host_support::hardware_enumeration::{
    enumerate_and_save_hardware, enumerate_hardware, load_hardware_from_cache,
};
use carbide_host_support::registration::register_machine;
use carbide_utils::arch::CpuArchitecture;
pub use command_line::{AgentCommand, AgentPlatformType, Options, RunOptions, WriteTarget};
use eyre::WrapErr;
use forge_tls::client_config::ClientCert;
use futures_util::TryStreamExt;
use mac_address::MacAddress;
use network_monitor::{NetworkPingerType, Ping};
use tokio::io::AsyncReadExt;
use version_compare::{Part, Version};

use crate::duppet::{SummaryFormat, SyncOptions};
use crate::health::HealthCheckParams;
use crate::host_machine_id::get_host_machine_id_retry;

pub mod dpu;

mod acl_rules;
pub mod agent_platform;
mod astra_weave;
mod command_line;
pub mod containerd;
mod dhcp;
mod dhcp_server_grpc_client;
mod ethernet_virtualization;
use carbide_uuid::machine::DpuMachineId;
pub use ethernet_virtualization::FPath;
pub mod extension_services;
mod fmds_client;

pub mod duppet;
mod hbn;
mod health;
mod host_machine_id;
mod instance_metadata_endpoint;
pub mod instrumentation;
pub mod lldp;
mod local_api;
mod machine_inventory_updater;
mod main_loop;
mod managed_files;
mod metadata_service;
mod mtu;
pub mod netlink;
pub mod network_monitor;
pub mod nvue; // pub so that integration tests can read nvue::PATH
mod ovs;
mod periodic_config_fetcher;
mod supplemental_config;
mod sysfs;
#[cfg(test)]
mod tests;
pub mod upgrade;
pub mod util;
pub mod weave_ew_vpc_client;
#[cfg(test)]
pub mod weave_ew_vpc_mock_server;

/// The minimum version of HBN that FMDS supports
pub const FMDS_MINIMUM_HBN_VERSION: &str = "1.5.0-doca2.2.0";

/// The minimum version of HBN that supports NVUE. Since NVUE is now the only
/// supported configuration path, DPUs running older HBN versions cannot be configured.
pub const NVUE_MINIMUM_HBN_VERSION: &str = "2.0.0-doca2.5.0";

/// Subdirectory holding a second copy of the trust anchor, and nothing else.
///
/// Co-located services need the root CA but must never see the machine private
/// key that lives beside it (issue #355). A container can only be given one or
/// the other: mounting the credentials directory exposes the key, and mounting
/// the CA file alone by `subPath` bind-mounts its inode, so the atomic rename
/// in `install_bootstrap_ca` would never reach a running consumer. A directory
/// containing only the CA gives both properties — no key, and renames resolve.
///
/// Placed beside whichever CA path is configured rather than at a fixed
/// location, so it tracks a deployment that moves the credentials directory —
/// token-mode fmds watches `<certsDir>/pub/<rootCaFile>` and would otherwise
/// block forever. With the defaults on both sides this resolves to
/// `/opt/forge/pub/forge_root.pem`.
const BOOTSTRAP_CA_PUBLIC_SUBDIR: &str = "pub";
const MOUNTED_BOOTSTRAP_CA_PATH: &str = "/var/run/secrets/nico-bootstrap-ca/ca.pem";
const MAX_BOOTSTRAP_CA_BYTES: usize = 1024 * 1024;
const BOOTSTRAP_CA_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const PEM_BEGIN_MARKER: &[u8] = b"-----BEGIN ";
const CERTIFICATE_PEM_BEGIN_MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";

async fn acquire_bootstrap_ca(
    source: command_line::InitContainerBootstrapCaSource,
    legacy_download_url: &url::Url,
) -> eyre::Result<Vec<u8>> {
    match source {
        command_line::InitContainerBootstrapCaSource::LegacyDownload => {
            download_bootstrap_ca(legacy_download_url).await
        }
        command_line::InitContainerBootstrapCaSource::Mounted => {
            read_bootstrap_ca_file(Path::new(MOUNTED_BOOTSTRAP_CA_PATH)).await
        }
    }
}

async fn read_bootstrap_ca_file(path: &Path) -> eyre::Result<Vec<u8>> {
    let file = tokio::fs::File::open(path)
        .await
        .wrap_err_with(|| format!("failed to open bootstrap CA at {}", path.display()))?;
    let mut contents = Vec::new();
    file.take((MAX_BOOTSTRAP_CA_BYTES + 1) as u64)
        .read_to_end(&mut contents)
        .await
        .wrap_err_with(|| format!("failed to read bootstrap CA at {}", path.display()))?;
    if contents.len() > MAX_BOOTSTRAP_CA_BYTES {
        eyre::bail!(
            "bootstrap CA at {} exceeds the {MAX_BOOTSTRAP_CA_BYTES}-byte size limit",
            path.display()
        );
    }

    Ok(contents)
}

async fn download_bootstrap_ca(url: &url::Url) -> eyre::Result<Vec<u8>> {
    download_bootstrap_ca_with_timeout(url, BOOTSTRAP_CA_DOWNLOAD_TIMEOUT).await
}

async fn download_bootstrap_ca_with_timeout(
    url: &url::Url,
    timeout: Duration,
) -> eyre::Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .wrap_err("failed to build bootstrap CA download client")?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .wrap_err_with(|| format!("failed to download bootstrap CA from {url}"))?
        .error_for_status()
        .wrap_err_with(|| format!("bootstrap CA download from {url} returned an error status"))?;

    if response
        .content_length()
        .is_some_and(|length| length > MAX_BOOTSTRAP_CA_BYTES as u64)
    {
        eyre::bail!("bootstrap CA from {url} exceeds the {MAX_BOOTSTRAP_CA_BYTES}-byte size limit");
    }

    let mut contents = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .wrap_err_with(|| format!("failed while reading bootstrap CA response from {url}"))?
    {
        let new_len = contents
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| eyre::eyre!("bootstrap CA size overflow"))?;
        if new_len > MAX_BOOTSTRAP_CA_BYTES {
            eyre::bail!(
                "bootstrap CA from {url} exceeds the {MAX_BOOTSTRAP_CA_BYTES}-byte size limit"
            );
        }
        contents.extend_from_slice(&chunk);
    }

    Ok(contents)
}

fn validate_bootstrap_ca(contents: &[u8]) -> eyre::Result<()> {
    if contents.len() > MAX_BOOTSTRAP_CA_BYTES {
        eyre::bail!("bootstrap CA exceeds the {MAX_BOOTSTRAP_CA_BYTES}-byte size limit");
    }
    if contains_unsupported_pem_begin_marker(contents) {
        eyre::bail!("bootstrap CA contains unsupported PEM material");
    }

    let items = rustls_pemfile::read_all(&mut Cursor::new(contents))
        .collect::<Result<Vec<_>, _>>()
        .wrap_err("bootstrap CA contains invalid PEM")?;

    let mut trust_anchors = rustls::RootCertStore::empty();
    for item in items {
        let rustls_pemfile::Item::X509Certificate(certificate) = item else {
            eyre::bail!("bootstrap CA contains non-certificate PEM material");
        };
        trust_anchors
            .add(certificate)
            .wrap_err("bootstrap CA contains an invalid certificate")?;
    }
    if trust_anchors.is_empty() {
        eyre::bail!("bootstrap CA contains no certificates");
    }

    Ok(())
}

fn contains_unsupported_pem_begin_marker(contents: &[u8]) -> bool {
    let mut search_offset = 0;
    while let Some(relative_offset) = contents[search_offset..]
        .windows(PEM_BEGIN_MARKER.len())
        .position(|marker| marker == PEM_BEGIN_MARKER)
    {
        let begin_offset = search_offset + relative_offset;
        let line_end = contents[begin_offset..]
            .iter()
            .position(|byte| matches!(*byte, b'\n' | b'\r'))
            .map_or(contents.len(), |line_end| begin_offset + line_end);
        if &contents[begin_offset..line_end] != CERTIFICATE_PEM_BEGIN_MARKER {
            return true;
        }
        search_offset = begin_offset + PEM_BEGIN_MARKER.len();
    }
    false
}

fn install_bootstrap_ca(contents: &[u8], output_path: &Path) -> eyre::Result<()> {
    // Validate before creating a temporary file so a bad source cannot disturb
    // the last known-good trust anchor.
    validate_bootstrap_ca(contents)?;

    let parent = output_path.parent().ok_or_else(|| {
        eyre::eyre!(
            "bootstrap CA output path has no parent: {}",
            output_path.display()
        )
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).wrap_err_with(|| {
        format!(
            "failed to create temporary bootstrap CA in {}",
            parent.display()
        )
    })?;
    temporary
        .write_all(contents)
        .wrap_err("failed to write temporary bootstrap CA")?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))
        .wrap_err("failed to set bootstrap CA permissions")?;
    temporary
        .as_file()
        .sync_all()
        .wrap_err("failed to sync temporary bootstrap CA")?;
    temporary.persist(output_path).map_err(|error| {
        eyre::eyre!(
            "failed to atomically install bootstrap CA at {}: {}",
            output_path.display(),
            error.error
        )
    })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .wrap_err_with(|| format!("failed to sync bootstrap CA directory {}", parent.display()))?;

    Ok(())
}

async fn provision_bootstrap_ca(
    options: &command_line::InitContainerOptions,
    root_ca_path: &str,
) -> eyre::Result<()> {
    let contents =
        acquire_bootstrap_ca(options.bootstrap_ca_source, &options.bootstrap_ca_url).await?;
    // Installed where the agent will later look for it, rather than at a
    // constant of its own. The two were equal by coincidence -- `[forge-system]
    // root-ca` defaults to the same path -- and nothing kept them that way, so
    // a deployment that moved the CA would have had the init container write
    // one place and every reader look in another.
    let output_path = Path::new(root_ca_path);
    install_bootstrap_ca(&contents, output_path)?;
    publish_bootstrap_ca(&contents, output_path)
}

/// Mirrors the trust anchor into a `pub/` subdirectory beside `source_ca_path`
/// for key-less consumers. Same atomic install, so a consumer with the
/// directory mounted picks up a replacement without restarting.
///
/// Derived from the configured CA rather than a constant: token-mode fmds
/// waits on `<certsDir>/pub/<rootCaFile>`, so a deployment that moves the
/// credentials directory would otherwise have the agent publish to the default
/// location while fmds watched the configured one — leaving its init container
/// blocked forever with nothing to say why.
fn publish_bootstrap_ca(contents: &[u8], source_ca_path: &Path) -> eyre::Result<()> {
    let dir = source_ca_path
        .parent()
        .ok_or_else(|| eyre::eyre!("CA path has no parent: {}", source_ca_path.display()))?;
    let file_name = source_ca_path
        .file_name()
        .ok_or_else(|| eyre::eyre!("CA path has no file name: {}", source_ca_path.display()))?;
    let parent = dir.join(BOOTSTRAP_CA_PUBLIC_SUBDIR);
    std::fs::create_dir_all(&parent)
        .wrap_err_with(|| format!("failed to create public CA directory {}", parent.display()))?;
    install_bootstrap_ca(contents, &parent.join(file_name))
}

/// How often the published copy of the trust anchor is reconciled against the
/// configured one. Matches the API listener's own reload cadence, so a rotation
/// reaches key-less consumers on roughly the same clock as it reaches the
/// listener and the token validator.
const CA_REPUBLISH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Copies the configured trust anchor into its `pub/` mirror when the two
/// differ.
///
/// Publishing only at startup is not enough. `pub/` is the only trust anchor a
/// token-mode consumer has — it does not mount the credentials directory — so a
/// CA rotated while the agent is running would leave fmds pinned to the old
/// issuer indefinitely, and it would stop being able to verify the API the
/// moment that issuer is retired. The API listener re-reads the same material
/// every few minutes; the published copy has to keep pace.
///
/// Compares before writing so a steady state costs a read rather than an atomic
/// rename every interval, and best-effort throughout: a missing or unreadable
/// CA is the pre-registration case, and a failed write leaves the previous copy
/// in place for the next pass.
fn republish_bootstrap_ca_if_changed(root_ca_path: &str) {
    let source = Path::new(root_ca_path);
    let Ok(contents) = std::fs::read(source) else {
        tracing::debug!(
            target: "node_auth",
            path = %root_ca_path,
            "node-auth: no root CA to publish for co-located services yet"
        );
        return;
    };

    let published = source
        .parent()
        .zip(source.file_name())
        .map(|(dir, name)| dir.join(BOOTSTRAP_CA_PUBLIC_SUBDIR).join(name));
    if let Some(published) = &published
        && std::fs::read(published).is_ok_and(|existing| existing == contents)
    {
        return;
    }

    match publish_bootstrap_ca(&contents, source) {
        Ok(()) => tracing::info!(
            target: "node_auth",
            path = %root_ca_path,
            "node-auth: published the root CA for co-located services"
        ),
        Err(error) => tracing::warn!(
            target: "node_auth",
            %error,
            "node-auth: could not publish the root CA for co-located services"
        ),
    }
}

pub async fn start(cmdline: command_line::Options) -> eyre::Result<()> {
    if cmdline.version {
        println!("{}", carbide_version::version!());
        return Ok(());
    }

    let (agent, path) = match cmdline.config_path {
        // normal production case
        None => (AgentConfig::default(), "default".to_string()),
        // development overrides
        Some(config_path) => (
            AgentConfig::load_from(&config_path).wrap_err(format!(
                "error loading agent configuration from {}",
                config_path.display()
            ))?,
            config_path.display().to_string(),
        ),
    };
    agent
        .machine_identity
        .validate()
        .map_err(|e| eyre::eyre!("invalid [machine-identity] in agent config: {e}"))?;
    tracing::info!(config_path = path.as_str(), ?agent, "Using configuration");

    if agent.machine.is_fake_dpu {
        tracing::warn!("Pretending local host is a DPU. Dev only.");
    }

    // Published once here so it exists before anything else starts, then kept
    // current by `main_loop::run_single_iteration`, which reconciles it beside
    // certificate renewal.
    republish_bootstrap_ca_if_changed(&agent.forge_system.root_ca);

    // Node-auth (#355): the agent is the only process on the DPU that holds
    // the machine key. This minter signs bearer JWTs for the agent's own API
    // calls AND backs the local API socket that brokers tokens to co-located
    // services (fmds, ...). Ignored by the API unless [node_auth] is enabled.
    let node_jwt_minter = ::rpc::node_jwt::NodeJwtMinter::new(
        agent.forge_system.client_cert.clone(),
        agent.forge_system.client_key.clone(),
    );
    let forge_client_config = Arc::new(
        ForgeClientConfig::new(
            agent.forge_system.root_ca.clone(),
            Some(ClientCert {
                cert_path: agent.forge_system.client_cert.clone(),
                key_path: agent.forge_system.client_key.clone(),
            }),
        )
        .with_token_provider(node_jwt_minter.clone())
        .use_mgmt_vrf()?,
    );

    match cmdline.cmd {
        None => {
            tracing::error!("Missing cmd. Try `forge-dpu-agent --help`");
        }

        // "run" is the normal command
        Some(AgentCommand::Run(options)) => {
            if options.skip_upgrade_check {
                tracing::warn!("Upgrades disabled. Dev only");
            }

            let local_api = {
                let minter = node_jwt_minter.clone();
                let socket_path = agent.forge_system.local_api_socket.clone();
                async move {
                    loop {
                        if let Err(error) = local_api::serve(minter.clone(), &socket_path).await {
                            tracing::warn!(target: "node_auth", %error, "node-auth: agent local API server failed; retrying");
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    }
                }
            };

            let Registration {
                machine_id,
                factory_mac_address,
            } = match options.override_machine_id {
                // Normal case
                None => register(&agent, &options.agent_platform_type)
                    .await
                    .wrap_err("registration error")?,
                // Dev / test override
                Some(machine_id) => Registration {
                    machine_id,
                    factory_mac_address: "11:22:33:44:55:66".parse().unwrap(),
                },
            };
            let main_loop_result = tokio::select! {
                result = main_loop::setup_and_run(
                    machine_id,
                    factory_mac_address,
                    forge_client_config,
                    agent,
                    *options,
                ) => result.wrap_err("main_loop error exit"),
                () = local_api => unreachable!("the agent local API retry loop cannot finish"),
            };

            main_loop_result?;
            tracing::info!("Agent exit");
        }

        // enumerate hardware and exit
        Some(AgentCommand::Hardware(options)) => {
            let info = match options.agent_platform_type {
                // Containerized: read the snapshot written by the init container
                AgentPlatformType::Containerized => load_hardware_from_cache()?,
                // No container mode, just plain old dpu-agent running as a service on DPU OS.
                AgentPlatformType::DpuOs => enumerate_hardware()?,
            };
            let string_result = serde_json::to_string_pretty(&info)?;
            match options.output_file.as_ref() {
                Some(output_file) => tokio::fs::write(output_file.as_path(), string_result).await?,
                None => {
                    // print to stderr so it can be re-directed to a file without logs
                    eprintln!("{string_result}");
                }
            }
        }

        // Init-container entry point: provision the CA + snapshot hardware to the shared volume.
        // Output path is fixed (HW_CACHE_PATH) so the main container can always find it.
        Some(AgentCommand::InitContainer(options)) => {
            provision_bootstrap_ca(&options, &agent.forge_system.root_ca).await?;
            enumerate_and_save_hardware().await?;
            util::save_host_nameservers()?;
        }

        // One-off health check.
        // Does not take into account tenant ignored peers, so it can fail when the real check would
        // succeed.
        // Same thing as above with respect to "minimum healthy links" -- we don't have it here so
        // it may fail when the real one would succeed for single-port setups.
        // This also only works with the newest HBN as the ifc suffix is hard coded to the new version
        Some(AgentCommand::Health) => {
            let health_report = health::health_check(HealthCheckParams {
                hbn_root: &agent.hbn.root_dir,
                host_routes: &[],
                has_changed_configs: false,
                min_healthy_links: 2,
                route_servers: &[],
                // Standalone checks lack managed-host config to establish that
                // the FNN IPv6 underlay is active.
                should_check_ipv6_unicast: false,
                hbn_device_names: HBNDeviceNames::hbn_23(),
                include_dhcp_server: false,
                run_restricted_mode_check: true,
            })
            .await;
            println!("{}", serde_json::to_string_pretty(&health_report)?);
        }

        Some(AgentCommand::LldpNeighbors(options)) => {
            let neighbors = collect_lldp_neighbors(&options.agent_platform_type).await?;
            println!("{neighbors:#?}");
        }

        Some(AgentCommand::SidecarMode) => run_lldp_sidecar().await,

        // One-off network monitor check.
        // dumps JSON-formatted peer DPU network reachability and latency status
        Some(AgentCommand::Network(options)) => {
            let machine_id = register(&agent, &AgentPlatformType::DpuOs)
                .await
                .wrap_err("network check machine registration error")?
                .machine_id;

            let pinger_type = match options.network_pinger_type {
                Some(pinger_type) => pinger_type,
                None => NetworkPingerType::OobNetBind,
            };

            tracing::info!(%pinger_type, "Using pinger");
            let pinger: Arc<dyn Ping> = Arc::from(pinger_type);

            let mut network_monitor =
                network_monitor::NetworkMonitor::new(machine_id, None, pinger);

            network_monitor
                .run_onetime(&agent.forge_system.api_server, &forge_client_config)
                .await;
        }

        // The duppet subcommand does a single duppet run for duppet-managed files.
        Some(AgentCommand::Duppet(duppet_options)) => {
            let parsed_format = match duppet_options.summary_format.as_str() {
                "json" => SummaryFormat::Json,
                "yaml" => SummaryFormat::Yaml,
                _ => SummaryFormat::PlainText,
            };
            let sync_options = SyncOptions {
                dry_run: duppet_options.dry_run,
                quiet: duppet_options.quiet,
                no_color: duppet_options.no_color,
                summary_format: parsed_format,
            };

            // Since the duppet sync also syncs out the otel machine_id and
            // host_machine_id files, we need to make a registration call to
            // get the machine_id, and a carbide api request to get the
            // host_machine_id.
            let Registration { machine_id, .. } = register(&agent, &AgentPlatformType::DpuOs)
                .await
                .wrap_err("registration error")?;

            let forge_api_server = agent.forge_system.api_server.clone();
            let periodic_config_fetcher = periodic_config_fetcher::PeriodicConfigFetcher::new(
                periodic_config_fetcher::PeriodicConfigFetcherConfig {
                    config_fetch_interval: Duration::from_secs(
                        agent.period.network_config_fetch_secs,
                    ),
                    machine_id,
                    forge_api: forge_api_server.clone(),
                    forge_client_config: Arc::clone(&forge_client_config),
                },
            )
            .await;

            let host_machine_id = match get_host_machine_id_retry(
                &agent,
                &periodic_config_fetcher,
                Arc::clone(&forge_client_config),
                &forge_api_server,
            )
            .await
            {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(error = ?e, "Failed to get host machine ID after retries");
                    return Err(e);
                }
            };

            managed_files::main_sync(sync_options, &machine_id, &host_machine_id);
        }

        // Output a templated file
        // Normally this is (will be) done when receiving requests from carbide-api
        Some(AgentCommand::Write(target)) => match target {
            // Legacy ETV write targets are no longer supported
            WriteTarget::Frr(_) | WriteTarget::Interfaces(_) | WriteTarget::Dhcp(_) => {
                eyre::bail!(
                    "legacy ETV write targets (frr, interfaces, dhcp) are no longer supported. use 'write nvue' instead"
                );
            }

            // Example:
            // forge-dpu-agent write nvue
            // --path /tmp/startup.yaml
            // --loopback-ip 10.0.0.1
            // --asn 65535
            // --dpu-hostname bob
            // --ct-name ct_name
            // --ct-l3vni l3vnihere
            // --ct-vrf-loopback 10.0.0.2
            // --uplinks up1,up2
            // --route-servers 10.217.126.5  # comma separated list
            // --dhcp-servers 10.217.126.2  # comma separated list
            // --l3-domain 4096,10.0.0.1,svi  # repeat for multiple
            // --ct-external-access 4096  # comma separated list
            // --ct-port-config '{"interface_name": "if1", "vlan": 123, "vni": 456, "gateway_cidr": "10.0.0.100/32"}' # repeated for multiple
            // --hbn_version 1.5.0-doca2.2.0
            WriteTarget::Nvue(opts) => {
                let mut port_configs = Vec::with_capacity(opts.ct_port_config.len());
                for net_json in opts.ct_port_config {
                    let c: nvue::PortConfig = serde_json::from_str(&net_json)?;
                    port_configs.push(c);
                }

                let network_security_policy_override_rules = opts
                    .network_security_policy_override_rule
                    .into_iter()
                    .map(|r| serde_json::from_str::<nvue::NetworkSecurityGroupRule>(&r))
                    .collect::<Result<Vec<nvue::NetworkSecurityGroupRule>, _>>()?;

                let additional_route_target_imports = opts
                    .additional_fnn_route_target_import
                    .into_iter()
                    .map(|r| serde_json::from_str::<nvue::RouteTargetConfig>(&r))
                    .collect::<Result<Vec<nvue::RouteTargetConfig>, _>>()?;

                let network_security_groups = opts
                    .network_security_group
                    .into_iter()
                    .map(|r| serde_json::from_str::<nvue::NetworkSecurityGroup>(&r))
                    .collect::<Result<Vec<nvue::NetworkSecurityGroup>, _>>()?;

                let access_vlans = opts
                    .vlan
                    .into_iter()
                    .map(|s| {
                        let mut parts = s.split(',');
                        let vlan_id = parts.next().unwrap().parse().unwrap();
                        let ip = parts.next().unwrap().to_string();
                        nvue::VlanConfig {
                            vlan_id,
                            network: ip.clone() + "/32",
                            ip,
                            ipv6_vlan_config: None,
                        }
                    })
                    .collect();

                let conf = nvue::NvueConfig {
                    is_fnn: opts.is_fnn,
                    site_global_vpc_vni: opts.site_global_vpc_vni,
                    vpc_virtualization_type: opts.virtualization_type,
                    hbn_version: opts.hbn_version,
                    use_admin_network: true,
                    tenancy_enabled: true,
                    loopback_ip: opts.loopback_ip,
                    loopback_ip_v6: opts.loopback_ip_v6,
                    asn: opts.asn,
                    datacenter_asn: opts.datacenter_asn,
                    anycast_site_prefixes: vec!["5.255.255.0/24".to_string()],
                    tenant_host_asn: Some(65100),
                    common_internal_route_target: opts
                        .common_internal_route_target
                        .map(|r| serde_json::from_str::<nvue::RouteTargetConfig>(&r))
                        .transpose()?,
                    additional_route_target_imports,
                    dpu_hostname: opts.dpu_hostname,
                    dpu_search_domain: "".to_string(),
                    uplinks: opts.uplinks,
                    dhcp_servers: opts.dhcp_servers,
                    deny_prefixes: vec![],
                    site_fabric_prefixes: vec![],
                    use_vpc_isolation: true,
                    network_security_policy_override_rules,
                    stateful_acls_enabled: opts.stateful_acls_enabled,
                    route_servers: opts.route_servers,
                    l3_domains: vec![],
                    ct_vrf_name: opts.ct_vrf_name,
                    ct_l3_vni: opts.ct_l3vni,
                    ct_vrf_loopback: opts.ct_vrf_loopback,
                    ct_port_configs: port_configs,
                    ct_access_vlans: access_vlans,
                    ct_routing_profile: opts
                        .ct_routing_profile
                        .map(|r| serde_json::from_str::<nvue::RoutingProfile>(&r))
                        .transpose()?,
                    network_security_groups,
                    bgp_leaf_session_password: opts.bgp_leaf_session_password,
                    is_dpu_os: true,
                    fmds_gateway_vlan: None,
                };
                let contents = nvue::build(conf)?;
                std::fs::write(&opts.path, contents)?;
                println!("Wrote {}", opts.path);
            }
        },
    }
    Ok(())
}

/// Collect LLDP neighbors from the source appropriate for this agent platform.
///
/// Periodic topology publishers must retain their last successful topology when
/// this returns an error, then log the failure and retry on their normal interval.
pub async fn collect_lldp_neighbors(
    platform_type: &AgentPlatformType,
) -> carbide_host_support::lldp_collector::LldpCollectorResult<
    Vec<carbide_host_support::lldp_collector::LldpNeighbor>,
> {
    collect_lldp_neighbors_with_collectors(
        platform_type,
        carbide_host_support::lldp_collector::collect_lldp_neighbors,
        carbide_host_support::lldp_collector::collect_lldp_neighbors_from_snapshot,
    )
    .await
}

async fn collect_lldp_neighbors_with_collectors<DpuCollector, DpuFuture>(
    platform_type: &AgentPlatformType,
    dpu_os_collector: DpuCollector,
    containerized_collector: impl FnOnce() -> carbide_host_support::lldp_collector::LldpCollectorResult<
        Vec<carbide_host_support::lldp_collector::LldpNeighbor>,
    >,
) -> carbide_host_support::lldp_collector::LldpCollectorResult<
    Vec<carbide_host_support::lldp_collector::LldpNeighbor>,
>
where
    DpuCollector: FnOnce() -> DpuFuture,
    DpuFuture: Future<
        Output = carbide_host_support::lldp_collector::LldpCollectorResult<
            Vec<carbide_host_support::lldp_collector::LldpNeighbor>,
        >,
    >,
{
    match platform_type {
        AgentPlatformType::DpuOs => dpu_os_collector().await,
        AgentPlatformType::Containerized => containerized_collector(),
    }
}

const LLDP_SIDECAR_REFRESH_INTERVAL: Duration = Duration::from_secs(120);
const LLDP_SIDECAR_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Continuously refreshes the LLDP snapshot consumed by the containerized agent.
pub async fn run_lldp_sidecar() {
    run_lldp_sidecar_loop(
        carbide_host_support::lldp_collector::publish_lldp_snapshot,
        None,
    )
    .await;
}

async fn run_lldp_sidecar_loop<Capture, CaptureFuture>(
    mut capture: Capture,
    attempt_limit: Option<usize>,
) where
    Capture: FnMut() -> CaptureFuture,
    CaptureFuture: Future<Output = carbide_host_support::lldp_collector::LldpCollectorResult<()>>,
{
    let mut attempts = 0;
    loop {
        let delay = match capture().await {
            Ok(()) => LLDP_SIDECAR_REFRESH_INTERVAL,
            Err(error) => {
                tracing::warn!(
                    %error,
                    retry_interval_seconds = LLDP_SIDECAR_RETRY_INTERVAL.as_secs(),
                    "Could not refresh LLDP sidecar snapshot; retaining the last successful snapshot"
                );
                LLDP_SIDECAR_RETRY_INTERVAL
            }
        };
        attempts += 1;
        if attempt_limit.is_some_and(|limit| attempts >= limit) {
            return;
        }
        tokio::time::sleep(delay).await;
    }
}

struct Registration {
    machine_id: DpuMachineId,
    factory_mac_address: MacAddress,
}

#[derive(Clone)]
struct HBNDeviceNames {
    uplinks: [&'static str; 2],
    reps: [&'static str; 2],
    virt_rep_begin: &'static str,
    sfs: [&'static str; 2],
    sf_id: &'static str,
}

impl HBNDeviceNames {
    fn pre_23() -> HBNDeviceNames {
        HBNDeviceNames {
            uplinks: ["p0_sf", "p1_sf"],
            reps: ["pf0hpf_sf", "pf1hpf_sf"],
            virt_rep_begin: "pf0vf",
            sfs: ["pf0dpu0_sf", "pf0dpu2_sf"],
            sf_id: "_sf",
        }
    }

    fn hbn_23() -> HBNDeviceNames {
        HBNDeviceNames {
            uplinks: ["p0_if", "p1_if"],
            reps: ["pf0hpf_if", "pf1hpf_if"],
            virt_rep_begin: "pf0vf",
            sfs: ["pf0dpu1", "pf0dpu3"],
            sf_id: "_if",
        }
    }
    fn new(hbn_version: Version) -> Self {
        let min_version: Version = Version::from_parts(
            "2.3.0-doca2.8.0",
            vec![
                Part::Number(2),
                Part::Number(3),
                Part::Number(0),
                Part::Text("doca"),
                Part::Number(2),
                Part::Number(8),
                Part::Number(0),
            ],
        );
        if hbn_version < min_version {
            HBNDeviceNames::pre_23()
        } else {
            HBNDeviceNames::hbn_23()
        }
    }
    fn build_virt(&self, virt_rep_id: u32) -> String {
        format!("{}{}{}", self.virt_rep_begin, virt_rep_id, self.sf_id)
    }
}

/// Discover hardware, register DPU with carbide-api, and return machine id.
async fn register(
    agent: &AgentConfig,
    platform_type: &AgentPlatformType,
) -> Result<Registration, eyre::Report> {
    // Use synthetic hardware in fake-DPU mode so local tests do not depend on host devices.
    let hardware_info = if agent.machine.is_fake_dpu {
        fake_dpu_discovery_info()
    } else {
        match platform_type {
            AgentPlatformType::Containerized => {
                load_hardware_from_cache().wrap_err("load_hardware_from_cache failed")
            }
            _ => enumerate_hardware().wrap_err("enumerate_hardware failed"),
        }?
    };

    let factory_mac_address: MacAddress = match hardware_info.dpu_info.as_ref() {
        Some(dpu_info) => dpu_info.factory_mac_address.parse().map_err(|e| {
            eyre::eyre!(
                "failed to parse factory MAC address from DPU info: {} (err: {})",
                dpu_info.factory_mac_address,
                e
            )
        })?,
        None => eyre::bail!("missing DPU info, should be impossible"),
    };

    let (registration_data, ..) = register_machine(
        &agent.forge_system.api_server,
        agent.forge_system.root_ca.clone(),
        agent.machine.interface_id,
        hardware_info,
        true,
        carbide_host_support::registration::DiscoveryRetry {
            secs: agent.period.discovery_retry_secs,
            max: agent.period.discovery_retries_max,
        },
        false,
        !agent.machine.is_fake_dpu,
        ::rpc::MachineDiscoveryReporter::DpuAgent,
        Some(carbide_version::v!(build_version).to_string()),
    )
    .await?;

    let machine_id = DpuMachineId::try_from(registration_data.machine_id)
        .context("invalid machine_id returned from DiscoverMachine")?;
    tracing::info!(%machine_id, %factory_mac_address, "Successfully discovered machine");

    Ok(Registration {
        machine_id,
        factory_mac_address,
    })
}

/// Builds synthetic DPU discovery information for fake-DPU tests and local development.
fn fake_dpu_discovery_info() -> DiscoveryInfo {
    // Start with stable DMI values so registration does not depend on host hardware.
    let mut hardware_info = DiscoveryInfo {
        dmi_data: Some(DmiData {
            board_name: "BlueField SoC".to_string(),
            product_name: "BlueField SoC".to_string(),
            product_serial: "Stable Local Dev serial".to_string(),
            sys_vendor: "Nvidia".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Reuse the existing fake-DPU overlay for DPU identity and architecture fields.
    fill_fake_dpu_info(&mut hardware_info);
    tracing::debug!("Successfully built fake DPU discovery data");

    hardware_info
}

pub fn pretty_cmd(c: &Command) -> String {
    format!(
        "{} {}",
        c.get_program().to_string_lossy(),
        c.get_args()
            .map(|x| x.to_string_lossy())
            .collect::<Vec<std::borrow::Cow<'_, str>>>()
            .join(" ")
    )
}

// fill_fake_dpu_info will take a pre-populated DiscoveryInfo
// from enumerate_hardware (which also adds things like
// discovered cores [from your local machine] and such),
// and injects data to mock your machine to look like
// a DPU. This is intended for use with unit testing
// and local development only.
fn fill_fake_dpu_info(hardware_info: &mut DiscoveryInfo) {
    hardware_info.machine_type = CpuArchitecture::Aarch64.to_string(); // old
    hardware_info.machine_arch = Some(rpc::utils::cpu_architecture_to_rpc(
        CpuArchitecture::Aarch64,
    )); // new
    if let Some(dmi) = hardware_info.dmi_data.as_mut() {
        dmi.board_name = "BlueField SoC".to_string();
        if dmi.product_serial.is_empty() {
            // Older Dell Precision 5760 don't have any serials
            dmi.product_serial = "Stable Local Dev serial".to_string();
        }
    }
    hardware_info.dpu_info = Some(DpuData {
        part_number: "1".to_string(),
        part_description: "1".to_string(),
        product_version: "1".to_string(),
        factory_mac_address: "11:22:33:44:55:66".to_string(),
        firmware_version: "1".to_string(),
        firmware_date: "01/01/1970".to_string(),
        switches: vec![],
    });
}

/// Return Some(s) if s is not empty, otherwise None. Used to avoid repetition
/// when dealing with gRPC-sourced fields that use an empty string to indicate
/// an absent value.
pub fn get_non_empty_str<S>(s: &S) -> Option<&str>
where
    S: AsRef<str>,
{
    let s = s.as_ref();
    if s.is_empty() { None } else { Some(s) }
}
