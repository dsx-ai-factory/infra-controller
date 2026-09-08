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
use tracing::error;
use version_compare::Version;

use crate::command_line::HbnConfigMode;
use crate::dpu::DpuNetworkInterfaces;
use crate::dpu::interface::Interface;
use crate::dpu::route::{DpuRoutePlan, IpRoute, Route};
use crate::duppet::{SummaryFormat, SyncOptions};
use crate::ethernet_virtualization::{
    InterfaceTranslationMode, NvueClientContext, NvueUpdateFlavor, ServiceAddresses,
};
use crate::fmds_client::{FmdsUpdater, register_external_connection_metric};
use crate::health::HealthCheckParams;
use crate::host_machine_id::get_host_machine_id_retry;
use crate::instrumentation::{
    NetworkStatus, OvsRestart, create_metrics, get_dpu_agent_meter, get_prometheus_registry,
};
use crate::machine_inventory_updater::MachineInventoryUpdaterConfig;
use crate::network_monitor::{self, NetworkPingerType};
use crate::util::get_host_boot_timestamp;
use crate::{
    FMDS_MINIMUM_HBN_VERSION, HBNDeviceNames, NVUE_MINIMUM_HBN_VERSION, RunOptions, astra_weave,
    command_line, ethernet_virtualization, extension_services, get_non_empty_str, hbn, health,
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
pub(super) async fn setup_and_run(
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

    match agent_config
        .telemetry
        .metrics_address
        .parse::<std::net::SocketAddr>()
    {
        Ok(metrics_address) => {
            tracing::info!(
                metrics_address = %metrics_address,
                "Starting Prometheus /metrics endpoint"
            );
            let metrics_config = metrics_endpoint::MetricsEndpointConfig {
                address: metrics_address,
                registry: get_prometheus_registry(),
                health_controller: None,
                additional_prefix: None,
            };
            tokio::task::spawn(async move {
                if let Err(e) = metrics_endpoint::run_metrics_endpoint(&metrics_config).await {
                    tracing::error!(
                        error = format!("{e:#}"),
                        "Prometheus /metrics endpoint exited"
                    );
                }
            });
        }
        Err(e) => {
            tracing::warn!(
                error = format!("{e:#}"),
                "Failed to start Prometheus /metrics endpoint"
            );
        }
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
        let last_connect_succeeded = register_external_connection_metric(&get_dpu_agent_meter());
        FmdsUpdater::External {
            address: fmds_addr.clone(),
            machine_identity: agent_config.machine_identity.clone(),
            connect_timeout: Duration::from_secs(options.fmds_connect_timeout_secs),
            last_connect_succeeded,
        }
    } else {
        if options.enable_metadata_service {
            crate::metadata_service::spawn_metadata_service(
                agent_config.metadata_service.address.clone(),
                metrics.clone(),
                instance_metadata_state.clone(),
            )
            .unwrap_or_else(|e| {
                tracing::warn!(error = format!("{e:#}"), "Failed to run metadata service");
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
            tracing::warn!(
                error = format!("{e:#}"),
                "Error calculating process start timestamp"
            );
        }
    }

    match get_host_boot_timestamp() {
        Ok(timestamp) => {
            metrics.record_machine_boot_time(timestamp);
        }
        Err(e) => {
            tracing::warn!(
                error = format!("{e:#}"),
                "Error getting host boot timestamp"
            );
        }
    }

    // The certificate-expiry gauge re-reads the certificate on every metrics
    // collection, so it follows the renewals ClientCertRenewer performs over
    // the agent's lifetime.
    {
        let forge_client_config = Arc::clone(&forge_client_config);
        metrics.record_client_cert_expiry_time(move || forge_client_config.client_cert_expiry());
    }

    if options.agent_platform_type.is_dpu_os()
        && !agent_config.machine.is_fake_dpu
        && let Err(e) = crate::agent_platform::ensure_doca_containers().await
    {
        // The HBN container health check will notice this problem and
        // turn it into an unhealthy status that gets reported to the
        // API, so we're not going to do much more than log this for
        // now.
        tracing::error!(error = %e, "Couldn't ensure DOCA pods");
    }

    let fmds_minimum_hbn_version = Version::from(FMDS_MINIMUM_HBN_VERSION).ok_or(eyre::eyre!(
        "unable to convert string: {FMDS_MINIMUM_HBN_VERSION} to version"
    ))?;
    let nvue_minimum_hbn_version = Version::from(NVUE_MINIMUM_HBN_VERSION).ok_or(eyre::eyre!(
        "unable to convert string: {NVUE_MINIMUM_HBN_VERSION} to version"
    ))?;

    if options.agent_platform_type.is_dpu_os()
        && let Err(err) = crate::ovs::set_vswitchd_yield().await
    {
        tracing::warn!(error = %err, "Failed asking ovs_vswitchd to not use 100% of a CPU core. Non-fatal.");
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
            tracing::error!(error = ?e, "Failed to get host machine ID after retries");
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

    if options.agent_platform_type.is_dpu_os() {
        if let Err(e) = lldp::prepare_lldp().await {
            tracing::error!(error = %e, "Couldn't prepare LLDP configuration");
        }
        if let Err(e) = lldp::set_lldp_system_description(&machine_id).await {
            tracing::warn!(error = %e, "Couldn't update LLDP system description");
        }
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
            tracing::debug!(%pinger_type, "Starting network monitor");
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
        ca_republish_time: std::time::Instant::now(),
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
        last_ovs_restart_version: None,
        ovs_restart_retry_backoff: None,
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
    ca_republish_time: std::time::Instant,
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
    last_ovs_restart_version: Option<String>,
    ovs_restart_retry_backoff: Option<OvsRestartRetryBackoff>,
}

struct OvsRestartRetryBackoff {
    managed_host_config_version: String,
    retry_after: Instant,
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
    /// Hash of the supplemental network config file's contents at the last
    /// successful reconciliation. The file path never changes between
    /// iterations, so only a content hash can detect an in-place edit.
    supplemental_config_hash: Option<u64>,
}

impl CurrentNetworkVersion {
    /// Returns whether the explicit versions and HBN inputs from the response,
    /// plus the supplemental network config contents, match the last
    /// successful network reconciliation.
    fn matches_versions_from(
        &self,
        conf: &ManagedHostNetworkConfigResponse,
        supplemental_config: Option<&str>,
    ) -> bool {
        if self.supplemental_config_hash != supplemental_config.map(Self::hash_supplemental_config)
        {
            tracing::info!("Supplemental network config changed");
            return false;
        }
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

    /// Records the versions and HBN inputs from the response, plus the
    /// supplemental network config contents, after network reconciliation
    /// succeeds.
    fn update_from(
        &mut self,
        conf: &ManagedHostNetworkConfigResponse,
        supplemental_config: Option<&str>,
    ) {
        self.managed_host_config_version =
            get_non_empty_str(&conf.managed_host_config_version).map(String::from);
        self.instance_network_config_version =
            get_non_empty_str(&conf.instance_network_config_version).map(String::from);
        self.rendered_inputs_hash
            .replace(Self::hash_rendered_inputs(conf));
        self.supplemental_config_hash = supplemental_config.map(Self::hash_supplemental_config);
    }

    /// Hashes the supplemental network config file's contents for change
    /// detection. Hashing the path alone would make in-place edits invisible.
    fn hash_supplemental_config(contents: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        contents.hash(&mut hasher);
        hasher.finish()
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
        config.use_admin_network_changed = None;

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

        // HBN rendering does not consume the family-neutral address list. Exclude
        // it from the fingerprint so changes to that staged field do not trigger
        // an apply that cannot render them.
        if let Some(admin_interface) = &mut config.admin_interface {
            admin_interface.addresses.clear();
        }
        for interface in &mut config.tenant_interfaces {
            interface.addresses.clear();
        }
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
                tracing::warn!(
                    error = format!("{e:#}"),
                    "Failed to fetch DHCP timestamps via gRPC"
                );
                vec![]
            }
        };
    }

    let mut dhcp_timestamps = DhcpTimestamps::new(DhcpTimestampsFilePath::Dpu);
    if let Err(e) = dhcp_timestamps.read() {
        tracing::warn!(
            dhcp_timestamps_path = %DhcpTimestampsFilePath::Dpu.path_str(),
            error = %e,
            "Failed to read DHCP timestamps file"
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

    async fn restart_ovs_after_admin_network_change_if_needed(
        &mut self,
        conf: &ManagedHostNetworkConfigResponse,
        status_out: &mut rpc::DpuNetworkStatus,
    ) -> bool {
        if !conf.use_admin_network_changed.unwrap_or_default() {
            return true;
        }

        let now = Instant::now();
        let mut can_ack_network_config = true;
        if !self.options.agent_platform_type.is_dpu_os() {
            tracing::info!(
                agent_platform_type = ?self.options.agent_platform_type,
                managed_host_config_version =
                    conf.managed_host_config_version.as_str(),
                "Skip OVS restart because agent is not running on DPU OS"
            );
        } else if self.last_ovs_restart_version.as_deref()
            == Some(conf.managed_host_config_version.as_str())
        {
            tracing::info!(
                managed_host_config_version = conf.managed_host_config_version.as_str(),
                "Skip OVS restart because this network config version already restarted OVS"
            );
        } else if self
            .ovs_restart_retry_backoff
            .as_ref()
            .is_some_and(|backoff| {
                backoff.managed_host_config_version == conf.managed_host_config_version
                    && now < backoff.retry_after
            })
        {
            tracing::warn!(
                managed_host_config_version = conf.managed_host_config_version.as_str(),
                "Skip OVS restart retry because backoff is still active"
            );
            status_out.network_config_error =
                Some("waiting to retry OVS restart after prior failure".to_string());
            can_ack_network_config = false;
        } else {
            tracing::info!(
                managed_host_config_version = conf.managed_host_config_version.as_str(),
                "Restart OVS because use_admin_network_changed is set to true"
            );
            if let Err(err) = crate::ovs::restart_ovs()
                .await
                .wrap_err("restarting OVS after admin network change")
            {
                carbide_instrument::emit(OvsRestart::Retrying {
                    error: format!("{err:#}"),
                    managed_host_config_version: conf.managed_host_config_version.clone(),
                });
                status_out.network_config_error = Some(err.to_string());
                self.ovs_restart_retry_backoff = Some(OvsRestartRetryBackoff {
                    managed_host_config_version: conf.managed_host_config_version.clone(),
                    retry_after: Instant::now() + OVS_RESTART_RETRY_BACKOFF,
                });
                can_ack_network_config = false;
            } else {
                self.last_ovs_restart_version = Some(conf.managed_host_config_version.clone());
                self.ovs_restart_retry_backoff = None;
            }
        }
        tracing::info!(can_ack_network_config, "Finished restarting OVS");

        can_ack_network_config
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
        let mut has_changed_hbn_config = false;
        let mut current_host_network_config_version = None;
        let mut current_instance_network_config_version = None;
        let mut current_instance_config_version = None;
        let mut current_instance_id = None;
        let mut current_extension_service_version = None;

        let client_certificate_expiry_unix_epoch_secs =
            self.forge_client_config.client_cert_expiry();

        let fabric_interfaces = get_fabric_interfaces_data().await.unwrap_or_else(|err| {
            tracing::warn!(
                error = format!("{err:#}"),
                "Error getting link data for fabric interfaces"
            );
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

                // The fetcher projects `addresses` into the compatibility prefix before use.
                #[allow(deprecated)]
                let proposed_routes: Vec<_> = conf
                    .tenant_interfaces
                    .iter()
                    .filter_map(|interface| interface.prefix.as_deref())
                    .filter_map(|prefix| IpNetwork::from_str(prefix).ok())
                    .collect();

                let tenant_peers = ethernet_virtualization::tenant_peers(&conf);
                let mut should_check_ipv6_unicast = false;
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
                                    "couldn't parse HBN version from NVUE system build (\"{nvue_system_build}\")"
                                )),
                            }?
                        }
                    };

                    let hbn_version = Version::from(hbn_version.as_str())
                        .ok_or(eyre::eyre!("unable to convert string to version"))?;
                    // HBN changed their naming scheme in HBN 2.3 from _sf to _if so we will pass that little bit around
                    // after doing an initial version check instead of assuming _sf
                    self.hbn_device_names = HBNDeviceNames::new(hbn_version.clone());

                    // NVUE is the only supported configuration path. If the DPU's
                    // HBN/DOCA is too old to support NVUE, we cannot configure it.
                    if hbn_version < self.nvue_minimum_hbn_version {
                        return Err(eyre::eyre!(
                            "HBN version {hbn_version} is older than the minimum required for NVUE ({NVUE_MINIMUM_HBN_VERSION})"
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
                            machine_id = %self.machine_id,
                            error = %e,
                            "Failed to ensure HBN container file configuration"
                        );
                    }

                    tracing::trace!(network_config = ?conf, "Desired network config");
                    // Resolve the virtualization type used to generate HBN
                    // configuration, including any RunOptions override. The IPv6
                    // health gate follows that same effective value.
                    let virtualization_type = effective_virtualization_type(&conf, &self.options)?;
                    should_check_ipv6_unicast =
                        ipv6_unicast_health_enabled(&conf, virtualization_type);

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

                    // Read the supplemental network config (if configured) on
                    // every iteration so in-place edits to the mounted file are
                    // picked up. A configured-but-unreadable file fails the
                    // update loudly instead of silently applying an unmerged
                    // config.
                    let mut supplemental_config_error = None;
                    let supplemental_config =
                        match &self.agent_config.network.supplemental_config_path {
                            None => None,
                            Some(path) => match std::fs::read_to_string(path) {
                                Ok(contents) => Some(contents),
                                Err(err) => {
                                    supplemental_config_error = Some(eyre::eyre!(
                                        "couldn't read supplemental network config {}: {err}",
                                        path.display()
                                    ));
                                    None
                                }
                            },
                        };

                    let update_result = if let Some(err) = supplemental_config_error {
                        Err(err)
                    } else if self
                        .current_network_version
                        .matches_versions_from(&conf, supplemental_config.as_deref())
                    {
                        tracing::debug!(
                            current_network_version = ?self.current_network_version,
                            "No configuration change, skipping HBN updates"
                        );
                        Ok(false)
                    } else {
                        if self.options.agent_platform_type.is_dpu_os()
                            && hbn_version >= self.fmds_minimum_hbn_version
                        {
                            let fmds_proposed_interfaces = &self.agent_config.fmds_armos_networking;
                            let network_plan = DpuNetworkInterfaces::new(fmds_proposed_interfaces);

                            let fmds_interface_plan =
                                Interface::plan(self.hbn_device_names.sfs[0], network_plan).await?;
                            tracing::trace!(interface_plan = ?fmds_interface_plan, "Interface plan");

                            Interface::apply(fmds_interface_plan).await?;

                            // plan_fmds_armos_routing reads the interface's current IPv4 address
                            // for prefsrc and returns Ok(None) when no address is present, skipping
                            // route installation entirely. The config-version cache can prevent a
                            // retry on the next tick, so the interface address must be applied
                            // before the route plan is computed.
                            let route_plan = plan_fmds_armos_routing(
                                self.hbn_device_names.sfs[0],
                                &proposed_routes,
                            )
                            .await?;
                            tracing::trace!(route_plan = ?route_plan, "Route plan");

                            if let Some(route_plan) = route_plan {
                                Route::apply(route_plan).await?;
                            }
                        }

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
                            supplemental_config.as_deref(),
                        )
                        .await
                    };

                    let astra_config_status =
                        astra_weave::update_weave_ew_vpc_astra_config(conf.astra_config.as_ref())
                            .await;

                    let joined_result = match (update_result, dhcp_result, astra_config_status) {
                        (Ok(hbn_changed), Ok(dhcp_changed), Ok(spx_net_status)) => {
                            Ok((hbn_changed, dhcp_changed, spx_net_status))
                        }
                        (update_result, dhcp_result, astra_config_status) => {
                            let mut errors = Vec::new();

                            if let Err(err) = update_result {
                                errors.push(format!("update={err:#}"));
                            }
                            if let Err(err) = dhcp_result {
                                errors.push(format!("dhcp={err:#}"));
                            }
                            if let Err(err) = astra_config_status {
                                errors.push(format!("spx={err:#}"));
                            }

                            Err(eyre::eyre!("network update failed: {}", errors.join(", ")))
                        }
                    };
                    match joined_result {
                        Ok((hbn_changed, dhcp_changed, astra_config_status)) => {
                            self.current_network_version
                                .update_from(&conf, supplemental_config.as_deref());
                            has_changed_hbn_config = hbn_changed;
                            has_changed_configs = hbn_changed || dhcp_changed;
                            if conf.astra_config.is_some() {
                                status_out.astra_config_status = Some(astra_config_status);
                            }
                            if self.options.agent_platform_type.is_dpu_os()
                                && let Err(err) = mtu::ensure().await
                            {
                                tracing::error!(error = %err, "Error reading/setting MTU for p0 or p1");
                            }

                            let can_ack_network_config = self
                                .restart_ovs_after_admin_network_change_if_needed(
                                    &conf,
                                    &mut status_out,
                                )
                                .await;

                            if can_ack_network_config {
                                (
                                    current_host_network_config_version,
                                    current_instance_network_config_version,
                                ) = ack_network_config_update(
                                    &conf,
                                    instance_data.as_deref(),
                                    &mut status_out,
                                );
                            }

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

                let min_healthy_links = conf.min_dpu_functioning_links.unwrap_or(2) as usize;
                let health_report = match self.nvue_context.as_ref() {
                    None => {
                        // In `ContainerExec` mode, the DHCP flag represents a real local
                        // reload, so keep the existing wait while that service restarts.
                        health::health_check(HealthCheckParams {
                            hbn_root: &self.agent_config.hbn.root_dir,
                            host_routes: &tenant_peers,
                            has_changed_configs,
                            min_healthy_links,
                            route_servers: &conf.route_servers,
                            should_check_ipv6_unicast,
                            hbn_device_names: self.hbn_device_names.clone(),
                            include_dhcp_server: !conf.use_admin_network || conf.is_primary_dpu,
                            run_restricted_mode_check: false,
                        })
                        .await
                    }
                    Some(nvue_context) => {
                        // DHCP gRPC reports every accepted request as changed, so only an
                        // actual HBN update may trigger the NVUE wait after a config update.
                        health::nvue::NvueHealthCheck {
                            nvue_client: &nvue_context.nvue_client,
                            min_healthy_links,
                            hbn_device_names: &self.hbn_device_names,
                            has_changed_hbn_config,
                        }
                        .health_check()
                        .await
                    }
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

        // Beside renewal because the two revolve around the same files:
        // renewal rewrites the credentials, and key-less consumers need the
        // resulting trust anchor mirrored into `pub/` (issue #355). Driving it
        // from here rather than a spawned task means it inherits the loop's
        // shutdown handling, and a panic takes the agent down for a restart
        // instead of silently leaving consumers on a stale anchor.
        if now > self.ca_republish_time {
            self.ca_republish_time = now.add(crate::CA_REPUBLISH_INTERVAL);
            crate::republish_bootstrap_ca_if_changed(&self.agent_config.forge_system.root_ca);
        }

        if now > self.inventory_updater_time {
            self.inventory_updater_time =
                now.add(self.inventory_updater_config.update_inventory_interval);
            if let Err(err) =
                machine_inventory_updater::single_run(&self.inventory_updater_config).await
            {
                tracing::error!(error = %err, "machine_inventory_updater error");
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
            health_probe_alert_count = health_alerts.len(),
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
                        forge_api_server = %self.forge_api_server,
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

/// Enables IPv6-unicast health only after the effective FNN configuration has
/// the dedicated IPv6 loopback used by that underlay.
///
/// FNN templates activate the address family before the loopback pool is
/// available, so FRR summary-key presence cannot serve as the activation
/// signal.
fn ipv6_unicast_health_enabled(
    conf: &ManagedHostNetworkConfigResponse,
    virtualization_type: VpcVirtualizationType,
) -> bool {
    virtualization_type == VpcVirtualizationType::Fnn
        && conf
            .managed_host_config
            .as_ref()
            .is_some_and(|config| config.loopback_ip_v6.is_some())
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
                default_virtualization_type = %VpcVirtualizationType::EthernetVirtualizer,
                "Missing network_virtualization_type, defaulting"
            );
            VpcVirtualizationType::EthernetVirtualizer
        });

    Ok(virtualization_type)
}

fn ack_network_config_update(
    conf: &ManagedHostNetworkConfigResponse,
    instance_data: Option<&periodic_config_fetcher::InstanceMetadata>,
    status_out: &mut rpc::DpuNetworkStatus,
) -> (Option<String>, Option<String>) {
    // Updating network config succeeded.
    // Tell the server about the applied version.
    status_out.network_config_version = Some(conf.managed_host_config_version.clone());
    status_out.instance_id = conf.instance_id;
    // On the admin network we don't have to report the instance network config version
    if !conf.instance_network_config_version.is_empty() {
        status_out.instance_network_config_version = Some(
            match conf
                .instance_network_config_version
                .parse::<config_version::ConfigVersion>()
            {
                Ok(managed_host_instance_network_config_version) => {
                    match instance_data.map(|instance| instance.network_config_version) {
                        Some(instance_metadata_network_config_version) => {
                            // Report the older version of the versions received via 2 path
                            // That makes sure we don't report progress if we haven't received the newest version
                            // via both path.
                            let reported_instance_network_config_version =
                                managed_host_instance_network_config_version
                                    .min_by_timestamp(&instance_metadata_network_config_version);
                            if instance_metadata_network_config_version
                                != managed_host_instance_network_config_version
                            {
                                tracing::warn!(
                                    managed_host_version = %managed_host_instance_network_config_version,
                                    instance_metadata_version = %instance_metadata_network_config_version,
                                    reported_version = %reported_instance_network_config_version,
                                    "Different instance network config version received"
                                );
                            }
                            reported_instance_network_config_version.version_string()
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

    (
        status_out.network_config_version.clone(),
        status_out.instance_network_config_version.clone(),
    )
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

    tracing::trace!(?fmds_interface, "fmds_interface");

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
async fn record_network_status(
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
            NetworkStatus::ConnectionFailed {
                forge_api: forge_api.to_string(),
                error: format!("{err:#}"),
            }
            .emit();
            return;
        }
    };
    let request = tonic::Request::new(status);
    let result = client.record_dpu_network_status(request).await;
    match &result {
        Ok(_) => NetworkStatus::Succeeded.emit(),
        Err(err) => NetworkStatus::RpcFailed {
            error: format!("{err:#}"),
        }
        .emit(),
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
                    tracing::warn!(interface_name, "The MAC address was missing or empty");
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
const OVS_RESTART_RETRY_BACKOFF: Duration = Duration::from_secs(60);

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
            error!(error = %e, "Could not read bfvcheck.out");
            eyre::eyre!("could not read bfvcheck.out: {}", e)
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
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

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
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;
        // This is not a typo, we have to run it twice as per NBU
        tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", "sync"]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());
        tokio::time::timeout(crate::dpu::COMMAND_TIMEOUT, cmd.output())
            .await
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;

        // And now for the pièce de résistance, a reboot inline on the dpu OS, and this command
        // takes a LONG time so we will put an egregiously large reboot time
        let mut cmd = tokio::process::Command::new("bash");
        cmd.args(vec!["-c", "reboot"]);
        cmd.kill_on_drop(true);

        let cmd_str = pretty_cmd(cmd.as_std());
        tokio::time::timeout(Duration::from_secs(60 * 10), cmd.output())
            .await
            .wrap_err_with(|| format!("timeout while running command: {cmd_str:?}"))??;
    }

    // This method will either reboot a card or just return ok.
    Ok(())
}

#[cfg(test)]
#[path = "tests/main_loop.rs"]
mod tests;
