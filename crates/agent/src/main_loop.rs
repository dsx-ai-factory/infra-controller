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

use std::collections::HashSet;
use std::ffi::OsStr;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::IpAddr;
use std::ops::Add;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ::rpc::forge::ManagedHostNetworkConfigResponse;
use ::rpc::forge_tls_client::ForgeClientConfig;
use ::rpc::{forge as rpc, forge_tls_client};
use carbide_host_support::agent_config::AgentConfig;
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_rpc_utils::dhcp::{DhcpTimestamps, DhcpTimestampsFilePath};
use carbide_systemd::systemd;
use carbide_uuid::machine::MachineId;
use eyre::WrapErr;
use forge_certs::cert_renewal::ClientCertRenewer;
use forge_dpu_remediation::remediation::{MachineInfo, RemediationExecutor};
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use prost::Message as _;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::log::error;
use version_compare::Version;

use crate::command_line::HbnConfigMode;
use crate::dpu::DpuNetworkInterfaces;
use crate::dpu::interface::Interface;
use crate::dpu::route::{DpuRoutePlan, IpRoute, Route};
use crate::duppet::{SummaryFormat, SyncOptions};
use crate::ethernet_virtualization::{
    InterfaceTranslationMode, NvueClientContext, NvueUpdateFlavor, ServiceAddresses,
};
use crate::fmds_client::FmdsUpdater;
use crate::health::HealthCheckParams;
use crate::host_machine_id::get_host_machine_id_retry;
use crate::instrumentation::{create_metrics, get_dpu_agent_meter};
use crate::machine_inventory_updater::MachineInventoryUpdaterConfig;
use crate::network_monitor::{self, NetworkPingerType};
use crate::util::get_host_boot_timestamp;
use crate::{
    FMDS_MINIMUM_HBN_VERSION, HBNDeviceNames, NVUE_MINIMUM_HBN_VERSION, RunOptions, command_line,
    ethernet_virtualization, extension_services, get_non_empty_str, hbn, health,
    instance_metadata_endpoint, lldp, machine_inventory_updater, managed_files, mtu, netlink, nvue,
    periodic_config_fetcher, pretty_cmd, sysfs, upgrade,
};

// Main loop when running in daemon mode
// Before going into its main loop functionality, the
// code first launches a periodic config fetcher thread.
// The periodic config fetcher uses a grpc call to
// carbide to get network config information as well as
// instance metadata information and stores it. The main loop and the instance
// metadata service use the information fetched be the periodic fetcher by reading
// the information stored by the periodic config fetcher.
pub async fn setup_and_run(
    machine_id: MachineId,
    factory_mac_address: MacAddress,
    forge_client_config: Arc<ForgeClientConfig>,
    agent_config: AgentConfig,
    options: command_line::RunOptions,
) -> eyre::Result<()> {
    systemd::notify_start().await?;
    tracing::info!(
        version = carbide_version::version!(),
        "Started forge-dpu-agent"
    );

    // Issue a *hopefully* one time reboot in the event that a VERY specific case where the dpu
    // is unable to load its 2.9.2 ATF/UEFI, due to a nasty bug that deleted the uefi cert db. This
    // is a manual way to load the ATF/UEFI, because by the time we get to the dpu agent actually
    // running, then we got past all the pre-reqs that would have gotten us out of the state where
    // we cannot load the ATF/UEFI, so we run these commands and reboot, and the next time we check
    // the ATF/UEFI is loaded.
    //
    // Once the fleet is all on 2.9.2, we can remove this ugly hack.
    if options.agent_platform_type.is_dpu_os() {
        hack_dpu_os_to_load_atf_uefi_with_specific_versions().await?;
    }

    let process_start_time = SystemTime::now();

    let forge_api_server = agent_config.forge_system.api_server.clone();
    // Setup client certificate renewal
    let client_cert_renewer =
        ClientCertRenewer::new(forge_api_server.clone(), Arc::clone(&forge_client_config));

    let machine_info = MachineInfo::new(machine_id);
    let remediation_executor = RemediationExecutor::new(
        forge_api_server.clone(),
        Arc::clone(&forge_client_config),
        machine_info,
    );
    tokio::task::spawn(async move {
        remediation_executor.run().await;
    });

    let instance_metadata_state = Arc::new(
        instance_metadata_endpoint::InstanceMetadataRouterStateImpl::new(
            machine_id,
            forge_api_server.clone(),
            Arc::clone(&forge_client_config),
            agent_config.machine_identity.clone(),
        )
        .map_err(|e| eyre::eyre!("failed to initialize instance metadata router state: {e}"))?,
    );

    let agent_meter = get_dpu_agent_meter();
    let metrics = create_metrics(agent_meter);

    if let Err(e) = crate::metadata_service::spawn_prometheus_metrics_server(
        agent_config.telemetry.metrics_address.clone(),
    ) {
        tracing::warn!(
            error = format!("{e:#}"),
            "Failed to start Prometheus /metrics endpoint"
        );
    }

    // And now set up our FMDS updater, which will either be our original
    // embedded server (which spins up a local listener within the DPU agent)
    // or will talk to an external FMDS server via gRPC (which is colocated
    // with the agent on the DPU).
    let fmds_updater = if let Some(ref fmds_addr) = options.fmds_grpc_server {
        tracing::info!(
            fmds_address = fmds_addr,
            "Using FmdsUpdater::External FMDS service"
        );
        match crate::fmds_client::FmdsGrpcClient::connect(
            fmds_addr,
            agent_config.machine_identity.clone(),
        )
        .await
        {
            Ok(fmds_client) => FmdsUpdater::External(Box::new(fmds_client)),
            Err(e) => {
                tracing::warn!(
                    "Failed to connect to external FMDS service: {e:#}, falling back to embedded"
                );
                FmdsUpdater::Embedded(instance_metadata_state.clone())
            }
        }
    } else {
        if options.enable_metadata_service {
            crate::metadata_service::spawn_metadata_service(
                agent_config.metadata_service.address.clone(),
                metrics.clone(),
                instance_metadata_state.clone(),
            )
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to run metadata service: {:#}", e);
            });
        }
        tracing::info!("Using FmdsUpdater::Embedded FMDS service");
        FmdsUpdater::Embedded(instance_metadata_state.clone())
    };

    // Some of these metrics only need to be set once, let's take care of them
    // now.
    match process_start_time.duration_since(UNIX_EPOCH) {
        Ok(time_since_epoch) => {
            let timestamp = time_since_epoch.as_secs();
            metrics.record_agent_start_time(timestamp);
        }
        Err(e) => {
            tracing::warn!("Error calculating process start timestamp: {e:#}");
        }
    }

    match get_host_boot_timestamp() {
        Ok(timestamp) => {
            metrics.record_machine_boot_time(timestamp);
        }
        Err(e) => {
            tracing::warn!("Error getting host boot timestamp: {e:#}");
        }
    }

    if options.agent_platform_type.is_dpu_os()
        && !agent_config.machine.is_fake_dpu
        && let Err(e) = crate::agent_platform::ensure_doca_containers().await
    {
        // The HBN container health check will notice this problem and
        // turn it into an unhealthy status that gets reported to the
        // API, so we're not going to do much more than log this for
        // now.
        tracing::error!("Couldn't ensure DOCA pods: {e}");
    }

    let fmds_minimum_hbn_version = Version::from(FMDS_MINIMUM_HBN_VERSION).ok_or(eyre::eyre!(
        "Unable to convert string: {FMDS_MINIMUM_HBN_VERSION} to Version"
    ))?;
    let nvue_minimum_hbn_version = Version::from(NVUE_MINIMUM_HBN_VERSION).ok_or(eyre::eyre!(
        "Unable to convert string: {NVUE_MINIMUM_HBN_VERSION} to Version"
    ))?;

    if options.agent_platform_type.is_dpu_os()
        && let Err(err) = crate::ovs::set_vswitchd_yield().await
    {
        tracing::warn!(%err, "Failed asking ovs_vswitchd to not use 100% of a CPU core. Non-fatal.");
        // We have eight cores. Letting ovs_vswitchd have one is OK.
    };

    let nvue_context = match options.hbn_config_mode {
        HbnConfigMode::ContainerExec => None,
        HbnConfigMode::NvueRest => {
            let nvue_client = nvue_client::NvueClient::new_https_from_env()?;
            let nvue_context = NvueClientContext::new(nvue_client);
            Some(nvue_context)
        }
    };

    let build_version = carbide_version::v!(build_version).to_string();

    let periodic_config_fetcher = periodic_config_fetcher::PeriodicConfigFetcher::new(
        periodic_config_fetcher::PeriodicConfigFetcherConfig {
            config_fetch_interval: Duration::from_secs(
                agent_config.period.network_config_fetch_secs,
            ),
            machine_id,
            forge_api: forge_api_server.clone(),
            forge_client_config: Arc::clone(&forge_client_config),
        },
    )
    .await;

    let host_machine_id = match get_host_machine_id_retry(
        &agent_config,
        &periodic_config_fetcher,
        Arc::clone(&forge_client_config),
        &forge_api_server,
    )
    .await
    {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("get_host_machine_id_retry() failed: {:?}", e);
            return Err(e);
        }
    };

    let duppet_options = SyncOptions {
        dry_run: false,
        quiet: false,
        no_color: false,
        summary_format: SummaryFormat::PlainText,
    };

    if options.agent_platform_type.is_dpu_os() {
        managed_files::main_sync(duppet_options, &machine_id, &host_machine_id);
    }

    if options.agent_platform_type.is_dpu_os()
        && let Err(e) = lldp::set_lldp_system_description(&machine_id)
    {
        tracing::warn!("Couldn't update LLDP system description: {e}")
    }

    let periodic_config_reader = periodic_config_fetcher.reader();

    let service_addrs = ServiceAddresses::build(
        &options.agent_platform_type,
        agent_config.machine.is_fake_dpu,
    )
    .await?;

    let inventory_updater_config = MachineInventoryUpdaterConfig {
        dpu_agent_version: build_version.clone(),
        update_inventory_interval: Duration::from_secs(agent_config.period.inventory_update_secs),
        machine_id,
        forge_api: forge_api_server.clone(),
        forge_client_config: Arc::clone(&forge_client_config),
        agent_platform_type: options.agent_platform_type.clone(),
    };

    // Get all DPU Ip addresses via gRPC call
    let (close_sender, mut close_receiver) = watch::channel(false);

    // Initialize network monitor and perform network check once
    let network_pinger_type = periodic_config_reader
        .net_conf_read()
        .as_ref()
        .as_ref()
        .and_then(|response| response.dpu_network_pinger_type.as_ref())
        .and_then(|value| NetworkPingerType::from_str(value).ok());

    let agent_meter = get_dpu_agent_meter();
    let network_monitor_metrics_state =
        crate::instrumentation::NetworkMonitorMetricsState::initialize(agent_meter, machine_id);

    let network_monitor_handle: Option<JoinHandle<()>> = match network_pinger_type {
        Some(pinger_type) => {
            tracing::debug!("Starting network monitor with {} pinger", pinger_type);
            let mut network_monitor = network_monitor::NetworkMonitor::new(
                machine_id,
                Some(network_monitor_metrics_state),
                Arc::from(pinger_type),
            );
            let forge_api_clone = forge_api_server.clone();
            let forge_client_config_clone = Arc::clone(&forge_client_config);
            let network_monitor_handle = tokio::spawn(async move {
                network_monitor
                    .run(
                        &forge_api_clone,
                        forge_client_config_clone,
                        &mut close_receiver,
                    )
                    .await
            });
            Some(network_monitor_handle)
        }
        None => {
            tracing::debug!(
                "No network pinger type provided from ManagedHostNetworkConfigResponse. Network monitor not started."
            );
            None
        }
    };

    // default to hbn 2.3 and above for the hbn device names. This will be properly set once the
    // HBN runtime container is online. This is set here initially so once it is read it can be properly
    // used in the event that hbn crashes and can no longer read the actual version of hbn
    let hbn_device_names = HBNDeviceNames::hbn_23();

    let extension_service_manager = extension_services::ExtensionServiceManager::platform_defaults(
        &options.agent_platform_type,
    );

    let dhcp_interface_translation_mode = options
        .dhcp_server_interface_prepend
        .as_ref()
        .map(|prefix| InterfaceTranslationMode::Prepend(prefix.clone()));

    let mut main_loop = MainLoop {
        forge_client_config,
        build_version,
        machine_id,
        periodic_config_reader,
        fmds_updater,
        client_cert_renewer,
        hbn_device_names,
        is_hbn_up: false,
        hbn_file_configs: hbn::HBNContainerFileConfigs::default(),
        seen_blank: false,
        has_logged_stable: false,
        version_check_time: std::time::Instant::now(),
        inventory_updater_time: std::time::Instant::now(),
        started_at: std::time::Instant::now(),
        inventory_updater_config,
        options,
        agent_config,
        forge_api_server,
        fmds_minimum_hbn_version,
        nvue_minimum_hbn_version,
        factory_mac_address,
        service_addrs,
        close_sender,
        network_monitor_handle,
        extension_service_manager,
        nvue_context,
        dhcp_interface_translation_mode,
        current_network_version: CurrentNetworkVersion::default(),
    };

    main_loop.run().await
}

struct MainLoop {
    forge_client_config: Arc<ForgeClientConfig>,
    machine_id: MachineId,
    factory_mac_address: MacAddress,
    build_version: String,
    periodic_config_reader: Box<periodic_config_fetcher::PeriodicConfigFetcherReader>,
    fmds_updater: FmdsUpdater,
    client_cert_renewer: ClientCertRenewer,
    hbn_device_names: HBNDeviceNames,
    is_hbn_up: bool,
    hbn_file_configs: hbn::HBNContainerFileConfigs,
    seen_blank: bool,
    has_logged_stable: bool,
    started_at: std::time::Instant,
    version_check_time: std::time::Instant,
    inventory_updater_time: std::time::Instant,
    inventory_updater_config: MachineInventoryUpdaterConfig,
    options: command_line::RunOptions,
    agent_config: AgentConfig,
    forge_api_server: String,
    fmds_minimum_hbn_version: Version<'static>,
    nvue_minimum_hbn_version: Version<'static>,
    service_addrs: ServiceAddresses,
    network_monitor_handle: Option<JoinHandle<()>>,
    close_sender: watch::Sender<bool>,
    extension_service_manager: extension_services::ExtensionServiceManager,
    nvue_context: Option<NvueClientContext>,
    dhcp_interface_translation_mode: Option<InterfaceTranslationMode>,
    current_network_version: CurrentNetworkVersion,
}

struct IterationResult {
    stop_agent: bool,
    loop_period: std::time::Duration,
}

/// `CurrentNetworkVersion` tracks the response inputs we last applied so the
/// agent can skip an HBN update only when the explicit versions match and the
/// current response still describes the same HBN behavior.
#[derive(Debug, Default)]
struct CurrentNetworkVersion {
    managed_host_config_version: Option<String>,
    instance_network_config_version: Option<String>,
    rendered_inputs_hash: Option<u64>,
}

impl CurrentNetworkVersion {
    /// Returns whether the explicit versions and HBN inputs from the response
    /// match the last successful network reconciliation.
    fn matches_versions_from(&self, conf: &ManagedHostNetworkConfigResponse) -> bool {
        let managed_host_config_version = get_non_empty_str(&conf.managed_host_config_version);
        let instance_network_config_version =
            get_non_empty_str(&conf.instance_network_config_version);

        let config_versions_identical = self.managed_host_config_version.as_deref()
            == managed_host_config_version
            && self.instance_network_config_version.as_deref() == instance_network_config_version;
        match (config_versions_identical, self.rendered_inputs_hash) {
            (true, Some(rendered_inputs_hash)) => {
                if rendered_inputs_hash == Self::hash_rendered_inputs(conf) {
                    true
                } else {
                    tracing::info!("Rendered network inputs changed without a version change");
                    false
                }
            }
            (false, _) => false,
            (_, None) => false,
        }
    }

    /// Records the versions and HBN inputs from the response after network
    /// reconciliation succeeds.
    fn update_from(&mut self, conf: &ManagedHostNetworkConfigResponse) {
        self.managed_host_config_version =
            get_non_empty_str(&conf.managed_host_config_version).map(String::from);
        self.instance_network_config_version =
            get_non_empty_str(&conf.instance_network_config_version).map(String::from);
        self.rendered_inputs_hash
            .replace(Self::hash_rendered_inputs(conf));
    }

    /// Builds the one local fingerprint used to decide whether HBN rendering
    /// can be skipped. Starting with the complete response makes new protobuf
    /// fields part of the comparison by default; we remove only fields that
    /// cannot affect HBN rendering, then normalize collections whose order does
    /// not change HBN behavior. A future protobuf map field must be normalized
    /// explicitly because its encoded iteration order is not stable.
    fn hash_rendered_inputs(conf: &ManagedHostNetworkConfigResponse) -> u64 {
        // TODO(chet): Eventually, Core should bump a version whenever any of
        // these HBN inputs change, and then we can drop this fingerprint.
        // Parent VPC policies can affect a lot of DPUs at once, though.
        // Updating every instance would turn one VPC change into a lot of
        // writes, and the version would say the instance changed when it
        // didn't. This probably needs its own version.
        let mut canonical = conf.clone();
        Self::remove_non_rendered_inputs(&mut canonical);
        Self::normalize_set_like_inputs(&mut canonical);

        let mut hasher = DefaultHasher::new();
        canonical.encode_to_vec().hash(&mut hasher);

        hasher.finish()
    }

    /// Removes response fields that do not affect HBN rendering. The
    /// fingerprint starts with the complete response, so a new protobuf field
    /// remains part of the comparison until we deliberately exclude it here.
    fn remove_non_rendered_inputs(config: &mut ManagedHostNetworkConfigResponse) {
        // These versions are compared directly before the fingerprint.
        config.managed_host_config_version.clear();
        config.instance_network_config_version.clear();

        // Instance payload fields feed status and metadata paths, not network
        // rendering.
        config.instance_id = None;
        config.instance = None;

        // The pinger type is read once when the agent starts. A runtime change
        // cannot affect HBN rendering and still requires an agent restart.
        config.dpu_network_pinger_type = None;

        // These inputs have separate owners that run independently of the HBN
        // comparison.
        config.min_dpu_functioning_links = None;
        config.dpu_extension_services.clear();
        config.astra_config = None;

        // DHCP is reconciled before this HBN skip decision on every iteration.
        config.ntp_servers.clear();

        // `host_interface_id` helps resolve the host machine ID when the agent
        // starts, but a later change does not affect HBN rendering.
        config.host_interface_id = None;

        // Nothing in the agent reads the deprecated deny-prefix list anymore.
        config.deprecated_deny_prefixes.clear();

        // DHCP is always enabled; this deprecated flag no longer controls
        // rendering.
        config.enable_dhcp = false;
    }

    /// `normalize_set_like_inputs` sorts inputs that HBN treats as sets, but
    /// preserves relative order anywhere HBN cares about it.
    fn normalize_set_like_inputs(config: &mut ManagedHostNetworkConfigResponse) {
        config.dhcp_servers.sort_unstable();
        config.route_servers.sort_unstable();
        config.deny_prefixes.sort_unstable();
        config.site_fabric_prefixes.sort_unstable();
        config.anycast_site_prefixes.sort_unstable();
        config
            .additional_route_target_imports
            .sort_by_key(|route_target| (route_target.asn, route_target.vni));

        if let Some(admin_interface) = &mut config.admin_interface {
            Self::normalize_interface(admin_interface);
        }
        for interface in &mut config.tenant_interfaces {
            Self::normalize_interface(interface);
        }

        for rule in &mut config.network_security_policy_overrides {
            Self::normalize_security_group_rule(rule);
        }
        Self::normalize_security_group_rule_order(&mut config.network_security_policy_overrides);

        if let Some(routing_profile) = &mut config.routing_profile {
            Self::normalize_routing_profile(routing_profile);
        }
    }

    /// Orders prefix-policy entries by their only rendered value.
    fn sort_prefix_entries(entries: &mut [rpc::PrefixFilterPolicyEntry]) {
        entries.sort_by(|left, right| left.prefix.cmp(&right.prefix));
    }

    /// `normalize_interface` sorts the set-like inputs inside one interface
    /// without changing the surrounding interface order.
    fn normalize_interface(interface: &mut rpc::FlatInterfaceConfig) {
        interface.vpc_prefixes.sort_unstable();
        interface.vpc_peer_prefixes.sort_unstable();
        interface.vpc_peer_vnis.sort_unstable();

        if let Some(routing_profile) = &mut interface.vpc_routing_profile {
            Self::normalize_routing_profile(routing_profile);
        }
        if let Some(routing_profile) = &mut interface.interface_routing_profile {
            Self::sort_prefix_entries(&mut routing_profile.allowed_anycast_prefixes);
        }
        if let Some(network_security_group) = &mut interface.network_security_group {
            for rule in &mut network_security_group.rules {
                Self::normalize_security_group_rule(rule);
            }
            Self::normalize_security_group_rule_order(&mut network_security_group.rules);
        }
    }

    /// `normalize_routing_profile` sorts collections whose order does not
    /// change routing behavior.
    fn normalize_routing_profile(routing_profile: &mut rpc::RoutingProfile) {
        routing_profile
            .route_target_imports
            .sort_by_key(|route_target| (route_target.asn, route_target.vni));
        routing_profile
            .route_targets_on_exports
            .sort_by_key(|route_target| (route_target.asn, route_target.vni));
        Self::sort_prefix_entries(&mut routing_profile.accepted_leaks_from_underlay);
        Self::sort_prefix_entries(&mut routing_profile.allowed_anycast_prefixes);
    }

    /// `normalize_security_group_rule` sorts the resolved source and
    /// destination prefixes for one security-group rule.
    fn normalize_security_group_rule(rule: &mut rpc::ResolvedNetworkSecurityGroupRule) {
        rule.src_prefixes.sort_unstable();
        rule.dst_prefixes.sort_unstable();
    }

    /// The renderer partitions rules by direction and address family, then
    /// stable-sorts each partition by priority. Mirror that behavior without
    /// erasing the meaningful relative order of equal-priority rules.
    fn normalize_security_group_rule_order(rules: &mut [rpc::ResolvedNetworkSecurityGroupRule]) {
        rules.sort_by_key(|rule| {
            rule.rule.as_ref().map(|attributes| {
                (
                    attributes.direction()
                        == rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress,
                    attributes.ipv6,
                    attributes.priority,
                )
            })
        });
    }
}

/// Returns the last DHCP request timestamps for all known host interfaces.
///
/// When `dhcp_grpc_server` is `Some`, fetches timestamps from the dhcp-server
/// control service via gRPC.  This is required when the DHCP server runs in a
/// separate container where the timestamps file on the DPU filesystem is not
/// accessible.
///
/// When `dhcp_grpc_server` is `None`, reads the timestamps file directly from
/// the DPU filesystem (`DhcpTimestampsFilePath::Dpu`).
///
/// Errors on either path are logged as warnings and an empty list is returned
/// so the caller can degrade gracefully.
async fn fetch_last_dhcp_requests(dhcp_grpc_server: Option<&str>) -> Vec<rpc::LastDhcpRequest> {
    if let Some(addr) = dhcp_grpc_server {
        return match crate::dhcp_server_grpc_client::get_dhcp_timestamps(addr).await {
            Ok(requests) => requests,
            Err(e) => {
                tracing::warn!("Failed to fetch DHCP timestamps via gRPC: {e:#}");
                vec![]
            }
        };
    }

    let mut dhcp_timestamps = DhcpTimestamps::new(DhcpTimestampsFilePath::Dpu);
    if let Err(e) = dhcp_timestamps.read() {
        tracing::warn!(
            "Failed to read from {}: {e}",
            DhcpTimestampsFilePath::Dpu.path_str()
        );
    }
    dhcp_timestamps
        .into_iter()
        .map(|(host_interface_id, timestamp)| rpc::LastDhcpRequest {
            host_interface_id: Some(host_interface_id),
            timestamp,
        })
        .collect()
}

impl MainLoop {
    /// Runs the MainLoop in endless mode
    async fn run(&mut self) -> Result<(), eyre::Report> {
        let mut term_signal = signal(SignalKind::terminate())?;
        let mut hup_signal = signal(SignalKind::hangup())?;

        loop {
            let result = self.run_single_iteration().await?;
            if result.stop_agent {
                return Ok(());
            }

            tokio::select! {
                biased;
                _ = term_signal.recv() => {
                    systemd::notify_stop().await?;
                    let _ = self.close_sender.send(true);
                    if let Some(handle) = self.network_monitor_handle.take() {
                        let _ = handle.await;
                    }
                    tracing::info!(version=carbide_version::v!(build_version), "TERM signal received, clean exit");
                    return Ok(());
                }
                _ = hup_signal.recv() => {
                    tracing::info!("Hangup received, timer reset");
                    let now = Instant::now();
                    self.client_cert_renewer.renew_on_next_check();
                    self.inventory_updater_time = now;
                    self.version_check_time = now;
                    // the loop_period sleep is interrupted so we will fetch new network config
                }
                _ = tokio::time::sleep(result.loop_period) => {}
            }
        }
    }

    /// Runs a single iteration of the main loop
    async fn run_single_iteration(&mut self) -> Result<IterationResult, eyre::Report> {
        let loop_start = Instant::now();

        if let Err(err) = systemd::notify_watchdog().await {
            tracing::error!(error = format!("{err:#}"), "systemd::notify_watchdog");
        }

        let mut current_health_report = None;
        let mut current_config_error = None;
        let mut is_healthy = false;
        let mut has_changed_configs = false;
        let mut current_host_network_config_version = None;
        let mut current_instance_network_config_version = None;
        let mut current_instance_config_version = None;
        let mut current_instance_id = None;
        let mut current_extension_service_version = None;

        let client_certificate_expiry_unix_epoch_secs =
            self.forge_client_config.client_cert_expiry();

        let fabric_interfaces = get_fabric_interfaces_data().await.unwrap_or_else(|err| {
            tracing::warn!("Error getting link data for fabric interfaces: {err:#}");
            vec![]
        });

        let mut status_out = rpc::DpuNetworkStatus {
            dpu_machine_id: Some(self.machine_id),
            dpu_health: None,
            dpu_agent_version: Some(self.build_version.clone()),
            observed_at: None, // None makes carbide-api set it on receipt
            network_config_version: None,
            instance_config_version: None,
            instance_network_config_version: None,
            interfaces: vec![],
            network_config_error: None,
            instance_id: None,
            client_certificate_expiry_unix_epoch_secs,
            fabric_interfaces,
            last_dhcp_requests: vec![],
            dpu_extension_service_version: None,
            dpu_extension_services: vec![],
            astra_config_status: None,
        };

        // `read` does not block
        match self.periodic_config_reader.net_conf_read() {
            Some(conf) => {
                // DHCP server only runs on the primary dpu or when using the tenant network
                if !conf.use_admin_network || conf.is_primary_dpu {
                    status_out.last_dhcp_requests =
                        fetch_last_dhcp_requests(self.options.dhcp_grpc_server.as_deref()).await;
                }

                let instance_data = self.periodic_config_reader.meta_data_conf_reader();

                let proposed_routes: Vec<_> = conf
                    .tenant_interfaces
                    .iter()
                    .filter_map(|x| IpNetwork::from_str(x.prefix.as_str()).ok())
                    .collect();

                let tenant_peers = ethernet_virtualization::tenant_peers(&conf);
                if self.is_hbn_up {
                    // First thing is to read the existing HBN version and properly set the hbn device names
                    // associated with that version.
                    let hbn_version = match self.nvue_context.as_mut() {
                        None => hbn::read_version().await?,
                        Some(nvue_context) => {
                            let nvue_system_build =
                                nvue_context.nvue_client.system_build_info().await?;
                            match nvue_system_build.strip_prefix("HBN ") {
                                Some(hbn_version) => Ok(hbn_version.into()),
                                None => Err(eyre::format_err!(
                                    "Couldn't parse HBN version from NVUE system build (\"{nvue_system_build}\")"
                                )),
                            }?
                        }
                    };

                    let hbn_version = Version::from(hbn_version.as_str())
                        .ok_or(eyre::eyre!("Unable to convert string to version"))?;
                    // HBN changed their naming scheme in HBN 2.3 from _sf to _if so we will pass that little bit around
                    // after doing an initial version check instead of assuming _sf
                    self.hbn_device_names = HBNDeviceNames::new(hbn_version.clone());

                    // NVUE is the only supported configuration path. If the DPU's
                    // HBN/DOCA is too old to support NVUE, we cannot configure it.
                    if hbn_version < self.nvue_minimum_hbn_version {
                        return Err(eyre::eyre!(
                            "HBN version {hbn_version} is older than the minimum required for NVUE ({NVUE_MINIMUM_HBN_VERSION})."
                        ));
                    }

                    // Now issue a one time per container runtime hack in the event the hack is needed for new DPU hardware
                    if self.options.hbn_config_mode.is_container_exec()
                        && let Err(err) = nvue::hack_platform_config_for_nvue().await
                    {
                        tracing::error!(
                            error = format!("{err:#}"),
                            "Hacking the container platform config."
                        );
                    };

                    if self.options.hbn_config_mode.is_container_exec()
                        && let Err(e) = self.hbn_file_configs.ensure_configs().await
                    {
                        tracing::error!(
                            "Error from HBNContainerFileConfigs::ensure_configs(): {e}"
                        );
                    }

                    tracing::trace!("Desired network config is {conf:?}");
                    // Get the actual virtualization type to use for configuring
                    // an interface, where we'll default to reading the one provided
                    // by the Carbide API, with the ability to override via RunOptions.
                    let virtualization_type = effective_virtualization_type(&conf, &self.options)?;

                    let dhcp_result = ethernet_virtualization::update_dhcp(
                        &self.agent_config.hbn.root_dir,
                        &conf,
                        self.agent_config.hbn.skip_reload,
                        &self.service_addrs,
                        self.hbn_device_names.clone(),
                        self.options.dhcp_grpc_server.clone(),
                        self.dhcp_interface_translation_mode.as_ref(),
                    )
                    .await;

                    let update_result = if self.current_network_version.matches_versions_from(&conf)
                    {
                        tracing::debug!(
                            "No configuration change, skipping HBN updates: {:?}",
                            &self.current_network_version
                        );
                        Ok(false)
                    } else {
                        if self.options.agent_platform_type.is_dpu_os()
                            && hbn_version >= self.fmds_minimum_hbn_version
                        {
                            // Generate the fmds interface plan from the config. This does not apply the plan.
                            // The plan is applied when the NVUE template is written
                            let fmds_proposed_interfaces = &self.agent_config.fmds_armos_networking;
                            let network_plan = DpuNetworkInterfaces::new(fmds_proposed_interfaces);

                            let fmds_interface_plan =
                                Interface::plan(self.hbn_device_names.sfs[0], network_plan).await?;
                            tracing::trace!("Interface plan: {:?}", fmds_interface_plan);

                            // Generate the fmds route plan from conf.tenant_interfaces[n].address
                            // the plan is applied when the nvue template is written
                            let route_plan = plan_fmds_armos_routing(
                                self.hbn_device_names.sfs[0],
                                &proposed_routes,
                            )
                            .await?;
                            tracing::trace!("Route plan: {:?}", route_plan);

                            // Apply the interface plan. This is where we actually configure
                            // the FMDS phone home interface on the DPU.
                            Interface::apply(fmds_interface_plan).await?;

                            // If there are routes, apply the route plan. This is where we
                            // actually add and remove FMDS phone home routes.
                            //
                            // When a DPU has recently booted, there may not be a pf0dpu1
                            // interface configured yet, so routes may not be applied on the
                            // first tick of the loop. Once the interface is configured, routes
                            // can be added and removed.

                            // This means that routes will be added last and might take a few seconds
                            // to appear
                            if let Some(route_plan) = route_plan {
                                Route::apply(route_plan).await?;
                            }
                        }

                        // We'll update some internal bridging config if bridging config
                        // for traffic_intercept was sent in.
                        let bridging_result = if self.options.agent_platform_type.is_dpu_os()
                            && conf
                                .traffic_intercept_config
                                .as_ref()
                                .map(|vc| vc.bridging.is_some())
                                .unwrap_or_default()
                        {
                            ethernet_virtualization::update_traffic_intercept_bridging(
                                &conf,
                                self.hbn_device_names.clone(),
                                self.agent_config.hbn.skip_reload,
                            )
                            .await
                        } else {
                            Ok(false) // No errors and no change.
                        };

                        if bridging_result.is_ok() {
                            let update_flavor = match self.nvue_context.as_mut() {
                                Some(nvue_context) => NvueUpdateFlavor::RestApi { nvue_context },
                                None => NvueUpdateFlavor::StartupFile {
                                    hbn_root: &self.agent_config.hbn.root_dir,
                                    skip_post: self.agent_config.hbn.skip_reload,
                                },
                            };
                            ethernet_virtualization::update_nvue(
                                virtualization_type,
                                update_flavor,
                                &conf,
                                self.hbn_device_names.clone(),
                            )
                            .await
                        } else {
                            bridging_result
                        }
                    };

                    let joined_result = match (update_result, dhcp_result) {
                        (Ok(a), Ok(b)) => Ok(a | b),
                        (Err(e1), Err(e2)) => Err(eyre::eyre!(
                            "network update failed: update={e1:#}, dhcp={e2:#}"
                        )),
                        (Err(err), Ok(_)) => Err(err.wrap_err("network update failed (update)")),
                        (Ok(_), Err(err)) => Err(err.wrap_err("network update failed (dhcp)")),
                    };
                    match joined_result {
                        Ok(has_changed) => {
                            self.current_network_version.update_from(&conf);
                            has_changed_configs = has_changed;
                            if self.options.agent_platform_type.is_dpu_os()
                                && let Err(err) = mtu::ensure().await
                            {
                                tracing::error!(error = %err, "Error reading/setting MTU for p0 or p1");
                            }

                            // Updating network config succeeded.
                            // Tell the server about the applied version.
                            status_out.network_config_version =
                                Some(conf.managed_host_config_version.clone());
                            status_out.instance_id = conf.instance_id;
                            // On the admin network we don't have to report the instance network config version
                            if !conf.instance_network_config_version.is_empty() {
                                status_out.instance_network_config_version = Some(
                                    match conf
                                        .instance_network_config_version
                                        .parse::<config_version::ConfigVersion>()
                                    {
                                        Ok(managed_host_instance_network_config_version) => {
                                            match instance_data
                                                .as_ref()
                                                .map(|instance| instance.network_config_version)
                                            {
                                                Some(instance_metadata_network_config_version) => {
                                                    // Report the older version of the versions received via 2 path
                                                    // That makes sure we don't report progress if we haven't received the newest version
                                                    // via both path.
                                                    let reported_instance_network_config_version =
                                                    managed_host_instance_network_config_version
                                                        .min_by_timestamp(
                                                        &instance_metadata_network_config_version,
                                                    );
                                                    if instance_metadata_network_config_version
                                                    != managed_host_instance_network_config_version
                                                {
                                                    tracing::warn!("Different instance network config version received. GetManagedHostNetworkConfig: {}, FindInstanceByMachineId: {}, Reporting: {}",
                                                        managed_host_instance_network_config_version,
                                                    instance_metadata_network_config_version,
                                                    reported_instance_network_config_version,
                                                );
                                                }
                                                    reported_instance_network_config_version
                                                        .version_string()
                                                }
                                                None => {
                                                    // TODO: Maybe we want to wait until both receive path provide the same data?
                                                    tracing::warn!(
                                                        "Received instance_network_config_version via GetManagedHostNetworkConfig, but not via FindInstanceByMachineId. Acknowledging received version"
                                                    );
                                                    conf.instance_network_config_version.clone()
                                                }
                                            }
                                        }
                                        Err(err) => {
                                            // We can't compare the 2 received versions since the first is not parseable
                                            // This isn't really supposed to happen.
                                            // However to avoid breaking the system in that case,
                                            // we still report the version received via GetManagedHostNetworkConfig,
                                            // because that is also what we did in the past.
                                            tracing::error!(error = %err, "Failed to parse instance_network_config_version received via GetManagedHostNetworkConfig");
                                            conf.instance_network_config_version.clone()
                                        }
                                    },
                                );
                            }
                            current_host_network_config_version =
                                status_out.network_config_version.clone();
                            current_instance_network_config_version =
                                status_out.instance_network_config_version.clone();

                            match ethernet_virtualization::interfaces(
                                &conf,
                                self.factory_mac_address,
                                self.nvue_context.as_ref().map(|c| &c.nvue_client),
                            )
                            .await
                            {
                                Ok(interfaces) => status_out.interfaces = interfaces,
                                Err(err) => status_out.network_config_error = Some(err.to_string()),
                            }
                        }
                        Err(err) => {
                            tracing::error!(
                                error = format!("{err:#}"),
                                "Writing network configuration"
                            );
                            status_out.network_config_error = Some(err.to_string());
                        }
                    }

                    // In case of secondary DPU, the interface must be disabled if on admin network, else enabled.
                    // Note that the nvue config handles the blocking of traffic on the interface.  This is only so that the host link reflects the correct state.
                    if self.options.agent_platform_type.is_dpu_os()
                        && let Err(err) =
                            ethernet_virtualization::update_interface_state(&conf).await
                    {
                        tracing::error!(error = format!("{err:#}"), "Updating interface state.");
                    }
                }

                // Feed the latest instance metadata to FMDS and acknowledge it
                // Note: Performing the update here instead of the FMDS mechanism directly
                // pulling the Metadata is preferred - since the mainloop will make sure
                // all the information (Instance Metadata and Network Config) is in sync.
                // It will guarantee that the Instance Config that is acknowledged to
                // carbide via the status message is actually visible to the tenant via
                // FMDS
                self.fmds_updater
                    .update(instance_data.clone(), Some(conf.clone()))
                    .await;
                status_out.instance_config_version = instance_data
                    .as_ref()
                    .map(|instance| instance.config_version.version_string());
                current_instance_config_version = status_out.instance_config_version.clone();
                current_instance_id = status_out.instance_id.as_ref().map(|id| id.to_string());

                let health_report = match self.nvue_context.as_ref() {
                    None => {
                        health::health_check(HealthCheckParams {
                            hbn_root: &self.agent_config.hbn.root_dir,
                            host_routes: &tenant_peers,
                            has_changed_configs,
                            min_healthy_links: conf.min_dpu_functioning_links.unwrap_or(2),
                            route_servers: &conf.route_servers,
                            hbn_device_names: self.hbn_device_names.clone(),
                            include_dhcp_server: !conf.use_admin_network || conf.is_primary_dpu,
                            run_restricted_mode_check: false,
                        })
                        .await
                    }
                    Some(nvue_context) => health::nvue_api_health(&nvue_context.nvue_client).await,
                };
                is_healthy = !health_report.successes.is_empty() && health_report.alerts.is_empty();
                self.is_hbn_up = health::is_up(&health_report);
                // subset of is_healthy
                tracing::trace!(%self.machine_id, ?health_report, "HBN health");
                status_out.dpu_health = Some(health_report.clone().into());
                current_health_report = Some(health_report);
                current_config_error = status_out.network_config_error.clone();

                // Process extension services if configuration is available
                // If use_admin_network is true, it means the instance is being deleted,
                // so we mark all extension services as removed for termination
                let expected_extension_services = if conf.use_admin_network {
                    conf.dpu_extension_services
                        .clone()
                        .into_iter()
                        .map(|mut service| {
                            if service.removed.is_none() {
                                service.removed = Some(chrono::Utc::now().to_rfc3339());
                            }
                            service
                        })
                        .collect()
                } else {
                    conf.dpu_extension_services.clone()
                };

                if let Err(err) = self
                    .extension_service_manager
                    .update_desired_services(expected_extension_services.clone())
                    .await
                {
                    tracing::error!(
                        error = format!("{err:#}"),
                        "Failed to update extension services"
                    );
                }
                status_out.dpu_extension_service_version = instance_data
                    .as_ref()
                    .map(|instance| instance.extension_service_version.version_string());
                status_out.dpu_extension_services = self
                    .extension_service_manager
                    .get_service_statuses(expected_extension_services.clone())
                    .await
                    .unwrap_or_else(|err| {
                        tracing::error!(error = %err, "Error getting extension service statuses");
                        vec![]
                    });
                current_extension_service_version =
                    status_out.dpu_extension_service_version.clone();

                record_network_status(
                    status_out,
                    &self.forge_api_server,
                    &self.forge_client_config,
                )
                .await;
                self.seen_blank = false;
            }
            None => {
                // No network config means server can't find the DPU, usually because it was
                // force-deleted. Only reset network config the _second_ time we can't find the
                // DPU. Safety first.
                if self.seen_blank {
                    ethernet_virtualization::reset(
                        &self.agent_config.hbn.root_dir,
                        self.agent_config.hbn.skip_reload,
                    )
                    .await;
                }
                self.seen_blank = true;
                // we don't record_network_status because the server doesn't know about this DPU
            }
        };

        let now = Instant::now();
        self.client_cert_renewer
            .renew_certificates_if_necessary(None)
            .await;

        if now > self.inventory_updater_time {
            self.inventory_updater_time =
                now.add(self.inventory_updater_config.update_inventory_interval);
            if let Err(err) =
                machine_inventory_updater::single_run(&self.inventory_updater_config).await
            {
                tracing::error!(%err, "machine_inventory_updater error");
            }
        }

        if self.options.agent_platform_type.is_dpu_os()
            && let result @ IterationResult {
                stop_agent: true, ..
            } = self.perform_upgrade_check(now).await
        {
            return Ok(result);
        }

        let loop_period = if self.seen_blank || !is_healthy || has_changed_configs {
            std::time::Duration::from_secs(self.agent_config.period.main_loop_active_secs)
        } else {
            if !self.has_logged_stable {
                tracing::info!("HBN is healthy and network configuration is stable");
                self.has_logged_stable = true;
            }
            std::time::Duration::from_secs(self.agent_config.period.main_loop_idle_secs)
        };

        let health_alerts = current_health_report
            .as_ref()
            .map(|report| report.alerts.as_slice())
            .unwrap_or_default();
        tracing::info!(
            is_healthy,
            has_changed_configs,
            self.seen_blank,
            num_health_probe_alerts = health_alerts.len(),
            health_probe_alerts = {
                let mut result = String::new();
                for alert in health_alerts.iter() {
                    if !result.is_empty() {
                        result.push(',');
                    }
                    if let Some(target) = &alert.target {
                        result += &format!("{} [Target: {}]: {}", alert.id, target, alert.message);
                    } else {
                        result += &format!("{}: {}", alert.id, alert.message);
                    }
                }
                result
            },
            write_config_error = current_config_error.unwrap_or_default(),
            managed_host_network_config_version = current_host_network_config_version.unwrap_or_default(),
            instance_id = current_instance_id.unwrap_or_default(),
            instance_network_config_version = current_instance_network_config_version.unwrap_or_default(),
            instance_config_version = current_instance_config_version.unwrap_or_default(),
            extension_service_version = current_extension_service_version.unwrap_or_default(),
            loop_duration = %dt(loop_start.elapsed()),
            version_check_in = %dt(self.version_check_time - Instant::now()),
            uptime = %dt(self.started_at.elapsed()),
            "loop metrics",
        );

        Ok(IterationResult {
            stop_agent: false,
            loop_period,
        })
    }

    async fn perform_upgrade_check(&mut self, now: std::time::Instant) -> IterationResult {
        if self.options.skip_upgrade_check || !self.options.agent_platform_type.is_dpu_os() {
            return IterationResult {
                stop_agent: false,
                loop_period: Default::default(),
            };
        }

        // We potentially restart at this point, so make it last in the loop
        if now > self.version_check_time {
            self.version_check_time = now.add(std::time::Duration::from_secs(
                self.agent_config.period.version_check_secs,
            ));
            let upgrade_result = upgrade::upgrade(
                &self.forge_api_server,
                &self.forge_client_config,
                &self.machine_id,
                self.agent_config.updates.override_upgrade_cmd.as_deref(),
            )
            .await;
            match upgrade_result {
                Ok(false) => {
                    // did not upgrade, normal case, continue
                }
                Ok(true) => {
                    // upgraded, need to exit and restart
                    if let Err(err) = systemd::notify_stop().await {
                        tracing::error!(error = format!("{err:#}"), "systemd::notify_stop");
                    }
                    return IterationResult {
                        stop_agent: true,
                        loop_period: Duration::from_secs(0),
                    };
                }
                Err(e) => {
                    tracing::error!(
                        self.forge_api_server,
                        error = format!("{e:#}"), // we need alt display for wrap_err_with to work well
                        "upgrade_check failed"
                    );
                }
            }
        }

        IterationResult {
            stop_agent: false,
            loop_period: Default::default(),
        }
    }
}

/// effective_virtualization_type returns the virtualization type
/// to use for generating configuration. This defaults to whatever
/// comes from Carbide API, with the ability to override with runtime
/// options.
fn effective_virtualization_type(
    conf: &ManagedHostNetworkConfigResponse,
    options: &RunOptions,
) -> eyre::Result<VpcVirtualizationType> {
    // First, grab the VpcVirtualizationType returned to us
    // from the Carbide API (which *should* be what comes from
    // the `network_virtualization_type` column from the `vpcs`
    // table for the VPC this DPU is in).
    //
    // This may be unset, which means to just use
    // EthernetVirtualizer.
    let virtualization_type_from_remote = conf
        .network_virtualization_type
        .map(rpc::VpcVirtualizationType::try_from)
        .transpose()?
        .map(|v| v.into());

    // And now see if the remote virtualization type should be overwritten
    // by runtime options. If it's not, and the remote value was also unset,
    // then just use EthernetVirtualizer.
    let virtualization_type = options
        .override_network_virtualization_type // dev
        .or(virtualization_type_from_remote)
        .unwrap_or_else(|| {
            tracing::warn!(
                "Missing network_virtualization_type, defaulting to {}",
                VpcVirtualizationType::EthernetVirtualizer
            );
            VpcVirtualizationType::EthernetVirtualizer
        });

    Ok(virtualization_type)
}

// TODO(chet): We'll eventually want a documented IPv6 address we can
// configure here for tenants to hit FMDS over IPv6.
async fn plan_fmds_armos_routing(
    interface: &str,
    proposed_routes: &Vec<IpNetwork>,
) -> eyre::Result<Option<DpuRoutePlan>> {
    let mut proposed_plan = Vec::new();

    let interfaces = Interface::current_addresses(interface).await?;

    // find all ipv4 addresses on interface
    let fmds_interface = interfaces
        .iter()
        .find_map(|e| e.addr_info.iter().find(|i| i.family == "inet"));

    tracing::trace!("fmds_interface: {:?}", fmds_interface);

    if let Some(ipinterface) = fmds_interface {
        for route in proposed_routes {
            let new_route = IpRoute {
                dst: *route,
                dev: None,
                protocol: None,
                scope: None,
                gateway: Some(IpAddr::from([169, 254, 169, 253])), // use gateway IP from inside HBN container
                prefsrc: Some(ipinterface.local),
                flags: vec![],
            };
            proposed_plan.push(new_route);
        }
        let plan = Route::plan(interface, proposed_plan).await?;
        Ok(Some(plan))
    } else {
        Ok(None)
    }
}
pub async fn record_network_status(
    status: rpc::DpuNetworkStatus,
    forge_api: &str,
    forge_client_config: &forge_tls_client::ForgeClientConfig,
) {
    let mut client = match forge_tls_client::ForgeTlsClient::new(forge_client_config)
        .build(forge_api)
        .await
    {
        Ok(client) => client,
        Err(err) => {
            tracing::error!(
                forge_api,
                error = format!("{err:#}"),
                "record_network_status: Could not connect to Forge API server. Will retry."
            );
            return;
        }
    };
    let request = tonic::Request::new(status);
    if let Err(err) = client.record_dpu_network_status(request).await {
        tracing::error!(
            error = format!("{err:#}"),
            "Error while executing the record_network_status gRPC call"
        );
    }
}

// Get the link type, carrier status, MTU, and whatever else for our uplinks
// into the network fabric.
//
// For a link to be considered an uplink, it must satisfy all of the following:
// 1. The network device is on a PCI bus.
// 2. The network device type is Ethernet.
// 3. The Ethernet MAC address is in the unicast+universal range (last two bits
//    of the first OUI byte are both set to 0).
async fn get_fabric_interfaces_data()
-> Result<Vec<rpc::FabricInterfaceData>, Box<dyn std::error::Error>> {
    let pci_network_devices: HashSet<_> = {
        let net_devices = sysfs::get_net_devices()?;
        // let net_devices = net_devices.into_iter();
        net_devices
            .into_iter()
            .filter_map(|net_device| {
                net_device
                    .is_pci_device()
                    .map(|is_pci| is_pci.then(|| net_device.entry_name()))
                    .transpose()
            })
            .collect::<Result<_, _>>()?
    };

    let rtnetlink_link_data = netlink::get_all_interface_links().await?;

    fn is_universal_unicast(oui_first_byte: u8) -> bool {
        (oui_first_byte & 0x03) == 0
    }

    let fabric_interface_data = rtnetlink_link_data
        .into_iter()
        .filter_map(|(interface_name, interface_data)| {
            let is_ethernet = interface_data.is_ethernet();
            let is_pci = {
                let iface_name: &OsStr = interface_name.as_ref();
                pci_network_devices.contains(iface_name)
            };
            let is_universal_unicast = interface_data
                .address
                .as_ref()
                .and_then(|address| address.first())
                .map(|first_byte| is_universal_unicast(*first_byte))
                .unwrap_or_else(|| {
                    tracing::warn!(
                        "The MAC address for interface {interface_name} was missing or empty"
                    );
                    false
                });

            (is_ethernet && is_pci && is_universal_unicast).then(|| {
                let link_data: rpc::LinkData = (&interface_data).into();

                let link_data = Some(link_data);
                rpc::FabricInterfaceData {
                    interface_name,
                    link_data,
                }
            })
        })
        .collect();
    Ok(fabric_interface_data)
}

const ONE_SECOND: Duration = Duration::from_secs(1);

// Format a Duration for display
fn dt(d: Duration) -> humantime::FormattedDuration {
    humantime::format_duration(if d > ONE_SECOND {
        Duration::from_secs(d.as_secs())
    } else {
        Duration::from_millis(d.as_millis() as u64)
    })
}

// Do horrible things to the DPU including an out of band, unknown to forge, reboot.
//
// If a DPU cannot load its 2.9 ATF/UEFI, issue hacky commands to force load it and reboot
// which actually does the loading of the ATU/UEFI. Upon reboot, the bfvcheck will be valid or
// the dpu will be left in a broken state because it was unable to load its ATF/UEFI and needs to
// be investigated.
async fn hack_dpu_os_to_load_atf_uefi_with_specific_versions() -> eyre::Result<()> {
    tracing::info!("attempting to verify the hack for specific versions");
    let bfvcheck_output: String = if cfg!(test) {
        let test_data_dir = PathBuf::from(crate::dpu::ARMOS_TEST_DATA_DIR);

        std::fs::read_to_string(test_data_dir.join("bfvcheck.out")).map_err(|e| {
            error!("Could not read bfvcheck.out: {e}");
            eyre::eyre!("Could not read bfvcheck.out: {}", e)
        })?
    } else {
        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", "bfvcheck"]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());

        // bfvcheck is taking around 11 seconds, and worse case is 30s
        // bump it to a minute just in case
        let output = tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT * 6, cmd.output())
            .await
            .wrap_err_with(|| format!("Timeout while running command: {cmd_str:?}"))??;

        String::from_utf8_lossy(&output.stdout).to_string()
    };

    // The specific check is to see if we have not loaded but have a recommended ATF/UEFI is 2.9.
    // If this is the case, we have hit the "unable to load the ATF/UEFI because of the ugly bug"
    // condition, and it is the only time these commands are issued. The ATF/UEFI is immediately updated on
    // newer cards and after reboot on the older cards. Both need the reboot to properly populate the
    // redfish db as well.

    // these are colorized on bfvcheck output so instead of checking for ugly bash color codes, just
    // validate the actual strings
    if bfvcheck_output.clone().contains("WARNING: ATF VERSION DOES NOT MATCH RECOMMENDED!") &&
        bfvcheck_output.clone().contains("WARNING: UEFI VERSION DOES NOT MATCH RECOMMENDED!") &&
        // This is to ensure that the recommended is 4.9.3, meaning its only going to fix this if
        // moving to 4.9.3. This is done so that no older DPUs which may be operating under a customer
        // that have not properly updated but yet somehow became assigned. These DPUS are only in
        // this state because they are stuck in reprovisioning as part of the same release with
        // 4.9.3 included. If this check was deployed to production weeks after the update to 4.9.3,
        // there could be a problem with  assigned machines having 2.9.2 recommended but not loaded,
        // but if both releases drop at the same time, this state will only be seen during
        // reprovisioning / initial discovery.
        bfvcheck_output.clone().contains(
        "-RECOMMENDED VERSIONS-
ATF: v2.2(release):4.9.3-")
    && !cfg!(test)
    {
        tracing::info!(
            "bfvcheck output has identifed a situation in which it needs to load and \
                restart the DPU during install. This message should not print in logs multiple reboots. \
                If it does, the ATF/UEFI cannot be loaded and the card should be investigated."
        );
        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec![
            "-c",
            "mlxbf-bootctl -s -b /lib/firmware/mellanox/boot/default.bfb -d /dev/mmcblk0",
        ]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());

        // This is not a typo, we have to run it twice as per NBU
        tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("Timeout while running command: {cmd_str:?}"))??;
        // This is not a typo, we have to run it twice as per NBU
        tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("Timeout while running command: {cmd_str:?}"))??;

        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", "sync"]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());
        tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("Timeout while running command: {cmd_str:?}"))??;

        // And now for the pièce de résistance, a reboot inline on the dpu OS, and this command
        // takes a LONG time so we will put an egregiously large reboot time
        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", "reboot"]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());
        tokio::time::timeout(Duration::from_secs(60 * 10), cmd.output())
            .await
            .wrap_err_with(|| format!("Timeout while running command: {cmd_str:?}"))??;
    }

    // This method will either reboot a card or just return ok.
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;

    /// Builds a routing profile with multiple entries in each set-like
    /// collection used by the fingerprint tests.
    fn comparison_routing_profile() -> rpc::RoutingProfile {
        rpc::RoutingProfile {
            route_target_imports: vec![
                ::rpc::common::RouteTarget {
                    asn: 65_001,
                    vni: 101,
                },
                ::rpc::common::RouteTarget {
                    asn: 65_002,
                    vni: 102,
                },
            ],
            route_targets_on_exports: vec![
                ::rpc::common::RouteTarget {
                    asn: 65_003,
                    vni: 103,
                },
                ::rpc::common::RouteTarget {
                    asn: 65_004,
                    vni: 104,
                },
            ],
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,
            tenant_leak_communities_accepted: false,
            accepted_leaks_from_underlay: vec![
                rpc::PrefixFilterPolicyEntry {
                    prefix: "10.0.0.0/8".to_string(),
                },
                rpc::PrefixFilterPolicyEntry {
                    prefix: "172.16.0.0/12".to_string(),
                },
            ],
            allowed_anycast_prefixes: vec![
                rpc::PrefixFilterPolicyEntry {
                    prefix: "192.0.2.0/24".to_string(),
                },
                rpc::PrefixFilterPolicyEntry {
                    prefix: "198.51.100.0/24".to_string(),
                },
            ],
        }
    }

    /// Builds a resolved rule with nested prefix sets and caller-selected ID,
    /// priority, and action.
    fn comparison_security_group_rule(
        id: &str,
        priority: u32,
        action: rpc::NetworkSecurityGroupRuleAction,
    ) -> rpc::ResolvedNetworkSecurityGroupRule {
        rpc::ResolvedNetworkSecurityGroupRule {
            rule: Some(rpc::NetworkSecurityGroupRuleAttributes {
                id: Some(id.to_string()),
                direction: rpc::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress.into(),
                protocol: rpc::NetworkSecurityGroupRuleProtocol::NsgRuleProtoAny.into(),
                action: action.into(),
                priority,
                source_net: Some(
                    rpc::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                ..Default::default()
            }),
            src_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
            dst_prefixes: vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()],
        }
    }

    /// Builds an interface that exercises every nested collection normalized
    /// by the fingerprint.
    fn comparison_interface(id: &str, vlan_id: u32, vni: u32) -> rpc::FlatInterfaceConfig {
        rpc::FlatInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical.into(),
            vlan_id,
            vni,
            gateway: "10.0.0.1/24".to_string(),
            ip: "10.0.0.2".to_string(),
            interface_prefix: "10.0.0.0/31".to_string(),
            virtual_function_id: None,
            vpc_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
            prefix: "10.0.0.0/24".to_string(),
            fqdn: format!("{id}.example.test"),
            booturl: Some("http://boot.example.test/ipxe".to_string()),
            vpc_vni: vni + 1_000,
            svi_ip: Some("10.0.0.1".to_string()),
            tenant_vrf_loopback_ip: Some("10.0.0.3".to_string()),
            is_l2_segment: false,
            vpc_peer_prefixes: vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()],
            vpc_peer_vnis: vec![2_001, 2_002],
            mtu: Some(9_000),
            ipv6_interface_config: Some(rpc::FlatInterfaceIpv6Config {
                ip: "2001:db8::1".to_string(),
                interface_prefix: "2001:db8::/127".to_string(),
                svi_ip: Some("2001:db8::".to_string()),
            }),
            vpc_routing_profile: Some(comparison_routing_profile()),
            interface_routing_profile: Some(rpc::FlatInterfaceRoutingProfile {
                allowed_anycast_prefixes: vec![
                    rpc::PrefixFilterPolicyEntry {
                        prefix: "203.0.113.0/25".to_string(),
                    },
                    rpc::PrefixFilterPolicyEntry {
                        prefix: "203.0.113.128/25".to_string(),
                    },
                ],
            }),
            network_security_group: Some(rpc::FlatInterfaceNetworkSecurityGroupConfig {
                id: format!("nsg-{id}"),
                version: "nsg-v1".to_string(),
                source: rpc::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                rules: vec![
                    comparison_security_group_rule(
                        "first-rule",
                        100,
                        rpc::NetworkSecurityGroupRuleAction::NsgRuleActionPermit,
                    ),
                    comparison_security_group_rule(
                        "second-rule",
                        200,
                        rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny,
                    ),
                ],
                stateful_egress: false,
            }),
            internal_uuid: Some(::rpc::common::Uuid {
                value: id.to_string(),
            }),
        }
    }

    /// Builds one populated response shared by the rendered, non-HBN, and
    /// ordering comparison cases.
    fn comparison_network_config() -> ManagedHostNetworkConfigResponse {
        ManagedHostNetworkConfigResponse {
            asn: 65_000,
            dhcp_servers: vec!["10.10.0.1".to_string(), "10.10.0.2".to_string()],
            vni_device: "vxlan48".to_string(),
            managed_host_config: Some(rpc::ManagedHostNetworkConfig {
                loopback_ip: "10.20.0.1".to_string(),
                quarantine_state: None,
            }),
            managed_host_config_version: "managed-v1".to_string(),
            use_admin_network: false,
            admin_interface: Some(comparison_interface("admin", 10, 100)),
            tenant_interfaces: vec![
                comparison_interface("first-interface", 20, 200),
                comparison_interface("second-interface", 30, 300),
            ],
            instance_network_config_version: "instance-v1".to_string(),
            network_virtualization_type: Some(rpc::VpcVirtualizationType::Fnn.into()),
            vpc_vni: Some(1_000),
            route_servers: vec!["10.30.0.1".to_string(), "10.30.0.2".to_string()],
            remote_id: "dpu-remote-id".to_string(),
            deprecated_deny_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
            deny_prefixes: vec!["100.64.0.0/10".to_string(), "169.254.0.0/16".to_string()],
            site_fabric_prefixes: vec!["10.0.0.0/8".to_string(), "172.16.0.0/12".to_string()],
            vpc_isolation_behavior: rpc::VpcIsolationBehaviorType::VpcIsolationMutual.into(),
            stateful_acls_enabled: true,
            ntp_servers: vec!["10.40.0.1".to_string(), "10.40.0.2".to_string()],
            enable_dhcp: true,
            host_interface_id: Some("60cef902-9779-4666-8362-c9bb4b37185f".to_string()),
            is_primary_dpu: true,
            internet_l3_vni: Some(4_000),
            datacenter_asn: 65_100,
            common_internal_route_target: Some(::rpc::common::RouteTarget {
                asn: 65_200,
                vni: 5_000,
            }),
            additional_route_target_imports: vec![
                ::rpc::common::RouteTarget {
                    asn: 65_201,
                    vni: 5_001,
                },
                ::rpc::common::RouteTarget {
                    asn: 65_202,
                    vni: 5_002,
                },
            ],
            network_security_policy_overrides: vec![
                comparison_security_group_rule(
                    "first-override",
                    10,
                    rpc::NetworkSecurityGroupRuleAction::NsgRuleActionPermit,
                ),
                comparison_security_group_rule(
                    "second-override",
                    20,
                    rpc::NetworkSecurityGroupRuleAction::NsgRuleActionDeny,
                ),
            ],
            routing_profile: Some(comparison_routing_profile()),
            anycast_site_prefixes: vec!["192.0.2.0/24".to_string(), "198.51.100.0/24".to_string()],
            tenant_host_asn: Some(65_300),
            site_global_vpc_vni: Some(6_000),
            bgp_leaf_session_password: Some("leaf-password".to_string()),
            ..Default::default()
        }
    }

    /// Selects the unchanged baseline or one response mutation that should
    /// invalidate the HBN skip decision.
    #[derive(Clone, Copy, Debug)]
    enum RenderedInputChange {
        Unchanged,
        ManagedVersion,
        InstanceVersion,
        ManagedHostLoopback,
        DenyPrefix,
        SiteFabricPrefix,
        AdminInterfaceVni,
        TenantInterfaceVni,
        TenantVpcPrefix,
        TenantPeerPrefix,
        TenantPeerVni,
        VpcRoutingProfile,
        InterfaceRoutingProfile,
        NetworkSecurityGroup,
        NetworkSecurityPolicyOverride,
        NetworkVirtualizationType,
        PrimaryDpu,
    }

    impl RenderedInputChange {
        /// Applies the selected case to the shared response.
        fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
            match self {
                Self::Unchanged => {}
                Self::ManagedVersion => config.managed_host_config_version.push_str("-changed"),
                Self::InstanceVersion => {
                    config.instance_network_config_version.push_str("-changed")
                }
                Self::ManagedHostLoopback => {
                    config
                        .managed_host_config
                        .as_mut()
                        .expect("comparison fixture has managed-host config")
                        .loopback_ip = "10.20.0.2".to_string();
                }
                Self::DenyPrefix => config.deny_prefixes.push("192.0.2.0/24".to_string()),
                Self::SiteFabricPrefix => {
                    config
                        .site_fabric_prefixes
                        .push("192.168.0.0/16".to_string());
                }
                Self::AdminInterfaceVni => {
                    config
                        .admin_interface
                        .as_mut()
                        .expect("comparison fixture has an admin interface")
                        .vni += 1;
                }
                Self::TenantInterfaceVni => config.tenant_interfaces[1].vni += 1,
                Self::TenantVpcPrefix => {
                    config.tenant_interfaces[1]
                        .vpc_prefixes
                        .push("192.168.0.0/16".to_string());
                }
                Self::TenantPeerPrefix => {
                    config.tenant_interfaces[1]
                        .vpc_peer_prefixes
                        .push("203.0.113.0/24".to_string());
                }
                Self::TenantPeerVni => config.tenant_interfaces[1].vpc_peer_vnis.push(2_003),
                Self::VpcRoutingProfile => {
                    config.tenant_interfaces[1]
                        .vpc_routing_profile
                        .as_mut()
                        .expect("comparison fixture has a VPC routing profile")
                        .leak_default_route_from_underlay = true;
                }
                Self::InterfaceRoutingProfile => {
                    config.tenant_interfaces[1]
                        .interface_routing_profile
                        .as_mut()
                        .expect("comparison fixture has an interface routing profile")
                        .allowed_anycast_prefixes
                        .push(rpc::PrefixFilterPolicyEntry {
                            prefix: "100.64.0.0/10".to_string(),
                        });
                }
                Self::NetworkSecurityGroup => {
                    config.tenant_interfaces[1]
                        .network_security_group
                        .as_mut()
                        .expect("comparison fixture has a network security group")
                        .stateful_egress = true;
                }
                Self::NetworkSecurityPolicyOverride => {
                    config.network_security_policy_overrides[1]
                        .src_prefixes
                        .push("100.64.0.0/10".to_string());
                }
                Self::NetworkVirtualizationType => {
                    config.network_virtualization_type =
                        Some(rpc::VpcVirtualizationType::EthernetVirtualizer.into());
                }
                Self::PrimaryDpu => config.is_primary_dpu = false,
            }
        }
    }

    /// A new agent process must render once before it can use the HBN skip
    /// cache; no response may match an uninitialized fingerprint.
    #[test]
    fn current_network_version_never_matches_before_first_update() {
        let config = comparison_network_config();

        assert!(!CurrentNetworkVersion::default().matches_versions_from(&config));
    }

    /// Every input consumed by HBN must invalidate the cache, and either
    /// explicit version must do the same.
    #[test]
    fn current_network_version_detects_rendered_input_changes() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(run = |change| {
            let mut config = comparison_network_config();
            let mut current = CurrentNetworkVersion::default();
            current.update_from(&config);
            change.apply(&mut config);
            current.matches_versions_from(&config)
        };
            "unchanged configuration" {
                RenderedInputChange::Unchanged => true,
            }
            "version changes" {
                RenderedInputChange::ManagedVersion => false,
                RenderedInputChange::InstanceVersion => false,
            }
            "top-level rendering inputs" {
                RenderedInputChange::ManagedHostLoopback => false,
                RenderedInputChange::DenyPrefix => false,
                RenderedInputChange::SiteFabricPrefix => false,
                RenderedInputChange::NetworkVirtualizationType => false,
                RenderedInputChange::PrimaryDpu => false,
            }
            "interface rendering inputs" {
                RenderedInputChange::AdminInterfaceVni => false,
                RenderedInputChange::TenantInterfaceVni => false,
                RenderedInputChange::TenantVpcPrefix => false,
                RenderedInputChange::TenantPeerPrefix => false,
                RenderedInputChange::TenantPeerVni => false,
                RenderedInputChange::VpcRoutingProfile => false,
                RenderedInputChange::InterfaceRoutingProfile => false,
                RenderedInputChange::NetworkSecurityGroup => false,
                RenderedInputChange::NetworkSecurityPolicyOverride => false,
            }
        );
    }

    /// `SetLikeInputReordering` lists the response collections this test
    /// reverses to prove member order does not trigger another HBN apply.
    #[derive(Clone, Copy, Debug)]
    enum SetLikeInputReordering {
        DhcpServers,
        RouteServers,
        SiteFabricPrefixes,
        AdminInterfaceVpcPrefixes,
        TenantVpcPrefixes,
        TenantPeerPrefixes,
        TenantPeerVnis,
        AdditionalRouteTargets,
        TopLevelRoutingProfileRouteTargets,
        VpcRouteTargets,
        InterfaceAnycastPrefixes,
        NetworkSecurityGroupRules,
        NetworkSecurityGroupRulePrefixes,
        NetworkSecurityPolicyOverrides,
    }

    impl SetLikeInputReordering {
        /// Reverses the selected collection without changing its members.
        fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
            match self {
                Self::DhcpServers => config.dhcp_servers.reverse(),
                Self::RouteServers => config.route_servers.reverse(),
                Self::SiteFabricPrefixes => config.site_fabric_prefixes.reverse(),
                Self::AdminInterfaceVpcPrefixes => config
                    .admin_interface
                    .as_mut()
                    .expect("comparison fixture has an admin interface")
                    .vpc_prefixes
                    .reverse(),
                Self::TenantVpcPrefixes => config.tenant_interfaces[0].vpc_prefixes.reverse(),
                Self::TenantPeerPrefixes => {
                    config.tenant_interfaces[0].vpc_peer_prefixes.reverse();
                }
                Self::TenantPeerVnis => config.tenant_interfaces[0].vpc_peer_vnis.reverse(),
                Self::AdditionalRouteTargets => {
                    config.additional_route_target_imports.reverse();
                }
                Self::TopLevelRoutingProfileRouteTargets => config
                    .routing_profile
                    .as_mut()
                    .expect("comparison fixture has a top-level routing profile")
                    .route_target_imports
                    .reverse(),
                Self::VpcRouteTargets => {
                    config.tenant_interfaces[0]
                        .vpc_routing_profile
                        .as_mut()
                        .expect("comparison fixture has a VPC routing profile")
                        .route_target_imports
                        .reverse();
                }
                Self::InterfaceAnycastPrefixes => {
                    config.tenant_interfaces[0]
                        .interface_routing_profile
                        .as_mut()
                        .expect("comparison fixture has an interface routing profile")
                        .allowed_anycast_prefixes
                        .reverse();
                }
                Self::NetworkSecurityGroupRules => {
                    config.tenant_interfaces[0]
                        .network_security_group
                        .as_mut()
                        .expect("comparison fixture has a network security group")
                        .rules
                        .reverse();
                }
                Self::NetworkSecurityGroupRulePrefixes => {
                    config.tenant_interfaces[0]
                        .network_security_group
                        .as_mut()
                        .expect("comparison fixture has a network security group")
                        .rules[0]
                        .src_prefixes
                        .reverse();
                }
                Self::NetworkSecurityPolicyOverrides => {
                    config.network_security_policy_overrides.reverse();
                }
            }
        }
    }

    /// Reordering set-like inputs must not cause an unnecessary HBN update.
    #[test]
    fn current_network_version_ignores_set_like_input_order() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(run = |reordering| {
            let mut config = comparison_network_config();
            let mut current = CurrentNetworkVersion::default();
            current.update_from(&config);
            reordering.apply(&mut config);
            current.matches_versions_from(&config)
        };
            "top-level sets" {
                SetLikeInputReordering::DhcpServers => true,
                SetLikeInputReordering::RouteServers => true,
                SetLikeInputReordering::SiteFabricPrefixes => true,
                SetLikeInputReordering::AdditionalRouteTargets => true,
                SetLikeInputReordering::TopLevelRoutingProfileRouteTargets => true,
                SetLikeInputReordering::NetworkSecurityPolicyOverrides => true,
            }
            "interface sets" {
                SetLikeInputReordering::AdminInterfaceVpcPrefixes => true,
                SetLikeInputReordering::TenantVpcPrefixes => true,
                SetLikeInputReordering::TenantPeerPrefixes => true,
                SetLikeInputReordering::TenantPeerVnis => true,
                SetLikeInputReordering::VpcRouteTargets => true,
                SetLikeInputReordering::InterfaceAnycastPrefixes => true,
                SetLikeInputReordering::NetworkSecurityGroupRules => true,
                SetLikeInputReordering::NetworkSecurityGroupRulePrefixes => true,
            }
        );
    }

    /// Selects one collection whose relative order changes HBN behavior.
    #[derive(Clone, Copy, Debug)]
    enum OrderSensitiveInputReordering {
        TenantInterfaces,
        EqualPriorityNetworkSecurityGroupRules,
        EqualPriorityNetworkSecurityPolicyOverrides,
    }

    impl OrderSensitiveInputReordering {
        /// Makes the security-rule cases depend on stable equal-priority
        /// ordering before their entries are reversed.
        fn prepare(self, config: &mut ManagedHostNetworkConfigResponse) {
            match self {
                Self::TenantInterfaces => {}
                Self::EqualPriorityNetworkSecurityGroupRules => {
                    let rules = &mut config.tenant_interfaces[0]
                        .network_security_group
                        .as_mut()
                        .expect("comparison fixture has a network security group")
                        .rules;
                    let priority = rules[0]
                        .rule
                        .as_ref()
                        .expect("comparison fixture has rule attributes")
                        .priority;
                    rules[1]
                        .rule
                        .as_mut()
                        .expect("comparison fixture has rule attributes")
                        .priority = priority;
                }
                Self::EqualPriorityNetworkSecurityPolicyOverrides => {
                    let priority = config.network_security_policy_overrides[0]
                        .rule
                        .as_ref()
                        .expect("comparison fixture has rule attributes")
                        .priority;
                    config.network_security_policy_overrides[1]
                        .rule
                        .as_mut()
                        .expect("comparison fixture has rule attributes")
                        .priority = priority;
                }
            }
        }

        /// Reverses the selected order-sensitive collection.
        fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
            match self {
                Self::TenantInterfaces => config.tenant_interfaces.reverse(),
                Self::EqualPriorityNetworkSecurityGroupRules => config.tenant_interfaces[0]
                    .network_security_group
                    .as_mut()
                    .expect("comparison fixture has a network security group")
                    .rules
                    .reverse(),
                Self::EqualPriorityNetworkSecurityPolicyOverrides => {
                    config.network_security_policy_overrides.reverse();
                }
            }
        }
    }

    /// Interface order and equal-priority rule order must remain part of the
    /// fingerprint because the renderer preserves both.
    #[test]
    fn current_network_version_preserves_semantically_significant_order() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(run = |reordering| {
            let mut config = comparison_network_config();
            reordering.prepare(&mut config);
            let mut current = CurrentNetworkVersion::default();
            current.update_from(&config);
            reordering.apply(&mut config);
            current.matches_versions_from(&config)
        };
            "first-match interface order" {
                OrderSensitiveInputReordering::TenantInterfaces => false,
            }
            "equal-priority security rule order" {
                OrderSensitiveInputReordering::EqualPriorityNetworkSecurityGroupRules => false,
                OrderSensitiveInputReordering::EqualPriorityNetworkSecurityPolicyOverrides => false,
            }
        );
    }

    /// `NonHbnInputChange` lists the response fields this test changes to
    /// prove they stay out of the HBN fingerprint.
    #[derive(Clone, Copy, Debug)]
    enum NonHbnInputChange {
        NetworkPingerType,
        MinimumFunctioningLinks,
        InstanceId,
        InstancePayload,
        DeprecatedDhcpFlag,
        DeprecatedDenyPrefixes,
        NtpServers,
        HostInterfaceId,
        ExtensionServices,
        AstraConfig,
    }

    impl NonHbnInputChange {
        /// Applies the selected non-HBN change to the shared response.
        fn apply(self, config: &mut ManagedHostNetworkConfigResponse) {
            match self {
                Self::NetworkPingerType => {
                    config.dpu_network_pinger_type = Some("icmp".to_string());
                }
                Self::MinimumFunctioningLinks => config.min_dpu_functioning_links = Some(1),
                Self::InstanceId => config.instance_id = Some(Default::default()),
                Self::InstancePayload => config.instance = Some(Default::default()),
                Self::DeprecatedDhcpFlag => config.enable_dhcp = false,
                Self::DeprecatedDenyPrefixes => config
                    .deprecated_deny_prefixes
                    .push("192.0.2.0/24".to_string()),
                Self::NtpServers => config.ntp_servers.push("10.40.0.3".to_string()),
                Self::HostInterfaceId => {
                    config.host_interface_id =
                        Some("7340b4f5-1721-4dd3-8fc3-9f91a4c29f61".to_string());
                }
                Self::ExtensionServices => {
                    config
                        .dpu_extension_services
                        .push(rpc::ManagedHostDpuExtensionServiceConfig {
                            service_id: "extension-service".to_string(),
                            ..Default::default()
                        });
                }
                Self::AstraConfig => config.astra_config = Some(rpc::AstraConfig::default()),
            }
        }
    }

    /// Inputs used outside HBN, read only at startup, or deprecated must not
    /// retrigger HBN.
    #[test]
    fn current_network_version_ignores_non_hbn_inputs() {
        use carbide_test_support::value_scenarios;

        value_scenarios!(run = |change| {
            let mut config = comparison_network_config();
            let mut current = CurrentNetworkVersion::default();
            current.update_from(&config);
            change.apply(&mut config);
            current.matches_versions_from(&config)
        };
            "inputs applied outside HBN rendering" {
                NonHbnInputChange::MinimumFunctioningLinks => true,
                NonHbnInputChange::ExtensionServices => true,
                NonHbnInputChange::AstraConfig => true,
            }
            "startup, status, DHCP-only, and deprecated inputs" {
                NonHbnInputChange::NetworkPingerType => true,
                NonHbnInputChange::InstanceId => true,
                NonHbnInputChange::InstancePayload => true,
                NonHbnInputChange::HostInterfaceId => true,
                NonHbnInputChange::NtpServers => true,
                NonHbnInputChange::DeprecatedDenyPrefixes => true,
                NonHbnInputChange::DeprecatedDhcpFlag => true,
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_get_fabric_interfaces_data() {
        let fabric_interfaces_data = get_fabric_interfaces_data().await.unwrap();
        dbg!(fabric_interfaces_data.as_slice());
        // Under virtualization we probably can't make any assertions about
        // whether this list contains any interfaces, but uncommenting this
        // should pass on any Linux host with real hardware or a virtualized PCI
        // network interface.
        // assert!(fabric_interfaces_data.len() > 0);
    }
}
