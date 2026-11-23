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

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use bmc_vendor::BMCVendor;
use carbide_authn::config::{AllowedCertCriteria, TrustConfig};
use carbide_dpf::types::DpfProxyDetails;
use carbide_firmware::FirmwareConfig;
use carbide_firmware::defaults::{
    BF2_BMC_VERSION, BF2_CEC_VERSION, BF2_NIC_VERSION, BF2_UEFI_VERSION, BF3_BMC_VERSION,
    BF3_CEC_VERSION, BF3_NIC_VERSION, BF3_UEFI_VERSION,
};
use carbide_ib_fabric::config::{IBFabricConfig, IbFabricDefinition};
use carbide_machine_controller::config::power_manager::default_power_options;
use carbide_machine_controller::config::{
    BomValidationConfig, FirmwareGlobal, MachineStateControllerConfig,
    MachineStateHandlerSiteConfig, MachineValidationConfig, PowerManagerOptions, TimePeriod,
};
use carbide_nvlink_manager::config::NvLinkConfig;
use carbide_preingestion_manager::PreingestionManagerConfig;
use carbide_rack_controller::config::{RackValidationConfig, RmsConfig};
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_state_controller_common::config::StateControllerConfig;
use carbide_utils::config::{as_duration, as_option_duration, as_std_duration};
use chrono::Duration;
use db::host_naming::HostNamingStrategyKind;
use duration_str::{deserialize_duration, deserialize_duration_chrono};
use figment::Figment;
use health_report::HealthAlertClassification;
use ipnetwork::{IpNetwork, Ipv4Network};
use itertools::Itertools;
use libmlx::firmware::config::FirmwareFlasherProfile;
use libmlx::profile::profile::MlxConfigProfile;
use libmlx::profile::serialization::{
    deserialize_option_profile_map, serialize_option_profile_map,
};
use model::firmware::{
    AgentUpgradePolicyChoice, Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry,
};
use model::machine::HostHealthConfig;
use model::network_security_group::NetworkSecurityGroupRule;
use model::network_segment::NetworkDefinition;
use model::resource_pool::define::ResourcePoolDef;
use model::tenant::identity_config::SigningAlgorithm;
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};

pub(crate) const DEFAULT_DPU_NUM_OF_VFS: u32 = 16;
pub(crate) const MAX_DPU_NUM_OF_VFS: u32 = 126;

/// Parses an optional duration ("30d", "12h", ...; absent = `None`) into
/// `Option<chrono::Duration>`. Hand-rolled because `duration_str` deprecated
/// its own Option variant -- we do NOT use the deprecated function.
fn deserialize_option_duration_chrono<'de, D>(
    deserializer: D,
) -> Result<Option<chrono::Duration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|value| duration_str::parse_chrono(&value).map_err(serde::de::Error::custom))
        .transpose()
}

/// nico-api configuration file content
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CarbideConfig {
    /// Socket address for the gRPC API server, used by
    /// clients and nico-admin-cli to connect.
    /// Default is `[::]:1079`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

    /// Run this instance passively: no background services,
    /// just listen for RPC/web connections. Used in dev mode
    /// when running a second nico instance against a
    /// cluster that already has a "full" instance.
    #[serde(default)]
    pub listen_only: bool,

    /// Socket address for the HTTP server that serves
    /// Prometheus metrics under `/metrics`.
    pub metrics_endpoint: Option<SocketAddr>,

    /// Alternative metric prefix emitted alongside `carbide_`,
    /// used for dual-emitting while migrating dashboards and
    /// alerts. Increases observability system load.
    pub alt_metric_prefix: Option<String>,

    /// Postgres connection string used by the API server
    /// for all persistent state.
    pub database_url: String,

    /// Maximum size of the database connection pool.
    /// Default is 1000.
    #[serde(default = "default_max_database_connections")]
    pub max_database_connections: u32,

    /// InfiniBand fabric configuration, used by the IB
    /// fabric manager for partition and UFM management.
    pub ib_config: Option<IBFabricConfig>,

    /// Autonomous System Number, fixed per environment.
    /// Used by nico-dpu-agent to write `frr.conf` for
    /// BGP routing.
    pub asn: u32,

    /// DHCP server addresses announced to DPUs during
    /// network provisioning.
    #[serde(default)]
    pub dhcp_servers: Vec<Ipv4Addr>,

    /// NTP server IP addresses for the site.
    #[serde(default)]
    pub ntp_servers: Vec<Ipv4Addr>,

    /// Route server IP addresses for L2VPN (Ethernet
    /// Virtual) network support on DPUs.
    #[serde(default)]
    pub route_servers: Vec<IpAddr>,

    /// Enables route server injection into DPU FRR
    /// configs for L2VPN Ethernet Virtual networks.
    #[serde(default)]
    pub enable_route_servers: bool,

    /// List of IPv4 prefixes (in CIDR notation) that tenant instances are not allowed to talk to.
    //
    // TODO(chet): For now, this remains `Vec<Ipv4Network>`, because the dpu-agent consumers
    // that process deny prefixes are IPv4-only (and I'll do it in another PR):
    // - `crates/agent/src/acl_rules.rs` parses rules into `Ipv4Network` and generates
    //   iptables DROP rules via `make_deny_prefix_rules(&[Ipv4Network], ...)`
    // - nvue templates (in `nvue_startup_fnn.conf` and `nvue_startup_etv.conf`) render these
    //   prefixes under a "p0000_deny_prefixes_ipv4" ACL policy with `type: ipv4`.
    //
    // Updating to support `Vec<IpNetwork>` requires the agent to generate parallel IPv6 deny
    // rules (I think via ip6tables / `type: ipv6` ACL policy), similar to how NSG rules already
    // handle the `ipv6: bool` split.
    #[serde(default)]
    pub deny_prefixes: Vec<Ipv4Network>,

    /// List of IP prefixes (in CIDR notation) that are assigned for tenant
    /// use within this site. Supports both IPv4 and IPv6 prefixes.
    #[serde(default)]
    pub site_fabric_prefixes: Vec<IpNetwork>,

    /// List of aggregate IPv4 prefixes (in CIDR notation) that contain prefixes assigned
    /// to tenants so that they themselves can announce to the DPU.  E.g., BYOIP
    #[serde(default)]
    pub anycast_site_prefixes: Vec<Ipv4Network>,

    /// An ASN allocated for tenants to use
    /// when they peer with the DPU.
    /// If configured, the DPU will expect the host
    /// to peer with this ASN.  If left unset
    /// remote-as external will be used, allowing
    /// any ASN.
    pub common_tenant_host_asn: Option<u32>,

    /// VPC isolation policy enforced on tenant traffic.
    /// Controls whether VPCs are mutually isolated or open.
    #[serde(default)]
    pub vpc_isolation_behavior: VpcIsolationBehaviorType,

    /// Strategy for deriving machine hostnames: `ip_address` (default), `fun`
    /// (stable adjective-noun handles), `serial_number`, or `mac_address`.
    /// Only `fun` leaves existing hostnames alone (it keeps any real name);
    /// the others re-derive, so switching to one progressively renames
    /// existing interfaces as they reconcile. `serial_number` errors on
    /// duplicate serials rather than assigning a substitute name.
    #[serde(default)]
    pub host_naming_strategy: HostNamingStrategyKind,

    /// Pinger implementation type (e.g., "OobNetBind") used
    /// by the DPU network monitor to health-check DPU links.
    #[serde(default)]
    pub dpu_network_monitor_pinger_type: Option<String>,

    /// TLS certificate and key paths for securing gRPC and
    /// HTTP connections.
    pub tls: Option<TlsConfig>,

    /// Transport mode for the gRPC API server.
    /// Default is `Tls`.
    #[serde(default)]
    pub listen_mode: ListenMode,

    /// Authentication and authorization configuration
    /// including Casbin policies and client certificate
    /// trust settings.
    pub auth: Option<AuthConfig>,

    /// Resource pools that allocate IPs, VNIs, etc.
    /// Required, but wrapped in `Option` so partial configs
    /// can be deserialized and merged.
    pub pools: Option<HashMap<String, ResourcePoolDef>>,

    /// Networks to create at startup. Use the
    /// `CreateNetworkSegment` gRPC to create them later
    /// instead.
    pub networks: Option<HashMap<String, NetworkDefinition>>,

    /// VPCs to create at startup. Use the
    /// `CreateVpc` gRPC to create them later
    /// instead.
    pub vpcs: Option<HashMap<String, VpcDefinition>>,

    /// IPMI tool implementation for DPU power control
    /// (e.g., "prod" or "fake").
    pub dpu_ipmi_tool_impl: Option<String>,

    /// Number of retries when IPMI returns an error during
    /// DPU reboot.
    pub dpu_ipmi_reboot_attempts: Option<u32>,

    /// Number of consecutive HTTP 401/403 responses from a BMC before the
    /// session-token path stops attempting to log in to that BMC, to avoid
    /// exhausting the BMC root account's retry budget.
    /// Default is 3.
    #[serde(default = "default_bmc_session_lockout_threshold")]
    pub bmc_session_lockout_threshold: u32,

    /// When `true`, `GetBmcCredentials` may return
    /// `UsernamePassword` credentials for BMCs whose Redfish ServiceRoot
    /// does not expose `SessionService`. When `false` (the default), such
    /// BMCs surface a `NoSessionService` error to the caller and no
    /// basic-auth fallback is performed. See the "Basic-auth fallback"
    /// section of `crates/api/src/credentials/bmc_session_manager.rs` for
    /// the full semantics.
    #[serde(default)]
    pub allow_bmc_basic_auth_fallback: bool,

    /// Infiniband fabrics managed by the site
    /// Note: At the moment, only a single fabric is supported
    #[serde(default)]
    pub ib_fabrics: HashMap<String, IbFabricDefinition>,

    /// Domain to create if there are no domains.
    ///
    /// Most sites use a single domain for their lifetime. This is that domain.
    /// The alternative is to create it via `CreateDomain` grpc endpoint.
    pub initial_domain_name: Option<String>,

    /// The policy we use to decide whether a specific nico-dpu-agent
    /// should be upgraded.
    ///
    /// Also settable via a `nico-admin-cli` command.
    pub initial_dpu_agent_upgrade_policy: Option<AgentUpgradePolicyChoice>,

    /// Deprecated, use machine_updater
    pub max_concurrent_machine_updates: Option<i32>,

    /// The interval at which the machine update manager checks for machine updates in seconds.
    pub machine_update_run_interval: Option<u64>,

    /// How long a retained boot interface pair (see the
    /// `retained_boot_interfaces` table) stays applicable after its
    /// `machine_interfaces` row was deleted. The default (`None`) retains
    /// forever: if the machine eventually comes back, the pair is waiting.
    /// Set a window (e.g. "30d") to keep a MAC that reappears on different
    /// hardware from inheriting an obsolete Redfish interface id.
    #[serde(
        default,
        deserialize_with = "deserialize_option_duration_chrono",
        serialize_with = "as_option_duration"
    )]
    pub retained_boot_interface_window: Option<chrono::Duration>,

    /// SiteExplorer related configuration
    #[serde(default)]
    pub site_explorer: SiteExplorerConfig,

    /// The policy to decide whether two VPCs are allowed to peer with each other based on their
    /// network virtualization type during creation
    pub vpc_peering_policy: Option<VpcPeeringPolicy>,

    /// The policy to decide whether a VPC peering should be active
    pub vpc_peering_policy_on_existing: Option<VpcPeeringPolicy>,

    /// Controls whether or not machine attestion is required before a machine
    /// can go from Discovered -> Ready (and, when enabled, introduces the new
    /// `Measuring` state to the flow).
    ///
    /// This control exists so we can roll it out on a site-by-site basis,
    /// which includes making sure the latest Scout image for the site has
    /// been deployed with attestation support (and knows Action::MEASURE).
    #[serde(default)]
    pub attestation_enabled: bool,

    /// *** This mode is for testing purposes and is not widely supported right now ***
    /// Controls if machines allowed to be registered without TPM module,
    /// in this case for stable machine identifier api will use chasis serial.
    /// Set `true` by default
    #[serde(default = "default_to_true")]
    pub tpm_required: bool,

    /// MachineStateController related configuration parameter
    #[serde(default)]
    pub machine_state_controller: MachineStateControllerConfig,

    /// NetworkSegmentController related configuration parameter
    #[serde(default)]
    pub network_segment_state_controller: NetworkSegmentStateControllerConfig,

    /// VpcPrefixStateController related configuration parameter
    #[serde(default)]
    pub vpc_prefix_state_controller: VpcPrefixStateControllerConfig,

    /// IbPartitionStateController related configuration parameter
    #[serde(default)]
    pub ib_partition_state_controller: IbPartitionStateControllerConfig,

    /// DpaInterfaceStateController related configuration parameter
    #[serde(default)]
    pub dpa_interface_state_controller: DpaInterfaceStateControllerConfig,

    /// RackStateController related configuration parameter
    #[serde(default)]
    pub rack_state_controller: RackStateControllerConfig,

    /// PowerShelfStateController related configuration parameter
    #[serde(default)]
    pub power_shelf_state_controller: PowerShelfStateControllerConfig,

    /// SwitchStateController related configuration parameter
    #[serde(default)]
    pub switch_state_controller: SwitchStateControllerConfig,

    /// SpdmStateController related configuration parameter
    #[serde(default)]
    pub spdm_state_controller: SpdmStateControllerConfig,

    /// Maps host model identifiers to firmware definitions,
    /// used by the firmware manager to determine BMC, UEFI,
    /// and NIC upgrade targets for each host type.
    #[serde(default)]
    pub host_models: HashMap<String, Firmware>,

    /// Global firmware update settings: upload concurrency,
    /// retry intervals, autoupdate policies, and firmware
    /// binary storage paths.
    #[serde(default)]
    pub firmware_global: FirmwareGlobal,

    /// Machine update policies: auto-reboot windows and
    /// concurrent update limits used by the machine update
    /// manager.
    #[serde(default)]
    pub machine_updater: MachineUpdater,

    /// Maximum number of IDs accepted by
    /// `find_*_by_ids` APIs to prevent oversized queries.
    /// Default is 100.
    #[serde(default = "default_max_find_by_ids")]
    pub max_find_by_ids: u32,

    /// Network security group settings: max expanded rule
    /// count, stateful ACL enforcement, and policy overrides
    /// injected before user-defined rules.
    #[serde(default)]
    pub network_security_group: NetworkSecurityGroupConfig,

    /// Minimum functioning DPU links required for the DPU
    /// to be considered healthy. If unset, all links must
    /// be functional.
    #[serde(default)]
    pub min_dpu_functioning_links: Option<u32>,

    /// Host health monitoring thresholds, used by the
    /// machine state controller to determine hardware health
    /// and DPU agent version compliance.
    #[serde(default)]
    pub host_health: HostHealthConfig,

    /// Observability settings shared across all state controllers, e.g.
    /// opt-in per-object metrics.
    #[serde(default)]
    pub observability: ObservabilityConfig,

    /// Network infrastructure-provided L3 VNI for FNN VPC Internet
    /// connectivity. Combined with `datacenter_asn` to form
    /// a route-target. If unset, VPCs cannot reach the
    /// Internet.
    /// Default is 100001.
    //
    // TODO(chet): This might be interesting to toggle on
    // a per-VPC basis (e.g. a VPC guaranteed not to access
    // the Internet).
    #[serde(default = "default_internet_l3_vni")]
    pub internet_l3_vni: u32,

    /// Measured boot metrics collector configuration.
    /// Exports TPM-based boot measurement data as
    /// Prometheus metrics for attestation monitoring.
    #[serde(default)]
    pub measured_boot_collector: MeasuredBootMetricsCollectorConfig,

    /// Machine validation test configuration. Runs
    /// hardware tests (memory latency, SSD I/O, etc.)
    /// after ingestion to verify machine health.
    #[serde(default)]
    pub machine_validation_config: MachineValidationConfig,

    /// Rack-level validation configuration. Runs
    /// multi-node partition tests after firmware upgrade
    /// and maintenance to verify rack health.
    #[serde(default)]
    pub rack_validation_config: RackValidationConfig,

    /// Machine identity (SPIFFE JWT-SVID) settings,
    /// used by `SignMachineIdentity` to issue short-lived
    /// identity tokens to tenant workloads.
    /// Section `[machine_identity]`.
    #[serde(default)]
    pub machine_identity: MachineIdentityConfig,

    /// Disables role-based access control enforcement.
    /// Intended for testing and development only.
    #[serde(default)]
    pub bypass_rbac: bool,

    /// DPU-specific firmware and provisioning config,
    /// including DPU model definitions, NIC firmware
    /// versions, and secure boot settings.
    #[serde(default)]
    pub dpu_config: DpuConfig,

    /// Fabric Nearest Neighbor (FNN) configuration for
    /// L3 VNI-based overlay networking, including routing
    /// profiles and route target import/export policies.
    #[serde(default)]
    pub fnn: Option<FnnConfig>,

    /// Bill-of-materials (BOM) validation settings.
    /// Ensures machines match expected SKU configurations
    /// before being marked as Ready.
    #[serde(default)]
    pub bom_validation: BomValidationConfig,

    /// BIOS profile definitions organized by vendor and
    /// model, used by SiteExplorer to apply Redfish BIOS
    /// settings during ingestion.
    #[serde(default)]
    pub bios_profiles: libredfish::BiosProfileVendor,

    /// Default BIOS profile type (e.g., Performance,
    /// PowerEfficiency) applied to machines when no
    /// per-model override exists.
    #[serde(default)]
    pub selected_profile: libredfish::BiosProfileType,

    /// Vendor-specific iDRAC/BMC manager attributes applied during machine_setup,
    /// before BMC lockdown. Keyed by vendor → model → profile → attribute name.
    ///
    /// These target the manager OEM attributes endpoint (e.g.
    /// `Managers/{id}/Oem/Dell/DellAttributes/{id}` on Dell), as opposed to
    /// `bios_profiles` which targets BIOS settings.
    ///
    /// Model names are normalized to lowercase with spaces replaced by underscores
    /// (e.g. `"PowerEdge R760"` → `"poweredge_r760"`).
    ///
    /// Example (carbide.toml):
    /// ```toml
    /// # Disable PSU Hot Spare on Dell R760 to prevent fan spin-up (nvbugs-5834644)
    /// [oem_manager_profiles.Dell.poweredge_r760.performance]
    /// "ServerPwr.1.PSRapidOn" = "Disabled"
    /// ```
    #[serde(default)]
    pub oem_manager_profiles: libredfish::BiosProfileVendor,

    /// DpaConfig refers to East West Ethernet (aka
    /// Cluster Interconnect Network) configuration
    #[serde(default)]
    pub dpa_config: Option<DpaConfig>,

    /// DSX Exchange Event Bus configuration. Publishes
    /// `ManagedHostState` transitions, BMS rack leak/isolation
    /// values, and heartbeat timestamps over MQTT, and subscribes
    /// to BMS metadata topics used to route those values.
    #[serde(default)]
    pub dsx_exchange_event_bus: Option<DsxExchangeEventBusConfig>,

    /// Datacenter ASN used by FNN to build DC-specific
    /// route targets for VRF import and export.
    /// Default is 11414.
    #[serde(default = "default_datacenter_asn")]
    pub datacenter_asn: u32,

    /// NvLink partitioning configuration, used by the
    /// NvLink monitor to manage GPU mesh partitions
    /// via NMX-C.
    #[serde(default)]
    pub nvlink_config: Option<NvLinkConfig>,

    /// Power management settings: retry intervals after
    /// success/failure and host reboot wait time.
    #[serde(default = "default_power_options")]
    pub power_manager_options: PowerManagerOptions,

    /// Human-readable site name, exposed to customers
    /// running tenant OS via the FMDS endpoint.
    pub sitename: Option<String>,

    /// Auto machine repair plugin. When enabled,
    /// automatically transitions failed machines into
    /// repair workflows.
    #[serde(default)]
    pub auto_machine_repair_plugin: AutoMachineRepairPluginConfig,

    /// VMaaS (VM-as-a-Service) configuration for using
    /// NICo with a VM system, including VF settings and
    /// traffic-intercept bridging.
    pub vmaas_config: Option<VmaasConfig>,

    /// Named Mellanox NIC firmware configuration profiles,
    /// used by superNIC firmware flashing to apply
    /// device-specific register settings.
    #[serde(
        default,
        rename = "mlx-config-profiles",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_option_profile_map",
        serialize_with = "serialize_option_profile_map"
    )]
    pub mlxconfig_profiles: Option<HashMap<String, MlxConfigProfile>>,

    /// The intent of this config option is to use the NICo site controller as a standalone
    /// (disconnected / air-gapped) infrastructure manager for racks of GB200/GB300/VR144.
    /// Only set this if using NICo site controller with Rack Manager to manage GB200/300/VR144.
    /// It will change site controller behavior significantly in the following ways, etc.:
    /// 1. skip dpu management and use dpus in nic mode (set the site-wide `[site_explorer] dpu_mode = "nic_mode"`, or per-host `ExpectedMachine.dpu_mode`)
    ///    a. no dpu bfb upgrade and host power cycle
    ///    b. no firmware upgrade and host power cycle
    ///    c. no hbn deployment (no ecmp, etc)
    ///    d. no dpu agent deployment
    ///    e. no restricted mode configuration
    ///    f. no tenant overlay network via L2 vxlan/evpn or L3 vni (fnn)
    /// 2. support any other nic interface on the compute nodes including the onboard 3p nic
    /// 3. require expected machines table rows to have other/all mac addresses for each machine
    /// 4. restrict dhcp service to only provide ip address to known mac addresses
    ///    a. for additional mac addresses, use HostInband network segment when dpu is in nic mode
    /// 5. disable compute host individual firmware upgrades
    ///    a. only rack level firmware upgrades are allowed
    /// 6. enable nvlink switch and power shelf discovery and ingestion
    ///    a. site explorer changes to explore switch and power shelf bmc
    ///    b. state machine for ingestion workflow
    ///    c. nvlink switch nvos deployment/upgrade via onie
    ///    d. nvlink switch default configuration and machine validation
    /// 7. enable rack state machine and calls to rack manager
    ///    a. depend on rack manager for firmware upgrades of the rack
    ///    b. depend on rack manager for all power sequencing of the rack and components
    ///    c. override/suspend component level state machine state transitions as needed
    /// 8. enable nvlink control plane integration with nmx-c
    ///    a. export nmx-c apis via site controller
    ///    b. hardware health daemon polling of switch telemetry and collection into site controller
    ///    prometheus instance
    /// 9. enable domain power service integration
    #[serde(default)]
    pub rack_management_enabled: bool,

    /// Rack Manager Service configuration for rack-level firmware upgrades,
    /// power sequencing, and mTLS connectivity.
    #[serde(default)]
    pub rms: RmsConfig,

    /// rack_profiles contains the rack profile definitions. When expected racks
    /// are created, they are given a rack_profile_id to reference. This maps
    /// those names to the actual RackProfileConfig. This may eventually change,
    /// and/or co-exist with a DCIM providing us an entire config as part of
    /// the ingestion call.
    #[serde(default)]
    pub rack_profiles: model::rack_type::RackProfileConfig,

    /// SPDM (Security Protocol and Data Model) configuration for hardware attestation.
    #[serde(default)]
    pub spdm: SpdmConfig,

    /// Due to limitations in Cumulus Linux route-leaking,
    /// some sites may require all VRFs to use the same VNI.
    /// Isolation is still possible via ACLs, and route-imports
    /// will still use the dynamically allocated VNI for deriving
    /// route-targets.
    /// This will limit the number of VRFs supported on the
    /// DPU to a single VRF.
    pub site_global_vpc_vni: Option<u32>,

    /// DPF (DPU Platform Framework) configuration for DPU fabric deployment as a Kubernetes service.
    #[serde(default)]
    pub dpf: DpfConfig,

    /// The URL to use for overriding the PXE boot url on X86 machines.
    #[serde(default)]
    pub x86_pxe_boot_url_override: Option<String>,

    /// The URL to use for overriding the PXE boot url on ARM machines.
    #[serde(default)]
    pub arm_pxe_boot_url_override: Option<String>,

    /// Canonical PXE base URL
    #[serde(default = "default_pxe_public_base_url")]
    pub pxe_public_base_url: String,

    /// Vendors for which the state controller should pin the UEFI HTTP boot
    /// URL on the BMC (via Redfish `HttpBootUri`) in addition to the existing
    /// DHCP option 67 path. Machines whose BMC vendor is NOT in this list
    /// continue to rely on carbide-dhcp's option 67 for the URL.
    ///
    /// Empty by default — no machines get the BMC-pinned URL until vendors
    /// are explicitly added here (typically after per-vendor verification on
    /// real hardware). Adding a vendor that libredfish doesn't yet implement
    /// (e.g., `Dell` / `Lenovo` until their libredfish impls land) will
    /// surface a runtime `NotSupported` error; carbide-dhcp option 67 is the
    /// fallback URL source.
    #[serde(default)]
    pub set_http_boot_uri_for_vendors: Vec<BMCVendor>,

    /// Alternate API URL for external hosts that cannot resolve
    /// https://carbide-pxe.forge. This be an IP (e.g., "https://10.0.0.1:1079"),
    /// or an externally resolvable hostname (e.g.,
    /// "https://carbide-stack-api.corp.example.com"). This is the URL
    /// that gets handed back to interfaces assigned ot the static-assignments
    /// subnet. If not set, external hosts will just get the "internal"
    /// variant of api_url.
    #[serde(default)]
    pub external_api_url: Option<String>,

    /// Alternate PXE URL for external hosts (e.g., "http://10.0.0.1:8080"
    /// or "http://carbide-stack-pxe.corp.example.com"). Used for cloud-init and
    /// root CA retrieval for interfaces on the static-assignments segment,
    /// and follows the same rules as external_api_url above.
    #[serde(default)]
    pub external_pxe_url: Option<String>,

    /// Alternate static PXE URL for external hosts (e.g.,
    /// "http://10.0.0.1:8081" or "http://carbide-stack-static.corp.example.com").
    /// Used for kernel/blob downloads on the static-assignments segment.
    /// If not set, falls back to `external_pxe_url`.
    #[serde(default)]
    pub external_static_pxe_url: Option<String>,

    /// Controls enforcement of compute allocations when a new instance is
    /// requested.
    #[serde(default)]
    pub compute_allocation_enforcement: ComputeAllocationEnforcement,

    /// supernic_firmware_profiles is a nested map of FirmwareFlasherProfiles
    /// keyed by part_number and PSID. Each profile specifies the firmware to
    /// flash and optional lifecycle flags (reset, verify_image, verify_version).
    ///
    /// Configured in `nico-api-config.toml`:
    ///
    /// ```toml
    /// [supernic_firmware_profiles.900-9D3B4-00CV-TA0.MT_0000000884]
    /// part_number = "900-9D3B4-00CV-TA0"
    /// psid = "MT_0000000884"
    /// version = "32.43.1014"
    /// firmware_url = "https://firmware.example.com/fw-32.43.1014.bin"
    /// reset = true
    ///
    /// [supernic_firmware_profiles.900-9D3B4-00CV-TB0.MT_0000000885]
    /// part_number = "900-9D3B4-00CV-TB0"
    /// psid = "MT_0000000885"
    /// version = "32.43.1014"
    /// firmware_url = "ssh://firmwarehost/path/to/fw-32.43.1014.bin"
    /// ```
    #[serde(default)]
    pub supernic_firmware_profiles: HashMap<String, HashMap<String, FirmwareFlasherProfile>>,

    /// Component manager configuration for managing
    /// NvLink switches and power shelves via rack
    /// manager integration.
    #[serde(default)]
    pub component_manager: Option<component_manager::config::ComponentManagerConfig>,

    /// The password source to use for sites where the LEAF TOR
    /// requires session passwords.
    #[serde(default)]
    pub bgp_leaf_session_password: Option<BgpLeafSessionPassword>,

    /// The default routing-profile to use when a tenant is created.
    #[serde(default = "default_tenant_routing_profile")]
    pub default_tenant_routing_profile_type: String,

    /// The initial_objects.toml file for seeding the database
    #[serde(default)]
    pub initial_objects_file: Option<PathBuf>,

    /// The Figment that produced this config, when one was used. Kept after
    /// extraction so runtime code can attribute individual keys back to their
    /// source files via `Figment::find_metadata`
    ///
    /// `None` for `CarbideConfig` values that didn't come from `parse_carbide_config`
    /// (test fixtures, programmatic construction).
    #[serde(skip)]
    pub config_ctx: Option<Figment>,

    /// Whether to serve the admin web UI (the HTML pages under `/admin`).
    /// Defaults to `true`. Set to `false` to run the server with only the
    /// gRPC API and no admin UI -- the gRPC service is unaffected either way.
    #[serde(default = "default_to_true")]
    pub enable_admin_ui: bool,

    /// External tool links surfaced in the admin web UI's "Tools"
    /// sidebar. Each entry's `name` must be unique. The section is
    /// hidden when the list is empty.
    #[serde(default)]
    pub web_ui_sidebar_tools: Vec<ToolLink>,

    /// In-memory log history for the admin web live log viewer
    /// (`/admin/logs`): how much recent log data to keep for
    /// replay-on-connect and scrollback, and how many lines to send
    /// per page to the browser.
    #[serde(default)]
    pub log_history: LogHistoryConfig,

    #[serde(default)]
    pub tracing: TracingConfig,

    /// Secrets backend configuration. When present, the credential reader
    /// chain and write target are operator-configured (defaulting to the same
    /// env -> file -> vault behavior as when it is absent); see `SecretsConfig`.
    pub secrets: Option<SecretsConfig>,

    /// IP cleanup on lease expiry
    #[serde(default)]
    pub dhcp_lease_expiry_handling: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TracingConfig {
    /// Whether to enable OTLP tracing. Default: false
    #[serde(default)]
    pub enabled: bool,
    /// Whether to allow enabling/disabling tracing at runtime. Default: true
    #[serde(default = "default_to_true")]
    pub allow_runtime_changes: bool,
    /// Endpoint to send traces to. Can be overridden by the OTEL_EXPORTER_OTLP_TRACES_ENDPOINT env var.
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_runtime_changes: true,
            otlp_endpoint: None,
        }
    }
}

impl CarbideConfig {
    pub fn machine_state_handler_site_config(&self) -> MachineStateHandlerSiteConfig {
        MachineStateHandlerSiteConfig {
            pxe_public_base_url: self.pxe_public_base_url.clone(),
            firmware_global: self.firmware_global.clone(),
            machine_state_controller: self.machine_state_controller.clone(),
            host_health: self.host_health,

            selected_profile: self.selected_profile,
            bios_profiles: self.bios_profiles.clone(),
            oem_manager_profiles: self.oem_manager_profiles.clone(),

            dpa_enabled: self.is_dpa_enabled(),
            dpf_enabled: self.dpf.enabled,
            spdm_enabled: self.spdm.enabled,

            dpu_enable_secure_boot: self.dpu_config.dpu_enable_secure_boot,
        }
    }
}

/// Observability settings shared across all state controllers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ObservabilityConfig {
    /// Health alert classifications for which an additional per-object metric
    /// (`carbide_object_unhealthy_by_classification_count`) is emitted,
    /// labeled with the object's type and id (e.g. `object_type="machine"`,
    /// `object_id="<machine_id>"`).
    #[serde(default)]
    pub per_object_metrics_for_classifications: Vec<HealthAlertClassification>,
}

/// One external tool link rendered in the admin web UI's "Tools"
/// sidebar.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolLink {
    /// Stable identifier, must be unique within `tools`. Used
    /// to look up well-known integrations.
    pub name: String,
    /// Label rendered in the sidebar.
    pub display_name: String,
    /// Absolute URL the link points to.
    pub url: String,
}

/// In-memory log history for the admin web live log viewer
/// (`crate::web::logs`). Bounds memory use and the page size served
/// to the browser.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogHistoryConfig {
    /// Maximum amount of recent log history to retain in memory, in
    /// MiB. Oldest lines are evicted once the budget is exceeded.
    /// Default 128.
    #[serde(default = "default_log_history_max_megabytes")]
    pub max_megabytes: usize,

    /// Number of lines sent in the initial view and in each
    /// scrollback page. Default 500.
    #[serde(default = "default_log_history_page_size")]
    pub page_size: usize,
}

impl Default for LogHistoryConfig {
    fn default() -> Self {
        Self {
            max_megabytes: default_log_history_max_megabytes(),
            page_size: default_log_history_page_size(),
        }
    }
}

fn default_log_history_max_megabytes() -> usize {
    128
}

fn default_log_history_page_size() -> usize {
    500
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub enum BgpLeafSessionPassword {
    /// Use a defined site-wide password.
    /// The password should already exist in the credentials
    /// store.
    #[default]
    SiteWide,
}

/// Configures the Postgres secrets backend and how credentials flow. When
/// this section is present the reader chain and the write target come from
/// `backends` / `writer` below; their defaults keep today's behavior
/// (env -> file -> vault, writes to vault), so adding `[secrets]` does not
/// change credential routing on its own. Operators choose which backends to
/// read, in what order, and which one takes writes, by editing `backends`
/// and `writer`. Vault keeps serving PKI certificates regardless of the
/// chain.
///
/// Two prerequisites live outside this process and matter once writes move
/// to Postgres (`writer = "postgres"`) or vault leaves `backends`:
///
/// - Services that read credentials from vault through their own chains
///   (`bmc-proxy`, `dsx-exchange-consumer`) will not see anything carbide-api
///   writes to Postgres. They must be pointed at the same backend, or fed
///   another way, before the credentials they read change.
/// - During a rolling upgrade, replicas still on an older config keep writing
///   rotated credentials to their own writer. Keep autonomous credential
///   writers (site-explorer credential rotation) disabled until the whole
///   fleet runs a consistent config.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    /// KMS backend configuration.
    pub kms: KmsConfig,

    /// Maps path prefixes to the kek_id that encrypts new writes under
    /// them, longest prefix winning. A "/" catch-all entry is required.
    /// Reads never consult routing -- every stored row records the KEK
    /// that wrapped it -- so rotating a key means changing it here and
    /// running `carbide-admin-cli secrets re-wrap`.
    ///
    /// Example:
    /// ```toml
    /// [secrets.routing]
    /// "/" = "default-key"
    /// "machines/bmc" = "bmc-key"
    /// ```
    pub routing: std::collections::HashMap<String, String>,

    /// The credential *backend* read order, highest priority first (first match
    /// wins). The local-override readers (env, file) are always tried ahead of
    /// these, when their `[credentials.*]` section is enabled; this list only
    /// orders the backends behind them. Order is the operator's choice -- list
    /// the backends you want, in the priority you want. Defaults to `["vault"]`
    /// -- with the local overrides, that is the env -> file -> vault chain.
    ///
    /// For example, to roll Postgres in gradually, walk this list:
    ///
    /// 1. `["vault"]` -- Postgres configured but not yet read.
    /// 2. `["postgres", "vault"]` -- Postgres in front, vault as the safety net
    ///    for anything Postgres misses.
    /// 3. `["postgres"]` -- vault no longer read.
    ///
    /// An empty list, or a backend named twice, fails the boot.
    #[serde(default = "default_secret_backends")]
    pub backends: Vec<CredentialBackend>,

    /// Where new credential writes go. Defaults to `vault`; set to `postgres`
    /// to send new writes to the journal. Independent of `backends`: e.g.
    /// `writer = "postgres"` while `postgres` is not in `backends` (reads still
    /// served by vault) is a valid shadow-write -- it confirms writes land
    /// before reads start trusting Postgres -- and only logs a warning.
    #[serde(default)]
    pub writer: CredentialBackend,

    /// A source backend to import secrets from at startup. Unset means a
    /// fresh site with nothing to import; unsupported values fail config
    /// parsing rather than silently skipping the import. Independent of
    /// `backends`/`writer` -- importing from vault is orthogonal to where
    /// reads and writes flow.
    pub import_from: Option<ImportSource>,

    /// How to treat secrets that already exist in Postgres during import.
    /// Defaults to missing_only.
    #[serde(default)]
    pub import_approach: crate::secrets::ImportApproach,
}

/// A backend the one-time secrets import can read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    Vault,
}

/// A credential backend -- postgres or vault. Listed in `[secrets].backends` to
/// order the backends behind the always-first local overrides (env, file;
/// first match wins, see `ChainedCredentialReader`), and named by
/// `[secrets].writer` to choose where new writes go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CredentialBackend {
    /// The Postgres secrets journal.
    Postgres,
    /// Vault/OpenBao KV. The default write target (today's behavior).
    #[default]
    Vault,
}

/// The default backend order (just vault). With the always-first env/file
/// local overrides, this is the env -> file -> vault chain, so adding
/// `[secrets]` changes nothing until an operator edits it.
fn default_secret_backends() -> Vec<CredentialBackend> {
    vec![CredentialBackend::Vault]
}

/// Configures the KMS backends that wrap DEKs. Several named providers can
/// be defined: the active one wraps DEKs for new writes, and every provider
/// answers unwraps for the kek_ids it has.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KmsConfig {
    /// The provider that wraps DEKs for new writes.
    pub active: String,

    /// Named provider configurations.
    pub providers: std::collections::HashMap<String, ProviderConfig>,
}

/// One KMS provider. The `type` field in TOML selects the variant, and each
/// variant only accepts the fields that belong to it -- an integrated
/// provider cannot be given a transit key list, a misspelled field is a
/// parse error, and so on.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderConfig {
    /// Local key material, loaded from the environment or files. The
    /// default backend when no external KMS exists.
    Integrated {
        /// kek_id to key source. Key material itself never appears in
        /// this config -- only where to find it.
        keys: std::collections::HashMap<String, carbide_kms_provider::KeySource>,
    },
    /// Vault/OpenBao Transit, which wraps and unwraps DEKs server-side.
    /// Requires a static vault token in the credential config -- the
    /// Kubernetes service-account login flow is not supported for transit
    /// yet.
    Transit {
        /// The Transit key names this provider answers for.
        keys: Vec<String>,
        /// The Transit secrets engine mount path. Defaults to "transit".
        #[serde(default)]
        transit_mount: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ComputeAllocationEnforcement {
    #[default]
    /// If an allocation exists, don't enforce, but log what would have happened.
    WarnOnly,
    /// Only enforce if allocations exist.
    EnforceIfPresent,
    /// Always enforce, and zero allocations for the tenant means
    /// the new instance request will be rejected.
    Always,
}

/// DPF (DPU Platform Framework) configuration for
/// deploying DPU fabric as a Kubernetes service.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DpfConfig {
    /// Enables DPF deployment.
    #[serde(default)]
    pub enabled: bool,
    /// Kubernetes deployment name for the DPF service.
    #[serde(default = "default_dpf_deployment_name")]
    pub deployment_name: String,
    /// Kubernetes DPUFlavor CR name.
    #[serde(default = "default_dpf_flavor_name")]
    pub flavor_name: String,
    /// Label key applied to DPUNode CRs for deployment matching.
    #[serde(default = "default_dpf_node_label_key")]
    pub node_label_key: String,
    /// URL to the BlueField firmware bundle (BFB) for
    /// DPU provisioning.
    #[serde(default = "default_dpf_bfb_url")]
    pub bfb_url: String,
    /// Optional override for the Kubernetes `imagePullSecrets` entry used to pull the
    /// docker images of the mandatory services. When set, it is applied to every
    /// mandatory service except `dts` and `doca_hbn`. This also overrides if
    /// docker_image_pull_secret is set in services sections as well.
    #[serde(default)]
    pub docker_image_pull_secret: Option<String>,
    /// Mandatory Helm services to deploy alongside DPF.
    #[serde(default)]
    pub services: Box<DpfMandatoryServicesConfig>,
    /// Optional proxy configuration for the DPU. When set, containerd on the DPU is
    /// configured to route outbound HTTPS traffic through the specified proxy.
    #[serde(default)]
    pub proxy: Option<DpfProxyDetails>,
}

impl Default for DpfConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            deployment_name: default_dpf_deployment_name(),
            flavor_name: default_dpf_flavor_name(),
            node_label_key: default_dpf_node_label_key(),
            bfb_url: String::new(),
            docker_image_pull_secret: None,
            services: Box::default(),
            proxy: None,
        }
    }
}

impl DpfConfig {
    /// Returns the mandatory services with the optional [`Self::docker_image_pull_secret`]
    /// override applied. The override affects every mandatory service except `dts` and
    /// `doca_hbn`, which keep their own configured pull secret.
    pub fn resolved_mandatory_services(&self) -> DpfMandatoryServicesConfig {
        let mut services = (*self.services).clone();
        if let Some(secret) = &self.docker_image_pull_secret {
            services.dpu_agent.docker_image_pull_secret = secret.clone();
            services.dhcp_server.docker_image_pull_secret = secret.clone();
            services.fmds.docker_image_pull_secret = secret.clone();
            services.otel.docker_image_pull_secret = secret.clone();
        }
        services
    }
}

fn default_dpf_bfb_url() -> String {
    "https://content.mellanox.com/BlueField/BFBs/Ubuntu24.04/bf-bundle-3.4.1-12_26.04_ubuntu-24.04_64k_prod.bfb".to_string()
}

fn default_dpf_deployment_name() -> String {
    "nico-deployment-v2".to_string()
}

fn default_dpf_flavor_name() -> String {
    "carbide-dpu-flavor".to_string()
}

fn default_dpf_node_label_key() -> String {
    "carbide.nvidia.com/controlled.node.v2".to_string()
}

/// Configuration for a mandatory Helm-based DPF service.
/// Making it configurable means, a user can provide the link for his version of the service (for
/// testing/dev purpose).
/// There are following mandatory services:
/// dpu-agent, fmds, dhcp-server, doca-hbn, dts and otel.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DpfMandatoryServicesConfig {
    #[serde(default = "crate::dpf_services::default_dts_service")]
    pub dts: DpfServiceConfig,
    #[serde(default = "crate::dpf_services::default_doca_hbn_service")]
    pub doca_hbn: DpfServiceConfig,
    #[serde(default = "crate::dpf_services::default_dpu_agent_service")]
    pub dpu_agent: DpfServiceConfig,
    #[serde(default = "crate::dpf_services::default_dhcp_server_service")]
    pub dhcp_server: DpfServiceConfig,
    #[serde(default = "crate::dpf_services::default_fmds_service")]
    pub fmds: DpfServiceConfig,
    #[serde(default = "crate::dpf_services::default_otelcol_service")]
    pub otel: DpfServiceConfig,
}

impl Default for DpfMandatoryServicesConfig {
    fn default() -> Self {
        Self {
            dts: crate::dpf_services::default_dts_service(),
            doca_hbn: crate::dpf_services::default_doca_hbn_service(),
            dpu_agent: crate::dpf_services::default_dpu_agent_service(),
            dhcp_server: crate::dpf_services::default_dhcp_server_service(),
            fmds: crate::dpf_services::default_fmds_service(),
            otel: crate::dpf_services::default_otelcol_service(),
        }
    }
}

/// Default name for the Kubernetes `imagePullSecrets` entry used by DPF workload charts.
pub(crate) const DEFAULT_DPF_IMAGE_PULL_SECRET: &str = "dpf-pull-secret";

fn default_dpf_image_pull_secret() -> String {
    DEFAULT_DPF_IMAGE_PULL_SECRET.to_string()
}

/// Configuration for a single Helm-based DPF service.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DpfServiceConfig {
    /// Name of the Helm service.
    pub name: String,
    /// URL of the Helm chart repository.
    pub helm_repo_url: String,
    /// Name of the Helm chart.
    pub helm_chart: String,
    /// Version of the Helm chart.
    pub helm_version: String,
    /// Url for docker image
    pub docker_repo_url: String,
    /// Version of docker image
    pub docker_image_tag: String,
    /// Secret to use to pull the docker images.
    #[serde(default = "default_dpf_image_pull_secret")]
    pub docker_image_pull_secret: String,
}

/// Machine identity (SPIFFE JWT-SVID) configuration.
/// Loaded from `[machine_identity]` section in config.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineIdentityConfig {
    /// Master switch. If false, SetTenantIdentityConfiguration and SignMachineIdentity return 503.
    #[serde(default = "machine_identity_default_enabled")]
    pub enabled: bool,
    /// Signing algorithm for per-org keys (e.g. ES256).
    #[serde(default = "machine_identity_default_algorithm")]
    pub algorithm: SigningAlgorithm,
    /// Min token TTL permitted in seconds.
    #[serde(default = "machine_identity_default_token_ttl_min_sec")]
    pub token_ttl_min_sec: u32,
    /// Max token TTL permitted in seconds.
    #[serde(default = "machine_identity_default_token_ttl_max_sec")]
    pub token_ttl_max_sec: u32,
    /// Optional HTTP proxy for token endpoint calls (SSRF mitigation).
    #[serde(default)]
    pub token_endpoint_http_proxy: Option<String>,
    /// Key-id for encrypting new tenant identity ciphertext (selects from secrets `machine_identity.encryption_keys`).
    #[serde(default)]
    pub current_encryption_key_id: Option<String>,
    /// Trust domains allowed for tenant JWT `iss` (normalized host). Empty = allow any.
    /// Patterns: exact hostname, `*.suffix` (one label under suffix), `**.suffix` (suffix or any subdomain).
    #[serde(default)]
    pub trust_domain_allowlist: Vec<String>,
    /// Allowed DNS names for the `token_endpoint` URL host (`http://` / `https://` only). Empty = allow any.
    /// Same pattern syntax as [`Self::trust_domain_allowlist`].
    #[serde(default)]
    pub token_endpoint_domain_allowlist: Vec<String>,
    /// Upper bound for `signing_key_overlap_sec` on `SetTenantIdentityConfiguration` when `rotate_key` is true (seconds).
    #[serde(default = "machine_identity_default_signing_key_overlap_max_sec")]
    pub signing_key_overlap_max_sec: u32,
}

fn machine_identity_default_enabled() -> bool {
    false
}
fn machine_identity_default_algorithm() -> SigningAlgorithm {
    SigningAlgorithm::Es256
}
fn machine_identity_default_token_ttl_min_sec() -> u32 {
    60
}
fn machine_identity_default_token_ttl_max_sec() -> u32 {
    86400
}
fn machine_identity_default_signing_key_overlap_max_sec() -> u32 {
    604800
}

impl Default for MachineIdentityConfig {
    fn default() -> Self {
        Self {
            enabled: machine_identity_default_enabled(),
            algorithm: machine_identity_default_algorithm(),
            token_ttl_min_sec: machine_identity_default_token_ttl_min_sec(),
            token_ttl_max_sec: machine_identity_default_token_ttl_max_sec(),
            token_endpoint_http_proxy: None,
            current_encryption_key_id: None,
            trust_domain_allowlist: Vec::new(),
            token_endpoint_domain_allowlist: Vec::new(),
            signing_key_overlap_max_sec: machine_identity_default_signing_key_overlap_max_sec(),
        }
    }
}

impl From<MachineIdentityConfig> for model::tenant::IdentityConfigValidationBounds {
    fn from(mi: MachineIdentityConfig) -> Self {
        Self {
            token_ttl_min_sec: mi.token_ttl_min_sec,
            token_ttl_max_sec: mi.token_ttl_max_sec,
            algorithm: mi.algorithm,
            encryption_key_id: mi
                .current_encryption_key_id
                .expect(
                    "current_encryption_key_id is required when machine identity is enabled; \
                     startup validation in parse_carbide_config failed",
                )
                .try_into()
                .expect(
                    "current_encryption_key_id must be non-empty when machine identity is enabled",
                ),
            trust_domain_allowlist: mi.trust_domain_allowlist,
            signing_key_overlap_max_sec: mi.signing_key_overlap_max_sec,
        }
    }
}

impl From<MachineIdentityConfig> for model::tenant::TokenDelegationValidationBounds {
    fn from(mi: MachineIdentityConfig) -> Self {
        Self {
            token_endpoint_domain_allowlist: mi.token_endpoint_domain_allowlist,
        }
    }
}

/// SPDM (Security Protocol and Data Model) configuration
/// for hardware attestation of DPU components.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpdmConfig {
    /// Enables SPDM-based hardware attestation.
    #[serde(default)]
    pub enabled: bool,
    /// NRAS (Network Root of trust for Attestation
    /// Service) configuration for secure boot
    /// verification.
    #[serde(default)]
    pub nras_config: Option<nras::Config>,
}

/// A BGP route target used in FNN VRF import/export policies.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct RouteTargetConfig {
    /// Autonomous System Number component of the route target.
    #[serde(default)]
    pub asn: u32,
    /// Virtual Network Identifier component of the route target.
    #[serde(default)]
    pub vni: u32,
}

/// Fabric Nearest Neighbor (FNN) configuration for L3 VNI-based overlay networking.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FnnConfig {
    /// Optional FNN configuration for the admin network VPC.
    #[serde(default)]
    pub admin_vpc: Option<AdminFnnConfig>,

    /// We'll double-tag our internal tenant routes with this tag.
    /// Original consumer is a Network Infrastructure team, who will
    /// import a common route-target for internal tenant routes,
    /// reducing the coordination needed between NICo and the Network
    /// Infrastructure, but who knows what the future holds.
    #[serde(default)]
    pub common_internal_route_target: Option<RouteTargetConfig>,
    /// Additional route targets to import on DPU VRFs beyond the per-VPC defaults.
    #[serde(default)]
    pub additional_route_target_imports: Vec<RouteTargetConfig>,

    /// Named routing profiles that define per-VPC route target import/export policies.
    #[serde(default)]
    pub routing_profiles: HashMap<String, FnnRoutingProfileConfig>,

    /// Whether IPs should be allocated for VPC loopbacks.
    /// The VPC loopback pool will not be used if this false and
    /// no VPC/VRF loopback IP will be sent to the DPU.
    #[serde(default)]
    pub use_vpc_vrf_loopback: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
pub struct FnnRoutingProfileConfig {
    /// These are used for import policies to import routes
    /// that match these targets.
    #[serde(default)]
    pub route_target_imports: Vec<RouteTargetConfig>,

    /// These are used for tagging routes exported by the DPU
    #[serde(default)]
    pub route_targets_on_exports: Vec<RouteTargetConfig>,

    /// Is this an internal or external tenant/VPC profile
    #[serde(default)]
    pub internal: bool,

    /// Should DPUs leak the default route from the
    /// underlay into the tenant VRF?
    #[serde(default)]
    pub leak_default_route_from_underlay: bool,

    /// Should DPUs leak the routes for the host IPs into
    /// into the underlay?
    #[serde(default)]
    pub leak_tenant_host_routes_to_underlay: bool,

    /// Are route-leak communities sent by the host OS honored by the DPU for allowing
    /// routes advertised by the host OS to be leaked into the underlay?
    #[serde(default)]
    pub tenant_leak_communities_accepted: bool,

    /// An explicit/granular list of prefixes that should
    /// be allowed to leak from the default VRF into the tenant
    /// VRF.
    ///
    /// These are purely for routing purposes and will not have any
    /// impact on ACLs.
    #[serde(default)]
    pub accepted_leaks_from_underlay: Vec<PrefixFilterPolicyEntry>,

    /// Prefixes that tenant hosts are allowed to announce
    /// to the DPU as anycast routes.
    #[serde(default)]
    pub allowed_anycast_prefixes: Vec<PrefixFilterPolicyEntry>,

    /// Currently controls which profiles a tenant can use
    /// when creating VPCs.  Lower value means broader access.
    /// A tenant can create a VPC with a routing profile of the same or broader access.
    ///
    /// Example:
    /// - ADMIN is access tier 0.
    /// - INTERNAL is access tier 1.
    /// - A tenant with ADMIN could create ADMIN VPCs and INTERNAL VPCs.
    /// - A tenant with INTERNAL could only create INTERNAL VPCs.
    #[serde(default)]
    pub access_tier: u32,
}

impl From<&FnnRoutingProfileConfig> for rpc::forge::RoutingProfile {
    fn from(profile: &FnnRoutingProfileConfig) -> Self {
        Self {
            tenant_leak_communities_accepted: profile.tenant_leak_communities_accepted,
            leak_default_route_from_underlay: profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: profile.leak_tenant_host_routes_to_underlay,
            accepted_leaks_from_underlay: profile
                .accepted_leaks_from_underlay
                .iter()
                .map(|entry| rpc::forge::PrefixFilterPolicyEntry {
                    prefix: entry.prefix.to_string(),
                })
                .collect(),
            allowed_anycast_prefixes: profile
                .allowed_anycast_prefixes
                .iter()
                .map(|entry| rpc::forge::PrefixFilterPolicyEntry {
                    prefix: entry.prefix.to_string(),
                })
                .collect(),
            route_target_imports: profile
                .route_target_imports
                .iter()
                .map(|route_target| rpc::common::RouteTarget {
                    asn: route_target.asn,
                    vni: route_target.vni,
                })
                .collect(),
            route_targets_on_exports: profile
                .route_targets_on_exports
                .iter()
                .map(|route_target| rpc::common::RouteTarget {
                    asn: route_target.asn,
                    vni: route_target.vni,
                })
                .collect(),
        }
    }
}

/// Entries used for prefix-list policies on the DPUS.
/// Default behavior is max-len lte 32
/// We can change that with additional fields on this struct
/// if necessary in the future.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct PrefixFilterPolicyEntry {
    pub prefix: IpNetwork,
}

/// FNN configuration specific to the admin network.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct AdminFnnConfig {
    /// Whether FNN should be applied to the admin network as well.
    pub enabled: bool,

    /// VNI for the admin network VPC. When enabled, will create a VPC with this VNI
    /// and attach it to the admin network segment. Panics if a conflicting VPC/segment exists.
    #[serde(default)]
    pub vpc_vni: Option<u32>,

    /// The inline definition for the routing config to use for the admin network.
    #[serde(default)]
    pub routing_profile: FnnRoutingProfileConfig,
}

/// Validates a tool URL: it must parse and use the `http` or
/// `https` scheme. The `name` is included in the error for context.
fn validate_tool_url(name: &str, url: &str) -> eyre::Result<()> {
    let parsed = url::Url::parse(url)
        .map_err(|e| eyre::eyre!("tools entry {name:?}: invalid url {url:?}: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(eyre::eyre!(
            "tools entry {name:?}: url {url:?} must use http or https scheme"
        )),
    }?;

    Ok(())
}

impl CarbideConfig {
    /// Returns a version of CarbideConfig where secrets are erased
    pub fn redacted(&self) -> Self {
        let mut config = self.clone();
        if let Some(host_index) = config.database_url.find('@') {
            let host = config.database_url.split_at(host_index).1;
            config.database_url = format!("postgres://redacted{host}");
        }
        config
    }
    pub fn get_firmware_config(&self) -> FirmwareConfig {
        FirmwareConfig::new(
            self.firmware_global.firmware_directory.clone(),
            &self.host_models,
            &self.dpu_config.dpu_models,
        )
    }

    /// Returns an error when two `tools` entries share a `name`,
    /// since names are used as stable identifiers (e.g. `name = "grafana"`
    /// is referenced by the per-machine "Logs" deep link).
    /// Also rejects entries whose `url` is unparsable or doesn't use the `http` /
    /// `https` scheme.
    pub fn validate_web_ui_sidebar_tools(&self) -> eyre::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for tool in &self.web_ui_sidebar_tools {
            if !seen.insert(tool.name.as_str()) {
                return Err(eyre::eyre!(
                    "duplicate tools entry with name = {:?}; tool names must be unique",
                    tool.name
                ));
            }
            validate_tool_url(&tool.name, &tool.url)?;
        }
        Ok(())
    }

    /// validate_supernic_firmware_profiles checks that each profile's inner
    /// part_number and psid match the HashMap keys they are nested under.
    /// Logs a warning for any mismatches (the inner values are authoritative
    /// at runtime since they are what gets sent to scout).
    pub fn validate_supernic_firmware_profiles(&self) {
        for (key_pn, psid_map) in &self.supernic_firmware_profiles {
            for (key_psid, profile) in psid_map {
                if profile.firmware_spec.part_number != *key_pn {
                    tracing::warn!(
                        config_key_part_number = %key_pn,
                        profile_part_number = %profile.firmware_spec.part_number,
                        psid = %key_psid,
                        "firmware profile part_number does not match config key"
                    );
                }
                if profile.firmware_spec.psid != *key_psid {
                    tracing::warn!(
                        part_number = %key_pn,
                        config_key_psid = %key_psid,
                        profile_psid = %profile.firmware_spec.psid,
                        "firmware profile psid does not match config key"
                    );
                }
            }
        }
    }

    /// get_supernic_firmware_profile looks up the firmware profile for a
    /// device by its part number and PSID. Returns None if no matching entry
    /// exists.
    pub fn get_supernic_firmware_profile(
        &self,
        part_number: &str,
        psid: &str,
    ) -> Option<&libmlx::firmware::config::FirmwareFlasherProfile> {
        self.supernic_firmware_profiles.get(part_number)?.get(psid)
    }

    // get_mlxconfig_profile looks up an MlxConfigProfile by name from
    // the mlx-config-profiles config map. Returns None if the map is
    // not configured or the name is not found.
    pub fn get_mlxconfig_profile(
        &self,
        name: &str,
    ) -> Option<&libmlx::profile::profile::MlxConfigProfile> {
        self.mlxconfig_profiles.as_ref()?.get(name)
    }

    pub fn max_concurrent_machine_updates(&self) -> MaxConcurrentUpdates {
        MaxConcurrentUpdates {
            absolute: self.machine_updater.max_concurrent_machine_updates_absolute,
            percent: self.machine_updater.max_concurrent_machine_updates_percent,
        }
    }

    pub fn is_dpa_enabled(&self) -> bool {
        let Some(conf) = &self.dpa_config else {
            return false;
        };

        conf.enabled
    }

    pub fn get_dpa_subnet_ip(&self) -> Result<Ipv4Addr, eyre::Report> {
        let Some(conf) = &self.dpa_config else {
            tracing::error!("get_dpa_subnet_ip: DPA config missing");
            return Err(eyre::eyre!("get_dpa_subnet_ip: DPA config missing"));
        };

        Ok(conf.subnet_ip)
    }

    pub fn get_dpa_subnet_mask(&self) -> Result<i32, eyre::Report> {
        let Some(conf) = &self.dpa_config else {
            tracing::error!("get_dpa_subnet_mask: DPA config missing");
            return Err(eyre::eyre!("get_dpa_subnet_mask: DPA config missing"));
        };

        Ok(conf.subnet_mask)
    }

    pub fn mqtt_broker_host(&self) -> Option<String> {
        self.dpa_config
            .as_ref()
            .map(|conf| conf.mqtt_endpoint.clone())
    }

    pub fn mqtt_broker_port(&self) -> Option<u16> {
        self.dpa_config.as_ref().map(|conf| conf.mqtt_broker_port)
    }

    pub fn get_hb_interval(&self) -> Option<chrono::TimeDelta> {
        self.dpa_config.as_ref().map(|conf| conf.hb_interval)
    }

    /// Returns true if the DSX Exchange Event Bus is enabled.
    pub fn is_dsx_exchange_event_bus_enabled(&self) -> bool {
        self.dsx_exchange_event_bus
            .as_ref()
            .map(|conf| conf.enabled)
            .unwrap_or(false)
    }

    /// Returns the DSX Exchange Event Bus MQTT broker endpoint if enabled.
    pub fn dsx_exchange_event_bus_mqtt_endpoint(&self) -> Option<&str> {
        self.dsx_exchange_event_bus
            .as_ref()
            .filter(|conf| conf.enabled)
            .map(|conf| conf.mqtt_endpoint.as_str())
    }

    /// Returns the DSX Exchange Event Bus MQTT broker port if enabled.
    pub fn dsx_exchange_event_bus_mqtt_broker_port(&self) -> Option<u16> {
        self.dsx_exchange_event_bus
            .as_ref()
            .filter(|conf| conf.enabled)
            .map(|conf| conf.mqtt_broker_port)
    }

    /// Returns preingestion manager config.
    pub fn preingestion_manager(&self) -> PreingestionManagerConfig {
        PreingestionManagerConfig {
            run_interval: self
                .firmware_global
                .run_interval
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(30)),
            concurrency_limit: self.firmware_global.concurrency_limit,
            hgx_bmc_gpu_reboot_delay: self
                .firmware_global
                .hgx_bmc_gpu_reboot_delay
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(30)),
            max_concurrent_bfb_copies: self.firmware_global.max_concurrent_bfb_copies,
            autoupdate: self.firmware_global.autoupdate,
            no_reset_retries: self.firmware_global.no_reset_retries,
            firmware_download_cache_directory: self
                .firmware_global
                .firmware_download_cache_directory
                .clone(),
            firmware: self.get_firmware_config(),
        }
    }
}

pub struct MaxConcurrentUpdates {
    absolute: Option<i32>,
    percent: Option<i32>,
}

impl MaxConcurrentUpdates {
    pub fn max_concurrent_updates(&self, unhealthy: i32, out_of: i32) -> Option<i32> {
        if self.percent.is_none() {
            self.absolute
        } else {
            let percent = self.percent?;
            if out_of <= 0 || percent <= 0 {
                return Some(0);
            }
            let percent = percent as usize;
            // Round up, so if someone specified 10% with 9 hosts they'll get 1.
            let mut count = (percent * out_of as usize).div_ceil(100);
            count = count.saturating_sub(unhealthy as usize);
            if let Some(absolute) = self.absolute {
                count = count.min(absolute as usize);
            }
            Some(count as i32)
        }
    }
}

/// NetworkSegmentStateController related config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NetworkSegmentStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
    /// The time for which network segments must have 0 allocated IPs, before they
    /// are actually released.
    /// This should be set to a duration long enough that ensures no pending
    /// RPC calls might still use the network segment to avoid race conditions.
    #[serde(
        default = "NetworkSegmentStateControllerConfig::network_segment_drain_time_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub network_segment_drain_time: chrono::Duration,
}

impl NetworkSegmentStateControllerConfig {
    pub fn network_segment_drain_time_default() -> Duration {
        Duration::minutes(5)
    }
}

impl Default for NetworkSegmentStateControllerConfig {
    fn default() -> Self {
        Self {
            controller: StateControllerConfig::default(),
            network_segment_drain_time: Self::network_segment_drain_time_default(),
        }
    }
}

/// VpcPrefixStateController related config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VpcPrefixStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
    /// The time for which VPC prefixes must have 0 referencing network prefixes,
    /// before they are actually released.
    /// This should be set to a duration long enough that ensures no pending
    /// RPC calls might still use the VPC prefix to avoid race conditions.
    #[serde(
        default = "VpcPrefixStateControllerConfig::vpc_prefix_drain_time_default",
        deserialize_with = "deserialize_duration_chrono",
        serialize_with = "as_duration"
    )]
    pub vpc_prefix_drain_time: chrono::Duration,
}

impl VpcPrefixStateControllerConfig {
    /// Returns the default VPC prefix drain time.
    pub fn vpc_prefix_drain_time_default() -> Duration {
        // Match the network segment drain default for hierarchical cleanup.
        Duration::minutes(5)
    }
}

impl Default for VpcPrefixStateControllerConfig {
    /// Builds the default VPC prefix state controller configuration.
    fn default() -> Self {
        // Use framework defaults plus the VPC prefix drain grace period.
        Self {
            controller: StateControllerConfig::default(),
            vpc_prefix_drain_time: Self::vpc_prefix_drain_time_default(),
        }
    }
}

/// IbPartitionStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct IbPartitionStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// DpaInterfaceStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DpaInterfaceStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// PowerShelfStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PowerShelfStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// RackStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RackStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// SwitchStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SwitchStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// SpdmStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SpdmStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InitialObjectsConfig {
    /// Resource pools that allocate IPs, VNIs, etc.
    /// Required, but wrapped in `Option` so partial configs
    /// can be deserialized and merged.
    pub pools: Option<HashMap<String, ResourcePoolDef>>,
    /// Network Segment definitions
    pub networks: Option<HashMap<String, NetworkDefinition>>,
    /// VPC definitions
    pub vpcs: Option<HashMap<String, VpcDefinition>>,
}

/// TLS certificate and key configuration for securing
/// gRPC and HTTP connections.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TlsConfig {
    /// Path to the root CA certificate file for
    /// validating client certificates.
    #[serde(default)]
    pub root_cafile_path: String,

    /// Path to the server identity certificate PEM
    /// file.
    #[serde(default)]
    pub identity_pemfile_path: String,

    /// Path to the server identity private key file.
    #[serde(default)]
    pub identity_keyfile_path: String,

    /// Path to the admin root CA certificate file for
    /// admin client validation.
    #[serde(default)]
    pub admin_root_cafile_path: String,
}

/// The transport protocol mode for the gRPC API server.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenMode {
    /// Plaintext HTTP/1.1 (no TLS).
    PlaintextHttp1,
    /// Plaintext HTTP/2 (no TLS).
    PlaintextHttp2,
    /// TLS-encrypted connections (default).
    #[serde(other)]
    #[default]
    Tls,
}

/// Authentication related configuration
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AuthConfig {
    /// Enable permissive mode in the authorization enforcer (for development).
    pub permissive_mode: bool,

    /// The Casbin policy file (in CSV format).
    pub casbin_policy_file: Option<PathBuf>,

    /// Additional nico-admin-cli certs allowed.  This does not include actually allowing the cert to connect, just that certs that can be verified which match these criteria can do GRPC requests.
    pub cli_certs: Option<AllowedCertCriteria>,

    /// Configuration for the root of trust for client cert auth
    pub trust: Option<TrustConfig>,
}

fn default_listen() -> SocketAddr {
    "[::]:1079".parse().unwrap()
}

fn default_max_database_connections() -> u32 {
    1000
}

pub const fn default_bmc_session_lockout_threshold() -> u32 {
    3
}

/// DpuConfig related internal configuration
#[derive(Clone, Debug, Serialize)]
pub struct DpuConfig {
    /// Enable dpu firmware updates on initial discovery
    #[serde(default)]
    pub dpu_nic_firmware_initial_update_enabled: bool,

    /// Enable dpu firmware updates on known machines
    #[serde(default)]
    pub dpu_nic_firmware_reprovision_update_enabled: bool,

    /// DPU related configuration parameter
    #[serde(default)]
    pub dpu_models: HashMap<String, Firmware>,

    #[serde(default)]
    pub dpu_nic_firmware_update_versions: Vec<String>,

    /// Whether to enable secure boot flow for DPU provisioning (via redfish)
    /// Default is false.
    #[serde(default)]
    pub dpu_enable_secure_boot: bool,

    /// Number of virtual functions configured per DPU PF during BlueField provisioning.
    /// Defaults to 16 and must not exceed 126.
    #[serde(default)]
    pub num_of_vfs: u32,
}

impl DpuConfig {
    pub fn find_bf3_entry(&self) -> Option<&FirmwareEntry> {
        self.dpu_models.get("bluefield3").and_then(|f| {
            f.components
                .get(&FirmwareComponentType::Bmc)
                .and_then(|fc| fc.known_firmware.first())
        })
    }
    pub fn find_bf2_entry(&self) -> Option<&FirmwareEntry> {
        self.dpu_models.get("bluefield2").and_then(|f| {
            f.components
                .get(&FirmwareComponentType::Bmc)
                .and_then(|fc| fc.known_firmware.first())
        })
    }
}

impl<'de> Deserialize<'de> for DpuConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Create a temporary struct for partial deserialization
        #[derive(Deserialize)]
        struct PartialDpuConfig {
            #[serde(default)]
            dpu_nic_firmware_initial_update_enabled: Option<bool>,
            #[serde(default)]
            dpu_nic_firmware_reprovision_update_enabled: Option<bool>,
            #[serde(default)]
            dpu_models: Option<HashMap<String, Firmware>>,
            #[serde(default)]
            dpu_nic_firmware_update_versions: Option<Vec<String>>,
            #[serde(default)]
            dpu_enable_secure_boot: Option<bool>,
            #[serde(default)]
            num_of_vfs: Option<u32>,
        }

        let partial = PartialDpuConfig::deserialize(deserializer)?;
        let default = DpuConfig::default();
        let num_of_vfs = partial.num_of_vfs.unwrap_or(default.num_of_vfs);
        if num_of_vfs > MAX_DPU_NUM_OF_VFS {
            return Err(serde::de::Error::custom(format!(
                "dpu_config.num_of_vfs must be <= {MAX_DPU_NUM_OF_VFS}"
            )));
        }

        Ok(DpuConfig {
            dpu_nic_firmware_initial_update_enabled: partial
                .dpu_nic_firmware_initial_update_enabled
                .unwrap_or(default.dpu_nic_firmware_initial_update_enabled),
            dpu_nic_firmware_reprovision_update_enabled: partial
                .dpu_nic_firmware_reprovision_update_enabled
                .unwrap_or(default.dpu_nic_firmware_reprovision_update_enabled),
            dpu_models: partial.dpu_models.unwrap_or(default.dpu_models),
            dpu_nic_firmware_update_versions: partial
                .dpu_nic_firmware_update_versions
                .unwrap_or(default.dpu_nic_firmware_update_versions),
            dpu_enable_secure_boot: partial
                .dpu_enable_secure_boot
                .unwrap_or(default.dpu_enable_secure_boot),
            num_of_vfs,
        })
    }
}

impl Default for DpuConfig {
    // Preingestion is only enabled for BF3 BMC Firmware upgrades. This is to support ingesting DPUs that come
    // with older BMC firmware versions than BF-23.10-5. BF-23.10-5 is the minimum BMC firmware that Site Explorer
    // can support auto-ingestion for.
    fn default() -> Self {
        Self {
            dpu_nic_firmware_initial_update_enabled: false,
            dpu_nic_firmware_reprovision_update_enabled: true,
            dpu_models: HashMap::from([
                (
                    "bluefield2".to_string(),
                    Firmware {
                        vendor: BMCVendor::Nvidia,
                        model: "Bluefield 2 SmartNIC Main Card".to_string(),
                        ordering: vec![FirmwareComponentType::Bmc, FirmwareComponentType::Cec],
                        explicit_start_needed: false,
                        components: HashMap::from([
                            (
                                FirmwareComponentType::Bmc,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("BMC_Firmware").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF2_BMC_VERSION)],
                                },
                            ),
                            (
                                FirmwareComponentType::Cec,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("Bluefield_FW_ERoT").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF2_CEC_VERSION)],
                                },
                            ),
                            (
                                FirmwareComponentType::Nic,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("DPU_NIC").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF2_NIC_VERSION)],
                                },
                            ),
                            (
                                FirmwareComponentType::Uefi,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("DPU_UEFI").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF2_UEFI_VERSION)],
                                },
                            ),
                        ]),
                    },
                ),
                (
                    "bluefield3".to_string(),
                    Firmware {
                        vendor: BMCVendor::Nvidia,
                        model: "Bluefield 3 SmartNIC Main Card".to_string(),
                        ordering: vec![FirmwareComponentType::Bmc, FirmwareComponentType::Cec],
                        explicit_start_needed: false,
                        components: HashMap::from([
                            (
                                FirmwareComponentType::Bmc,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("BMC_Firmware").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![
                                        // BF-24.10-33 (DOCA 2.9) is the expected BMC FW that we expect on BF3s after ingesting them
                                        FirmwareEntry::standard(BF3_BMC_VERSION),
                                    ],
                                },
                            ),
                            (
                                FirmwareComponentType::Cec,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("Bluefield_FW_ERoT").unwrap(),
                                    ),

                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF3_CEC_VERSION)],
                                },
                            ),
                            (
                                FirmwareComponentType::Nic,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("DPU_NIC").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF3_NIC_VERSION)],
                                },
                            ),
                            (
                                FirmwareComponentType::Uefi,
                                FirmwareComponent {
                                    current_version_reported_as: Some(
                                        Regex::new("DPU_UEFI").unwrap(),
                                    ),
                                    preingest_upgrade_when_below: None,
                                    known_firmware: vec![FirmwareEntry::standard(BF3_UEFI_VERSION)],
                                },
                            ),
                        ]),
                    },
                ),
            ]),
            dpu_nic_firmware_update_versions: vec![
                BF2_NIC_VERSION.to_string(),
                BF3_NIC_VERSION.to_string(),
            ],
            dpu_enable_secure_boot: false,
            num_of_vfs: DEFAULT_DPU_NUM_OF_VFS,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NetworkSecurityGroupConfig {
    /// The maximum number of unique rules allowed for
    /// a network security group after rules are expanded.
    /// (src port range * dst port range * src prefix list * dst prefix list)
    #[serde(default = "default_max_network_security_group_size")]
    pub max_network_security_group_size: u32,
    /// Whether to allow stateful security groups.
    /// This will initially only be passed through to the
    /// DPU as a way to toggle default stateful options
    /// in nvue config.
    #[serde(default = "default_to_true")]
    pub stateful_acls_enabled: bool,

    /// A set of NSG rules that will be inserted before any user-defined rules.
    #[serde(default)]
    pub policy_overrides: Vec<NetworkSecurityGroupRule>,
}

impl Default for NetworkSecurityGroupConfig {
    fn default() -> Self {
        NetworkSecurityGroupConfig {
            max_network_security_group_size: default_max_network_security_group_size(),
            stateful_acls_enabled: default_to_true(),
            policy_overrides: vec![],
        }
    }
}

/// Configuration for rolling machine updates and
/// maintenance windows.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct MachineUpdater {
    /// Time window during which machines may automatically
    /// reboot for updates.
    #[serde(default)]
    pub instance_autoreboot_period: Option<TimePeriod>,
    /// The maximum number of machines that have in-progress updates running.  This prevents
    /// too many machines from being put into maintenance at any given time.
    pub max_concurrent_machine_updates_absolute: Option<i32>,
    /// The maximum percentage of machines that have in-progress updates running.  This prevents
    /// too many machines from being put into maintenance at any given time.  If both values are given, the lesser will be used.
    pub max_concurrent_machine_updates_percent: Option<i32>,
}

pub fn default_max_find_by_ids() -> u32 {
    100
}

pub fn default_max_network_security_group_size() -> u32 {
    200
}

pub fn default_pxe_public_base_url() -> String {
    "http://carbide-pxe.forge:8080".to_string()
}

pub fn default_internet_l3_vni() -> u32 {
    // This is a number agreed upon between the Network
    // Infrastructure team and NICo that they will use to
    // tag the default route.
    //
    // It will be combined with datacenter_asn to form
    // a route-target of <DC_ASN>:<INTERNET_VNI>.
    100001
}

pub fn default_datacenter_asn() -> u32 {
    // This is a number previously provided by the Network
    // Infrastructure team.
    //
    // It represents a "global" (i.e., non-DC-specific)
    // identifier.  It's used in pre-FNN sites and in FNN
    // on DPU routes, but we'll transition away from that.
    11414
}

pub fn default_to_true() -> bool {
    true
}

fn default_tenant_routing_profile() -> String {
    "EXTERNAL".to_string()
}

/// Configuration for the measured boot metrics collector,
/// which exports TPM-based boot measurement data as
/// Prometheus metrics.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct MeasuredBootMetricsCollectorConfig {
    /// Enables the measured boot metrics monitor. When
    /// disabled, measured boot metrics are not exported.
    #[serde(default)]
    pub enabled: bool,
    /// Interval at which the monitor polls for the latest
    /// measured boot data.
    /// Default is 60 seconds.
    #[serde(
        default = "MeasuredBootMetricsCollectorConfig::default_run_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub run_interval: std::time::Duration,
}

impl Default for MeasuredBootMetricsCollectorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            run_interval: Self::default_run_interval(),
        }
    }
}

impl MeasuredBootMetricsCollectorConfig {
    const fn default_run_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }
}

/// The VPC isolation behavior enforced within a site.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VpcIsolationBehaviorType {
    #[default]
    /// VPCs will be isolated from each other.
    MutualIsolation,

    /// Open, no isolation.
    Open,
}

impl VpcIsolationBehaviorType {
    fn as_printable(&self) -> &'static str {
        use VpcIsolationBehaviorType::*;
        match self {
            MutualIsolation => "MutualIsolation",
            Open => "Open",
        }
    }
}

impl std::fmt::Display for VpcIsolationBehaviorType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_printable())
    }
}

impl From<VpcIsolationBehaviorType> for rpc::forge::VpcIsolationBehaviorType {
    fn from(b: VpcIsolationBehaviorType) -> Self {
        match b {
            VpcIsolationBehaviorType::Open => {
                rpc::forge::VpcIsolationBehaviorType::VpcIsolationOpen
            }
            VpcIsolationBehaviorType::MutualIsolation => {
                rpc::forge::VpcIsolationBehaviorType::VpcIsolationMutual
            }
        }
    }
}

#[allow(deprecated)] // nvue_enabled proto field is deprecated but still set for backwards compat
impl From<CarbideConfig> for rpc::forge::RuntimeConfig {
    fn from(value: CarbideConfig) -> Self {
        Self {
            listen: value.listen.to_string(),
            metrics_endpoint: value
                .metrics_endpoint
                .map(|x| x.to_string())
                .unwrap_or("NA".to_string()),
            database_url: value.database_url,
            max_database_connections: value.max_database_connections,
            enable_ip_fabric: value.ib_config.unwrap_or_default().enabled,
            asn: value.asn,
            dhcp_servers: value
                .dhcp_servers
                .into_iter()
                .map(|addr| addr.to_string())
                .collect(),
            route_servers: value
                .route_servers
                .into_iter()
                .map(|addr| addr.to_string())
                .collect(),
            enable_route_servers: value.enable_route_servers,
            deny_prefixes: value
                .deny_prefixes
                .into_iter()
                .map(|x| x.to_string())
                .collect(),
            site_fabric_prefixes: value
                .site_fabric_prefixes
                .into_iter()
                .map(|x| x.to_string())
                .collect(),
            vpc_isolation_behavior: value.vpc_isolation_behavior.to_string(),
            networks: value
                .networks
                .unwrap_or_default()
                .keys()
                .cloned()
                .collect_vec(),
            dpu_ipmi_tool_impl: value.dpu_ipmi_tool_impl.unwrap_or("Not Set".to_string()),
            dpu_ipmi_reboot_attempt: value.dpu_ipmi_reboot_attempts.unwrap_or_default(),
            initial_domain_name: value.initial_domain_name,
            sitename: value.sitename,
            initial_dpu_agent_upgrade_policy: value
                .initial_dpu_agent_upgrade_policy
                .unwrap_or(AgentUpgradePolicyChoice::Off)
                .to_string(),
            dpu_nic_firmware_update_version: HashMap::default(),
            dpu_nic_firmware_initial_update_enabled: DpuConfig::default()
                .dpu_nic_firmware_initial_update_enabled,
            dpu_nic_firmware_reprovision_update_enabled: DpuConfig::default()
                .dpu_nic_firmware_reprovision_update_enabled,
            max_concurrent_machine_updates: value
                .machine_updater
                .max_concurrent_machine_updates_absolute
                .unwrap_or_default(),
            machine_update_runtime_interval: value.machine_update_run_interval.unwrap_or_default(),
            nvue_enabled: true,
            attestation_enabled: value.attestation_enabled,
            auto_host_firmware_update: value.firmware_global.autoupdate,
            host_enable_autoupdate: value.firmware_global.host_enable_autoupdate,
            host_disable_autoupdate: value.firmware_global.host_disable_autoupdate,
            max_find_by_ids: value.max_find_by_ids,
            dpu_network_pinger_type: value.dpu_network_monitor_pinger_type,
            machine_validation_enabled: value.machine_validation_config.enabled,
            rack_validation_enabled: value.rack_validation_config.enabled,
            bom_validation_enabled: value.bom_validation.enabled,
            bom_validation_ignore_unassigned_machines: value
                .bom_validation
                .ignore_unassigned_machines,
            bom_validation_allow_allocation_on_validation_failure: value
                .bom_validation
                .allow_allocation_on_validation_failure,
            dpu_nic_firmware_update_versions: value.dpu_config.dpu_nic_firmware_update_versions,
            dpa_enabled: value.dpa_config.clone().unwrap_or_default().enabled,
            mqtt_endpoint: value.dpa_config.clone().unwrap_or_default().mqtt_endpoint,
            mqtt_broker_port: value
                .dpa_config
                .clone()
                .unwrap_or_default()
                .mqtt_broker_port as i32,
            mqtt_hb_interval: value
                .dpa_config
                .clone()
                .unwrap_or_default()
                .hb_interval
                .to_string(),
            bom_validation_auto_generate_missing_sku: value
                .bom_validation
                .auto_generate_missing_sku,
            bom_validation_auto_generate_missing_sku_interval: value
                .bom_validation
                .auto_generate_missing_sku_interval
                .as_secs(),
            dpu_secure_boot_enabled: value.dpu_config.dpu_enable_secure_boot,
            dpa_subnet_ip: value
                .dpa_config
                .clone()
                .unwrap_or_default()
                .subnet_ip
                .to_string(),
            dpa_subnet_mask: value.dpa_config.unwrap_or_default().subnet_mask,
            dpf_enabled: value.dpf.enabled,
            compile_time_helm_version: crate::dpf_services::COMPILE_TIME_HELM_VERSION.to_string(),
            compile_time_docker_version: crate::dpf_services::COMPILE_TIME_IMAGE_TAG.to_string(),
        }
    }
}

fn default_mqtt_endpoint() -> String {
    "mqtt.forge".to_string()
}

fn default_mqtt_broker_port() -> u16 {
    1884
}

pub use carbide_dpa_manager::config::{DpaConfig, MqttAuthConfig, MqttAuthMode};
use model::vpc::VpcDefinition;

/// DSX Exchange Event Bus configuration for publishing state change events via MQTT 3.1.1.
///
/// When configured, Carbide will publish `ManagedHostState` transitions to
/// `{topic_prefix}/{machineId}/state` (default `NICO/v1/machine`), publish BMS
/// rack leak/isolation values and heartbeat timestamps to metadata-defined DSX
/// topics, and subscribe to `BMS/v1/PUB/Metadata/#` to learn those routing
/// targets.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DsxExchangeEventBusConfig {
    /// Enable/disable the DSX Exchange Event Bus.
    #[serde(default)]
    pub enabled: bool,

    /// MQTT broker host (name or IP address) used to create client connections.
    #[serde(default = "default_mqtt_endpoint")]
    pub mqtt_endpoint: String,

    /// MQTT broker port to use to establish client connections.
    #[serde(default = "default_mqtt_broker_port")]
    pub mqtt_broker_port: u16,

    /// Timeout for MQTT publish operations. Defaults to 1 second.
    #[serde(
        default = "DsxExchangeEventBusConfig::default_publish_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub publish_timeout: std::time::Duration,

    /// Queue capacity for buffering DSX publish events while publishing.
    /// Events are dropped if the queue is full. Defaults to 1024.
    #[serde(default = "DsxExchangeEventBusConfig::default_queue_capacity")]
    pub queue_capacity: usize,

    /// Topic prefix used when publishing `ManagedHostState` transitions.
    /// The full topic is `{topic_prefix}/{machineId}/state`. Defaults to
    /// `NICO/v1/machine`. NATS subjects are case-sensitive, so this must
    /// match the producer pub allow configured on the broker.
    #[serde(default = "DsxExchangeEventBusConfig::default_topic_prefix")]
    pub topic_prefix: String,

    #[serde(default)]
    pub auth: MqttAuthConfig,
}

impl DsxExchangeEventBusConfig {
    pub const fn default_publish_timeout() -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }

    pub const fn default_queue_capacity() -> usize {
        1024
    }

    pub fn default_topic_prefix() -> String {
        "NICO/v1/machine".to_string()
    }
}

/// Auto machine repair plugin related configuration
#[derive(Default, Clone, Copy, Debug, Deserialize, Serialize)]
pub struct AutoMachineRepairPluginConfig {
    /// Whether automatic machine repair mode is enabled
    #[serde(default)]
    pub enabled: bool,
}

/// Defines the policy for VPC peering based on network virtualization type.
#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VpcPeeringPolicy {
    /// Only VPCs with the same network virtualization type can peer.
    Exclusive,

    /// VPCs with any network virtualization type can peer with each other.
    Mixed,

    /// VPC peering is not allowed.
    None,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct VmaasConfig {
    /// Allow VFs on instance creation.  defaults to true, but will be disabled when
    /// using SDN to manage the instance network configuration for VMs
    #[serde(default = "default_to_true")]
    pub allow_instance_vf: bool,

    /// Configure the DPUs to create the reps specified.
    /// when not provided, the DPU creates the reps for the 2 physical devices and 14 virtual devices
    pub hbn_reps: Option<String>,

    /// Configure the DPUs to create the SF representors specified.
    pub hbn_sfs: Option<String>,

    /// Options to configure advanced routing and bridging.
    pub bridging: Option<TrafficInterceptBridging>,

    /// Prefixes expected to be publicly routable and used
    /// by traffic-intercept users.
    pub public_prefixes: Vec<Ipv4Network>,

    /// Aggregate prefixes associated with secondary VTEPs. These are used only
    /// for routing and filtering; IP allocation is provided by the secondary
    /// VTEP resource pool.
    #[serde(default)]
    pub secondary_vtep_aggregate_prefixes: Vec<IpNetwork>,

    /// Whether a secondary overlay is expected,
    /// which will require secondary VTEP IPs to be allocated
    /// to DPUs
    #[serde(default = "default_to_true")]
    pub secondary_overlay_support: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct TrafficInterceptBridging {
    /// Prefix to be used for internal routing between HBN and intercept bridges
    /// within the DPU.
    pub internal_bridge_routing_prefix: Ipv4Network,

    /// The HBN/SFC bridge that intercept patch ports attach to during provisioning.
    #[serde(default = "default_hbn_bridge")]
    pub hbn_bridge: String,

    /// The name of the bridge that sits between VFs and br-hbn _**for VM-owned VFs**_.
    /// This bridge will be assigned an address from <internal_bridge_routing_prefix>
    /// so that we can route traffic to a /32 bound to it and used as a VTEP for
    /// an additional GENEVE VPN.
    #[serde(default = "default_vf_intercept_bridge_name")]
    pub vf_intercept_bridge_name: String,

    /// The <vf_intercept_bridge_name> side of the SF representor that connects the HBN pod to br-hbn.
    /// This will be the side owned by the <vf_intercept_bridge_name> bridge _**for VM-owned VFs**_
    #[serde(default = "default_vf_intercept_bridge_port")]
    pub vf_intercept_bridge_port: String,

    /// The SF used for internal routing of VF traffic.
    pub vf_intercept_bridge_sf: String,

    /// The layout of host-owned representors that will have intermediary bridges.
    /// E.g., [{"pf0hpf" => {bridge: "br-host", patch_port: "brh"}}]
    #[serde(default)]
    pub host_representor_intercept_bridging: HashMap<String, HostInterceptBridging>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct HostInterceptBridging {
    /// The name of the bridge (e.g., br-host) that will sit between host PF/VF and br-hbn.
    /// It will be connected to br-hbn or br-sfc.
    pub bridge: String,

    /// The patch port on this bridge that connects it toward HBN or SFC.
    pub patch_port: String,

    /// Control whether this bridging should be created during DPU (re)provisioning or not.
    /// By default, we expect to create these bridges.
    #[serde(default)]
    pub skip_create: bool,
}

impl TrafficInterceptBridging {
    /// Formats host-owned representor bridge config for BlueField provisioning.
    pub fn host_representor_intercept_bridging_provisioning_config(&self) -> Option<String> {
        // Keep bf.cfg input stable and omit entries that should not be provisioned.
        let config = self
            .host_representor_intercept_bridging
            .iter()
            .filter(|(_, bridge)| !bridge.skip_create)
            .sorted_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(representor, bridge)| {
                format!("{representor}:{}:{}", bridge.bridge, bridge.patch_port)
            })
            .join(",");

        // An empty map, or one with only skipped entries, means no provisioning config.
        (!config.is_empty()).then_some(config)
    }
}

pub fn default_hbn_bridge() -> String {
    "br-hbn".to_string()
}

pub fn default_vf_intercept_bridge_name() -> String {
    "br-dpu".to_string()
}

pub fn default_vf_intercept_bridge_port() -> String {
    "patch-br-dpu-to-hbn".to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering as AtomicOrdering;

    use carbide_authn::config::CertComponent;
    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_site_explorer::config::SiteExplorerExploreMode;
    use chrono::Datelike;
    use figment::Figment;
    use figment::providers::{Env, Format, Toml};
    use health_report::HealthAlertClassification;
    use libmlx::variables::value::MlxValueType;
    use libredfish::model::service_root::RedfishVendor;
    use model::expected_machine::DpuMode;
    use model::network_segment::NetworkDefinitionSegmentType;
    use model::resource_pool;

    use super::*;
    use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;

    const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cfg/test_data");

    #[test]
    fn deserialize_serialize_machine_controller_config() {
        let input = MachineStateControllerConfig {
            controller: StateControllerConfig {
                iteration_time: std::time::Duration::from_secs(30),
                max_object_handling_time: std::time::Duration::from_secs(60),
                max_concurrency: 10,
                processor_dispatch_interval: std::time::Duration::from_secs(2),
                processor_log_interval: std::time::Duration::from_secs(60),
                metric_emission_interval: std::time::Duration::from_secs(60),
                metric_hold_time: std::time::Duration::from_secs(5 * 60),
            },
            dpu_wait_time: Duration::minutes(20),
            power_down_wait: Duration::seconds(10),
            failure_retry_time: Duration::minutes(90),
            dpu_up_threshold: Duration::weeks(1),
            scout_reporting_timeout: Duration::minutes(5),
            uefi_boot_wait: Duration::minutes(5),
            max_bios_config_retries: 3,
            polling_bios_setup_stuck_threshold: Duration::minutes(15),
        };

        let config_str = serde_json::to_string(&input).unwrap();
        let config: MachineStateControllerConfig = serde_json::from_str(&config_str).unwrap();

        assert_eq!(config, input);
    }

    #[test]
    fn deserialize_serialize_machine_controller_config_default() {
        let input = MachineStateControllerConfig::default();
        let config_str = serde_json::to_string(&input).unwrap();
        let config: MachineStateControllerConfig = serde_json::from_str(&config_str).unwrap();
        assert_eq!(config, input);
    }

    #[test]
    fn deserialize_machine_controller_config() {
        let config = r#"{"dpu_wait_time": "20m","power_down_wait":"10s",
        "failure_retry_time":"1h30m", "dpu_up_threshold": "1w",
        "controller": {"iteration_time": "33s", "max_object_handling_time": "63s", "max_concurrency": 13}}"#;
        let config: MachineStateControllerConfig = serde_json::from_str(config).unwrap();

        assert_eq!(
            config,
            MachineStateControllerConfig {
                controller: {
                    StateControllerConfig {
                        iteration_time: std::time::Duration::from_secs(33),
                        max_object_handling_time: std::time::Duration::from_secs(63),
                        max_concurrency: 13,
                        processor_dispatch_interval: std::time::Duration::from_secs(2),
                        processor_log_interval: std::time::Duration::from_secs(60),
                        metric_emission_interval: std::time::Duration::from_secs(60),
                        metric_hold_time: std::time::Duration::from_secs(5 * 60),
                    }
                },
                dpu_wait_time: Duration::minutes(20),
                power_down_wait: Duration::seconds(10),
                failure_retry_time: Duration::minutes(90),
                dpu_up_threshold: Duration::weeks(1),
                scout_reporting_timeout: Duration::minutes(5),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
            }
        );
    }

    #[test]
    fn deserialize_machine_controller_config_with_default() {
        let config =
            r#"{"power_down_wait":"10s", "failure_retry_time":"1h30m", "dpu_up_threshold": "1w"}"#;
        let config: MachineStateControllerConfig = serde_json::from_str(config).unwrap();

        assert_eq!(
            config,
            MachineStateControllerConfig {
                controller: StateControllerConfig::default(),
                dpu_wait_time: Duration::minutes(5),
                power_down_wait: Duration::seconds(10),
                failure_retry_time: Duration::minutes(90),
                dpu_up_threshold: Duration::weeks(1),
                scout_reporting_timeout: Duration::minutes(5),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
            }
        );
    }

    #[test]
    fn deserialize_network_segment_state_controller_config() {
        let config = r#"{"network_segment_drain_time": "21m",
        "controller": {"iteration_time": "33s", "max_object_handling_time": "63s", "max_concurrency": 13}}"#;
        let config: NetworkSegmentStateControllerConfig = serde_json::from_str(config).unwrap();

        assert_eq!(
            config,
            NetworkSegmentStateControllerConfig {
                controller: {
                    StateControllerConfig {
                        iteration_time: std::time::Duration::from_secs(33),
                        max_object_handling_time: std::time::Duration::from_secs(63),
                        max_concurrency: 13,
                        processor_dispatch_interval: std::time::Duration::from_secs(2),
                        processor_log_interval: std::time::Duration::from_secs(60),
                        metric_emission_interval: std::time::Duration::from_secs(60),
                        metric_hold_time: std::time::Duration::from_secs(5 * 60),
                    }
                },
                network_segment_drain_time: Duration::minutes(21),
            }
        );
    }

    #[test]
    fn deserialize_network_segment_state_controller_config_with_default() {
        let config = r#"{}"#;
        let config: NetworkSegmentStateControllerConfig = serde_json::from_str(config).unwrap();

        assert_eq!(config, NetworkSegmentStateControllerConfig::default());
    }

    #[test]
    fn serialize_empty_state_controller_config() {
        let input = StateControllerConfig::default();
        let config_str = serde_json::to_string(&input).unwrap();
        assert_eq!(
            config_str,
            r#"{"iteration_time":"30s","max_object_handling_time":"180s","max_concurrency":10,"processor_dispatch_interval":"2s","processor_log_interval":"60s","metric_emission_interval":"60s","metric_hold_time":"300s"}"#
        );
        let config: StateControllerConfig = serde_json::from_str(&config_str).unwrap();
        assert_eq!(config, input);
    }

    #[test]
    fn validate_tool_url_accepts_https() {
        validate_tool_url("grafana", "https://grafana.example.com").unwrap();
    }

    #[test]
    fn validate_tool_url_accepts_http_domain() {
        validate_tool_url("grafana", "http://grafana.example.com").unwrap();
    }

    #[test]
    fn validate_tool_url_accepts_http_ip() {
        validate_tool_url("grafana", "http://10.213.1.115").unwrap();
    }

    #[test]
    fn validate_tool_url_rejects_javascript_scheme() {
        let err = validate_tool_url("evil", "javascript:alert(1)")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("must use http or https"),
            "unexpected error: {err}"
        );
    }

    /// Ensures `validate_web_ui_sidebar_tools` actually delegates per-entry
    /// URL validation: a URL that fails `validate_tool_url` must also cause
    /// `validate_web_ui_sidebar_tools` to fail.
    #[test]
    fn validate_web_ui_sidebar_tools_propagates_url_failure() {
        const BAD_URL: &str = "javascript:alert(1)";

        // Sanity-check the precondition: the helper rejects this URL.
        assert!(validate_tool_url("evil", BAD_URL).is_err());

        let mut config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();
        config.web_ui_sidebar_tools = vec![ToolLink {
            name: "evil".to_string(),
            display_name: "Evil".to_string(),
            url: BAD_URL.to_string(),
        }];
        assert!(config.validate_web_ui_sidebar_tools().is_err());
    }

    #[test]
    fn serialize_configured_state_controller_config() {
        let input = StateControllerConfig {
            iteration_time: std::time::Duration::from_secs(11),
            max_object_handling_time: std::time::Duration::from_secs(22),
            max_concurrency: 33,
            processor_dispatch_interval: std::time::Duration::from_secs(2),
            processor_log_interval: std::time::Duration::from_secs(60),
            metric_emission_interval: std::time::Duration::from_secs(60),
            metric_hold_time: std::time::Duration::from_secs(5 * 60),
        };
        let config_str = serde_json::to_string(&input).unwrap();
        assert_eq!(
            config_str,
            r#"{"iteration_time":"11s","max_object_handling_time":"22s","max_concurrency":33,"processor_dispatch_interval":"2s","processor_log_interval":"60s","metric_emission_interval":"60s","metric_hold_time":"300s"}"#
        );
        let config: StateControllerConfig = serde_json::from_str(&config_str).unwrap();
        assert_eq!(config, input);
    }

    #[test]
    fn test_redact_config() {
        let mut config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();
        let redacted = config.redacted();
        assert_eq!(
            redacted.database_url,
            "postgres://redacted@postgresql".to_string()
        );
        config.database_url = "postgres://forge-system.carbide:very-very-long-password@forge-pg-cluster.postgres.svc.cluster.local:5432/forge_system_carbide".to_string();
        let redacted = config.redacted();
        assert_eq!(redacted.database_url, "postgres://redacted@forge-pg-cluster.postgres.svc.cluster.local:5432/forge_system_carbide".to_string());
    }

    #[test]
    fn deserialize_min_config() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();
        assert_eq!(config.listen, "[::]:1081".parse().unwrap());
        assert_eq!(config.metrics_endpoint, None);
        assert_eq!(config.asn, 123);
        assert_eq!(config.database_url, "postgres://a:b@postgresql".to_string());
        assert_eq!(
            config.max_database_connections,
            default_max_database_connections()
        );
        assert!(config.dhcp_servers.is_empty());
        assert!(config.route_servers.is_empty());
        assert!(config.tls.is_none());
        assert!(config.auth.is_none());
        assert!(config.pools.is_none());
        assert!(config.ib_config.is_none());
        assert!(config.ib_fabrics.is_empty());
        assert_eq!(
            config.bmc_session_lockout_threshold,
            default_bmc_session_lockout_threshold()
        );
        assert!(
            !config.allow_bmc_basic_auth_fallback,
            "allow_bmc_basic_auth_fallback must default to false to preserve \
             the session-token-only contract for existing deployments"
        );
        assert!(config.vpc_peering_policy.is_none());
        assert!(config.site_explorer.enabled.load(AtomicOrdering::Relaxed));
        // `enable_admin_ui` is unset in the minimal config, so it should default to true.
        assert!(config.enable_admin_ui);
        assert!(config.initial_objects_file.is_none());
        assert!(
            config
                .site_explorer
                .create_machines
                .load(AtomicOrdering::Relaxed)
        );
        assert_eq!(
            config.machine_state_controller,
            MachineStateControllerConfig::default()
        );
        assert_eq!(
            config.network_segment_state_controller,
            NetworkSegmentStateControllerConfig::default()
        );
        assert_eq!(
            config.vpc_prefix_state_controller,
            VpcPrefixStateControllerConfig::default()
        );
        assert_eq!(
            config.ib_partition_state_controller,
            IbPartitionStateControllerConfig::default()
        );
        assert_eq!(config.max_find_by_ids, default_max_find_by_ids());
        assert_eq!(config.dpu_network_monitor_pinger_type, None);
        assert_eq!(config.measured_boot_collector, {
            MeasuredBootMetricsCollectorConfig {
                enabled: false,
                run_interval: MeasuredBootMetricsCollectorConfig::default_run_interval(),
            }
        });
        // And make sure lack of [mlx-config-profiles] doesn't blow up
        // for sites not configured with any.
        assert!(config.mlxconfig_profiles.is_none());
    }

    #[test]
    fn deserialize_patched_min_config() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::file(format!("{TEST_DATA_DIR}/site_config.toml")))
            .extract()
            .unwrap();
        assert_eq!(config.listen, "[::]:1081".parse().unwrap());
        assert_eq!(config.metrics_endpoint, None);
        assert_eq!(config.database_url, "postgres://a:b@postgresql".to_string());
        assert_eq!(config.max_database_connections, 1333);
        assert_eq!(config.asn, 777);
        assert_eq!(config.dhcp_servers, vec![Ipv4Addr::new(99, 101, 102, 103)]);
        assert!(config.route_servers.is_empty());
        assert_eq!(config.bmc_session_lockout_threshold, 5);
        assert_eq!(config.vpc_peering_policy, Some(VpcPeeringPolicy::Exclusive));
        assert_eq!(config.vpc_peering_policy_on_existing, None);
        assert_eq!(
            config.tls.as_ref().unwrap().identity_pemfile_path,
            "/patched/path/to/cert"
        );
        assert_eq!(
            config.tls.as_ref().unwrap().identity_keyfile_path,
            "/patched/path/to/key"
        );
        assert_eq!(
            config.tls.as_ref().unwrap().root_cafile_path,
            "/patched/path/to/ca"
        );
        assert!(config.auth.as_ref().unwrap().permissive_mode);
        assert_eq!(
            config
                .auth
                .as_ref()
                .unwrap()
                .casbin_policy_file
                .as_ref()
                .unwrap()
                .as_os_str(),
            "/patched/path/to/policy"
        );
        let pools = config.pools.as_ref().unwrap();
        assert_eq!(
            pools.get("lo-ip").unwrap(),
            &ResourcePoolDef {
                ranges: Vec::new(),
                prefix: Some("10.180.63.0/26".to_string()),
                pool_type: resource_pool::ResourcePoolType::Ipv4,
                delegate_prefix_len: None,
            }
        );
        assert!(pools.get("pkey").is_none());
        assert_eq!(
            config.ib_config,
            Some(IBFabricConfig {
                enabled: true,
                fabric_monitor_run_interval: std::time::Duration::from_secs(102),
                ..serde_json::from_str("{}").unwrap()
            })
        );
        assert_eq!(
            config.site_explorer,
            SiteExplorerConfig {
                retained_boot_interface_window: None,
                enabled: Arc::new(false.into()),
                run_interval: std::time::Duration::from_secs(120),
                concurrent_explorations: 10,
                explorations_per_run: 12,
                create_machines: Arc::new(false.into()),
                machines_created_per_run: 1,
                override_target_ip: None,
                override_target_port: None,
                bmc_proxy: carbide_site_explorer::config::bmc_proxy(None),
                allow_changing_bmc_proxy: None,
                reset_rate_limit: Duration::hours(1),
                admin_segment_type_non_dpu: Arc::new(false.into()),
                allocate_secondary_vtep_ip: false,
                create_power_shelves: Arc::new(true.into()),
                explore_power_shelves_from_static_ip: Arc::new(true.into()),
                power_shelves_created_per_run: 1,
                create_switches: Arc::new(true.into()),
                switches_created_per_run: 9,
                rotate_switch_nvos_credentials: Arc::new(false.into()),
                dpu_mode: None,
                explore_mode: SiteExplorerExploreMode::LibRedfish,
                explore_drive_firmware: false,
            }
        );
        assert_eq!(
            config.machine_state_controller,
            MachineStateControllerConfig {
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(3 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(11),
                    max_concurrency: 22,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
                dpu_wait_time: Duration::minutes(7),
                power_down_wait: Duration::seconds(17),
                failure_retry_time: Duration::minutes(70),
                dpu_up_threshold: Duration::minutes(77),
                scout_reporting_timeout: Duration::minutes(5),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
            }
        );
        assert_eq!(
            config.network_segment_state_controller,
            NetworkSegmentStateControllerConfig {
                network_segment_drain_time: Duration::seconds(45),
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(18 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(188),
                    max_concurrency: 1888,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.vpc_prefix_state_controller,
            VpcPrefixStateControllerConfig {
                vpc_prefix_drain_time: Duration::seconds(46),
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(19 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(199),
                    max_concurrency: 1999,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.ib_partition_state_controller,
            IbPartitionStateControllerConfig {
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(17 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(177),
                    max_concurrency: 1777,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(config.max_find_by_ids, 50);
        assert_eq!(
            config.dpu_network_monitor_pinger_type,
            Some("OobNetBind".to_string())
        );
    }

    #[test]
    fn deserialize_full_config() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .extract()
            .unwrap();
        assert_eq!(config.listen, "[::]:1081".parse().unwrap());
        assert_eq!(config.metrics_endpoint, Some("[::]:1080".parse().unwrap()));
        assert_eq!(config.database_url, "postgres://a:b@postgresql".to_string());
        assert_eq!(config.max_database_connections, 1222);
        assert_eq!(config.asn, 123);
        assert_eq!(config.bmc_session_lockout_threshold, 4);
        assert_eq!(
            config.dhcp_servers,
            vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)]
        );
        assert_eq!(
            config.ntp_servers,
            vec![Ipv4Addr::new(10, 20, 30, 40), Ipv4Addr::new(50, 60, 70, 80)]
        );
        assert_eq!(config.vpc_peering_policy, Some(VpcPeeringPolicy::Exclusive));
        assert_eq!(
            config.vpc_peering_policy_on_existing,
            Some(VpcPeeringPolicy::Mixed)
        );
        assert_eq!(config.pxe_public_base_url, "http://pxe.example.com:8080");
        assert_eq!(config.route_servers, vec![Ipv4Addr::new(9, 10, 11, 12)]);
        assert_eq!(
            config.tls.as_ref().unwrap().identity_pemfile_path,
            "/path/to/cert"
        );
        assert_eq!(
            config.tls.as_ref().unwrap().identity_keyfile_path,
            "/path/to/key"
        );
        assert_eq!(config.tls.as_ref().unwrap().root_cafile_path, "/path/to/ca");
        assert!(!config.auth.as_ref().unwrap().permissive_mode);
        assert_eq!(config.dpu_config.num_of_vfs, DEFAULT_DPU_NUM_OF_VFS);
        assert_eq!(
            config
                .auth
                .as_ref()
                .unwrap()
                .casbin_policy_file
                .clone()
                .unwrap()
                .as_os_str(),
            "/path/to/policy"
        );
        let pools = config.pools.as_ref().unwrap();
        assert_eq!(
            pools.get("lo-ip").unwrap(),
            &ResourcePoolDef {
                ranges: Vec::new(),
                prefix: Some("10.180.62.1/26".to_string()),
                pool_type: resource_pool::ResourcePoolType::Ipv4,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            pools.get("vlan-id").unwrap(),
            &ResourcePoolDef {
                ranges: vec![resource_pool::Range {
                    auto_assign: true,
                    start: "100".to_string(),
                    end: "501".to_string()
                }],
                prefix: None,
                pool_type: resource_pool::ResourcePoolType::Integer,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            config.ib_fabrics,
            [(
                "default".to_string(),
                IbFabricDefinition {
                    endpoints: vec!["https://1.2.3.4".to_string()],
                    pkeys: vec![resource_pool::Range {
                        auto_assign: true,
                        start: "1".to_string(),
                        end: "10".to_string()
                    }]
                }
            )]
            .into_iter()
            .collect()
        );

        assert_eq!(
            config.ib_config,
            Some(IBFabricConfig {
                enabled: false,
                fabric_monitor_run_interval: std::time::Duration::from_secs(101),
                ..serde_json::from_str("{}").unwrap()
            })
        );
        assert_eq!(
            config.site_explorer,
            SiteExplorerConfig {
                retained_boot_interface_window: None,
                enabled: Arc::new(true.into()),
                run_interval: std::time::Duration::from_secs(100),
                concurrent_explorations: 30,
                explorations_per_run: 11,
                create_machines: Arc::new(true.into()),
                machines_created_per_run: 2,
                override_target_ip: Some("1.2.3.4".to_owned()),
                override_target_port: Some(10443),
                bmc_proxy: carbide_site_explorer::config::bmc_proxy(None),
                allow_changing_bmc_proxy: None,
                reset_rate_limit: Duration::hours(2),
                admin_segment_type_non_dpu: Arc::new(false.into()),
                allocate_secondary_vtep_ip: false,
                create_power_shelves: Arc::new(true.into()),
                explore_power_shelves_from_static_ip: Arc::new(true.into()),
                power_shelves_created_per_run: 1,
                create_switches: Arc::new(true.into()),
                switches_created_per_run: 9,
                rotate_switch_nvos_credentials: Arc::new(false.into()),
                dpu_mode: None,
                explore_mode: SiteExplorerExploreMode::LibRedfish,
                explore_drive_firmware: false,
            }
        );

        assert_eq!(
            config.host_health,
            HostHealthConfig {
                hardware_health_reports: model::machine::HardwareHealthReportsConfig::Disabled,
                dpu_agent_version_staleness_threshold: Duration::days(1),
                prevent_allocations_on_stale_dpu_agent_version: true,
                prevent_allocations_on_scout_heartbeat_timeout: true,
                suppress_external_alerting_on_scout_heartbeat_timeout: false,
            }
        );
        assert_eq!(
            config.observability,
            ObservabilityConfig {
                per_object_metrics_for_classifications: vec![
                    HealthAlertClassification::hardware(),
                    HealthAlertClassification::prevent_allocations(),
                ],
            }
        );
        assert_eq!(
            config.machine_state_controller,
            MachineStateControllerConfig {
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(9 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(99),
                    max_concurrency: 999,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
                dpu_wait_time: Duration::minutes(3),
                power_down_wait: Duration::seconds(13),
                failure_retry_time: Duration::minutes(31),
                dpu_up_threshold: Duration::minutes(33),
                scout_reporting_timeout: Duration::minutes(20),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
            }
        );
        assert_eq!(
            config.network_segment_state_controller,
            NetworkSegmentStateControllerConfig {
                network_segment_drain_time: Duration::seconds(44),
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(8 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(88),
                    max_concurrency: 888,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.vpc_prefix_state_controller,
            VpcPrefixStateControllerConfig {
                vpc_prefix_drain_time: Duration::seconds(43),
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(6 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(66),
                    max_concurrency: 666,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.ib_partition_state_controller,
            IbPartitionStateControllerConfig {
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(7 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(77),
                    max_concurrency: 777,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(config.dpu_config.dpu_models.len(), 2);
        for (_, entry) in config.dpu_config.dpu_models.iter() {
            assert_eq!(entry.vendor, bmc_vendor::BMCVendor::Nvidia);
        }
        assert_eq!(config.host_models.len(), 2);
        for (_, entry) in config.host_models.iter() {
            assert_eq!(entry.vendor, bmc_vendor::BMCVendor::Dell);
        }
        assert_eq!(config.firmware_global.max_uploads, 3);
        assert_eq!(config.firmware_global.run_interval, Duration::seconds(20));
        assert_eq!(config.firmware_global.max_concurrent_bfb_copies, 7);
        assert_eq!(config.max_find_by_ids, 75);
        assert_eq!(config.dpu_network_monitor_pinger_type, None);
        assert_eq!(
            config.measured_boot_collector,
            MeasuredBootMetricsCollectorConfig {
                enabled: false,
                run_interval: std::time::Duration::from_secs(555),
            }
        );
        assert_eq!(
            config.auth.clone().unwrap().cli_certs.unwrap().group_from,
            Some(CertComponent::SubjectOU)
        );
        assert_eq!(
            config
                .auth
                .clone()
                .unwrap()
                .cli_certs
                .unwrap()
                .username_from,
            Some(CertComponent::SubjectCN)
        );
        assert_eq!(
            config
                .auth
                .clone()
                .unwrap()
                .cli_certs
                .unwrap()
                .required_equals
                .len(),
            2
        );
        assert_eq!(
            config
                .auth
                .clone()
                .unwrap()
                .cli_certs
                .unwrap()
                .required_equals
                .get(&CertComponent::IssuerO),
            Some("NVIDIA Corporation".to_string()).as_ref()
        );
        assert_eq!(
            config
                .auth
                .clone()
                .unwrap()
                .cli_certs
                .unwrap()
                .required_equals
                .get(&CertComponent::IssuerCN),
            Some("NVIDIA Forge Root Certificate Authority 2022".to_string()).as_ref()
        );
        assert_eq!(
            config
                .machine_updater
                .instance_autoreboot_period
                .clone()
                .unwrap()
                .start
                .day(),
            7
        );
        assert_eq!(
            config
                .machine_updater
                .instance_autoreboot_period
                .clone()
                .unwrap()
                .end
                .day(),
            8
        );
        // Do some more in-depth validation of the MlxConfigProfile section, ensuring
        // we're able to deserialize the SerializedProfile into an MlxConfigProfile
        // and validate entries were properly deserialized back to their types + values.
        //
        // First verify that both serialized profiles are detected.
        assert_eq!(config.mlxconfig_profiles.clone().unwrap().len(), 2);
        // And then pluck out one of them and validate everything deserialized
        // as expected. All of this is generally handled by existing unit tests
        // within the mlxconfig_profile tests already, but it doesn't hurt to
        // verify stuff here also.
        let mlxconfig_profile = config
            .mlxconfig_profiles
            .as_ref()
            .unwrap()
            .get("test-profile")
            .unwrap();
        assert_eq!(mlxconfig_profile.name, "test-profile");
        assert_eq!(mlxconfig_profile.registry.name, "mlx_generic");
        assert_eq!(mlxconfig_profile.config_values.len(), 2);
        assert_eq!(
            mlxconfig_profile.get_variable("SRIOV_EN").unwrap().value,
            MlxValueType::Boolean(true)
        );
        assert_eq!(
            mlxconfig_profile.get_variable("NUM_OF_VFS").unwrap().value,
            MlxValueType::Integer(4)
        );
        assert!(mlxconfig_profile.get_variable("NONEXISTENT_GOO").is_none());

        assert_eq!(config.rack_profiles.rack_profiles.len(), 2);
        let nvl72 = config.rack_profiles.get("NVL72").unwrap();
        assert_eq!(
            nvl72.product_family,
            Some(model::rack_type::RackProductFamily::Gb200)
        );
        assert_eq!(nvl72.rack_capabilities.compute.count, 18);
        assert_eq!(
            nvl72.rack_capabilities.compute.name.as_deref(),
            Some("GB200")
        );
        assert_eq!(
            nvl72.rack_capabilities.compute.vendor.as_deref(),
            Some("NVIDIA")
        );
        assert_eq!(nvl72.rack_capabilities.switch.count, 9);
        assert_eq!(nvl72.rack_capabilities.power_shelf.count, 8);
        let nvl36 = config.rack_profiles.get("NVL36").unwrap();
        assert_eq!(
            nvl36.product_family,
            Some(model::rack_type::RackProductFamily::Gb200)
        );
        assert_eq!(nvl36.rack_capabilities.compute.count, 9);
        assert_eq!(nvl36.rack_capabilities.switch.count, 9);
        assert_eq!(nvl36.rack_capabilities.power_shelf.count, 2);
    }

    #[test]
    fn deserialize_patched_full_config() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .merge(Toml::file(format!("{TEST_DATA_DIR}/site_config.toml")))
            .extract()
            .unwrap();
        assert_eq!(config.listen, "[::]:1081".parse().unwrap());
        assert_eq!(config.metrics_endpoint, Some("[::]:1080".parse().unwrap()));
        assert_eq!(config.database_url, "postgres://a:b@postgresql".to_string());
        assert_eq!(config.max_database_connections, 1333);
        assert_eq!(config.asn, 777);
        assert_eq!(config.bmc_session_lockout_threshold, 5);
        assert_eq!(config.dhcp_servers, vec![Ipv4Addr::new(99, 101, 102, 103)]);
        assert_eq!(config.route_servers, vec![Ipv4Addr::new(9, 10, 11, 12)]);
        assert_eq!(
            config.tls.as_ref().unwrap().identity_pemfile_path,
            "/patched/path/to/cert"
        );
        assert_eq!(
            config.tls.as_ref().unwrap().identity_keyfile_path,
            "/patched/path/to/key"
        );
        assert_eq!(
            config.tls.as_ref().unwrap().root_cafile_path,
            "/patched/path/to/ca"
        );
        assert!(config.auth.as_ref().unwrap().permissive_mode);
        assert_eq!(
            config
                .auth
                .as_ref()
                .unwrap()
                .casbin_policy_file
                .clone()
                .unwrap()
                .as_os_str(),
            "/patched/path/to/policy"
        );
        let pools = config.pools.as_ref().unwrap();
        assert_eq!(
            pools.get("lo-ip").unwrap(),
            &ResourcePoolDef {
                ranges: Vec::new(),
                prefix: Some("10.180.63.0/26".to_string()),
                pool_type: resource_pool::ResourcePoolType::Ipv4,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            pools.get("vlan-id").unwrap(),
            &ResourcePoolDef {
                ranges: vec![resource_pool::Range {
                    auto_assign: true,

                    start: "100".to_string(),
                    end: "501".to_string()
                }],
                prefix: None,
                pool_type: resource_pool::ResourcePoolType::Integer,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            config.ib_fabrics,
            [(
                "default".to_string(),
                IbFabricDefinition {
                    endpoints: vec!["https://1.2.3.4".to_string()],
                    pkeys: vec![resource_pool::Range {
                        auto_assign: true,

                        start: "1".to_string(),
                        end: "10".to_string()
                    }]
                }
            )]
            .into_iter()
            .collect()
        );
        assert_eq!(
            config.ib_config,
            Some(IBFabricConfig {
                enabled: true,
                fabric_monitor_run_interval: std::time::Duration::from_secs(102),
                ..serde_json::from_str("{}").unwrap()
            })
        );
        assert_eq!(
            config.site_explorer,
            SiteExplorerConfig {
                retained_boot_interface_window: None,
                enabled: Arc::new(false.into()),
                run_interval: std::time::Duration::from_secs(100),
                concurrent_explorations: 10,
                explorations_per_run: 12,
                create_machines: Arc::new(false.into()),
                machines_created_per_run: 2,
                override_target_ip: Some("1.2.3.4".to_owned()),
                override_target_port: Some(10443),
                bmc_proxy: carbide_site_explorer::config::bmc_proxy(None),
                allow_changing_bmc_proxy: None,
                reset_rate_limit: Duration::hours(2),
                admin_segment_type_non_dpu: Arc::new(false.into()),
                allocate_secondary_vtep_ip: false,
                create_power_shelves: Arc::new(true.into()),
                explore_power_shelves_from_static_ip: Arc::new(true.into()),
                power_shelves_created_per_run: 1,
                create_switches: Arc::new(true.into()),
                switches_created_per_run: 9,
                rotate_switch_nvos_credentials: Arc::new(false.into()),
                dpu_mode: None,
                explore_mode: SiteExplorerExploreMode::LibRedfish,
                explore_drive_firmware: false,
            }
        );

        assert_eq!(
            config.host_health,
            HostHealthConfig {
                hardware_health_reports: model::machine::HardwareHealthReportsConfig::Disabled,
                dpu_agent_version_staleness_threshold: Duration::days(1),
                prevent_allocations_on_stale_dpu_agent_version: true,
                prevent_allocations_on_scout_heartbeat_timeout: true,
                suppress_external_alerting_on_scout_heartbeat_timeout: false,
            }
        );
        assert_eq!(
            config.observability,
            ObservabilityConfig {
                per_object_metrics_for_classifications: vec![
                    HealthAlertClassification::hardware(),
                    HealthAlertClassification::prevent_allocations(),
                ],
            }
        );
        assert_eq!(
            config.machine_state_controller,
            MachineStateControllerConfig {
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(3 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(11),
                    max_concurrency: 22,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
                dpu_wait_time: Duration::minutes(7),
                power_down_wait: Duration::seconds(17),
                failure_retry_time: Duration::minutes(70),
                dpu_up_threshold: Duration::minutes(77),
                scout_reporting_timeout: Duration::minutes(20),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
            }
        );
        assert_eq!(
            config.network_segment_state_controller,
            NetworkSegmentStateControllerConfig {
                network_segment_drain_time: Duration::seconds(45),
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(18 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(188),
                    max_concurrency: 1888,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.vpc_prefix_state_controller,
            VpcPrefixStateControllerConfig {
                vpc_prefix_drain_time: Duration::seconds(46),
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(19 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(199),
                    max_concurrency: 1999,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.ib_partition_state_controller,
            IbPartitionStateControllerConfig {
                controller: StateControllerConfig {
                    iteration_time: std::time::Duration::from_secs(17 * 60),
                    max_object_handling_time: std::time::Duration::from_secs(177),
                    max_concurrency: 1777,
                    processor_dispatch_interval: std::time::Duration::from_secs(2),
                    processor_log_interval: std::time::Duration::from_secs(60),
                    metric_emission_interval: std::time::Duration::from_secs(60),
                    metric_hold_time: std::time::Duration::from_secs(5 * 60),
                },
            }
        );
        assert_eq!(
            config.dpu_network_monitor_pinger_type,
            Some("OobNetBind".to_string())
        );
        assert_eq!(
            config.selected_profile,
            libredfish::BiosProfileType::PowerEfficiency
        );
        assert_eq!(
            config
                .bios_profiles
                .get(&RedfishVendor::Lenovo)
                .unwrap()
                .get("ThinkSystem_SR655_V3")
                .unwrap()
                .get(&libredfish::BiosProfileType::Performance)
                .unwrap()
                .get("OperatingModes_ChooseOperatingMode")
                .unwrap()
                .as_str()
                .unwrap(),
            "MaximumPerformance"
        );
    }

    #[test]
    #[allow(clippy::result_large_err)] // complains about figma::Error which we don't control
    fn deserialize_env_patched_full_config() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("CARBIDE_API_DATABASE_URL", "postgres://othersql");
            jail.set_env("CARBIDE_API_ASN", 777);
            jail.set_env("CARBIDE_API_AUTH", "{permissive_mode=true}");
            jail.set_env(
                "CARBIDE_API_TLS",
                "{identity_pemfile_path=/patched/path/to/cert}",
            );

            let config: CarbideConfig = Figment::new()
                .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
                .merge(Env::prefixed("CARBIDE_API_"))
                .extract()
                .unwrap();
            assert_eq!(config.listen, "[::]:1081".parse().unwrap());
            assert_eq!(config.metrics_endpoint, Some("[::]:1080".parse().unwrap()));
            assert_eq!(config.database_url, "postgres://othersql".to_string());
            assert_eq!(config.asn, 777);
            assert_eq!(
                config.dhcp_servers,
                vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)]
            );
            assert_eq!(config.route_servers, vec![Ipv4Addr::new(9, 10, 11, 12)]);
            assert_eq!(config.dpu_network_monitor_pinger_type, None);
            assert_eq!(
                config.tls.as_ref().unwrap().identity_pemfile_path,
                "/patched/path/to/cert"
            );
            assert_eq!(
                config.tls.as_ref().unwrap().identity_keyfile_path,
                "/path/to/key"
            );
            assert_eq!(config.tls.as_ref().unwrap().root_cafile_path, "/path/to/ca");
            assert!(config.auth.as_ref().unwrap().permissive_mode);
            assert_eq!(
                config
                    .auth
                    .as_ref()
                    .unwrap()
                    .casbin_policy_file
                    .clone()
                    .unwrap()
                    .as_os_str(),
                "/path/to/policy"
            );

            Ok(())
        })
    }

    #[test]
    fn site_explorer_serde_defaults_match_core_defaults() -> eyre::Result<()> {
        // Make sure that if we let serde pick the defaults, it matches Default::default().
        let deserialized = serde_json::from_str::<SiteExplorerConfig>("{}")?;
        assert_eq!(deserialized, SiteExplorerConfig::default());
        Ok(())
    }

    /// Verifies the `[site_explorer] dpu_mode = ...` setting parses
    /// correctly for every named variant. When unset (the default),
    /// `site_explorer.dpu_mode` is `None` and hosts resolve to
    /// `DpuMode::DpuMode`.
    #[test]
    fn site_explorer_dpu_mode_parses_and_defaults_to_none() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();
        assert_eq!(config.site_explorer.dpu_mode, None);

        for (toml_value, expected) in [
            ("dpu_mode", DpuMode::DpuMode),
            ("nic_mode", DpuMode::NicMode),
            ("no_dpu", DpuMode::NoDpu),
        ] {
            let config: CarbideConfig = Figment::new()
                .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                .merge(Toml::string(&format!(
                    "[site_explorer]\ndpu_mode = \"{toml_value}\"\n"
                )))
                .extract()
                .unwrap();
            assert_eq!(
                config.site_explorer.dpu_mode,
                Some(expected),
                "[site_explorer] dpu_mode = {toml_value:?} should parse to {expected:?}",
            );
        }
    }

    /// Real-world site TOMLs may still carry the now-removed
    /// `force_dpu_nic_mode` setting (top-level and/or under
    /// `[site_explorer]`). serde silently ignores unknown keys, so
    /// those files should keep parsing cleanly after the rip-out --
    /// this is the regression guard for that.
    #[test]
    fn legacy_force_dpu_nic_mode_in_toml_still_parses() {
        let _config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(
                "force_dpu_nic_mode = false\n\
                 [site_explorer]\n\
                 force_dpu_nic_mode = true\n",
            ))
            .extract()
            .expect("legacy force_dpu_nic_mode in TOML must still parse");
    }

    #[test]
    fn tracing_config_defaults_when_omitted() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();

        assert!(!config.tracing.enabled);
        assert!(config.tracing.allow_runtime_changes);
        assert_eq!(config.tracing.otlp_endpoint, None);
    }

    #[test]
    fn tracing_config_deserializes_from_toml() {
        let toml = r#"
[tracing]
enabled = true
allow_runtime_changes = false
otlp_endpoint = "http://otel-collector.observability.svc.cluster.local:4317"
"#;

        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();

        assert!(config.tracing.enabled);
        assert!(!config.tracing.allow_runtime_changes);
        assert_eq!(
            config.tracing.otlp_endpoint.as_deref(),
            Some("http://otel-collector.observability.svc.cluster.local:4317")
        );
    }

    #[test]
    fn tracing_config_defaults_runtime_changes_when_section_is_partial() {
        let toml = r#"
[tracing]
enabled = true
"#;

        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();

        assert!(config.tracing.enabled);
        assert!(config.tracing.allow_runtime_changes);
        assert_eq!(config.tracing.otlp_endpoint, None);
    }

    #[test]
    fn test_max_concurrent_updates() -> eyre::Result<()> {
        let test = MaxConcurrentUpdates {
            absolute: Some(10),
            percent: None,
        };
        assert_eq!(test.max_concurrent_updates(1000, 5), Some(10));
        let test = MaxConcurrentUpdates {
            absolute: None,
            percent: Some(10),
        };
        assert_eq!(test.max_concurrent_updates(0, 500), Some(50));
        assert_eq!(test.max_concurrent_updates(7, 500), Some(43));
        assert_eq!(test.max_concurrent_updates(50, 500), Some(0));
        assert_eq!(test.max_concurrent_updates(0, 9), Some(1));

        Ok(())
    }

    #[test]
    fn deserialize_dpa_config() {
        let toml = r#"
enabled=true
mqtt_endpoint = "mqtt.forge"
        "#;

        let dpa_config: DpaConfig = Figment::new().merge(Toml::string(toml)).extract().unwrap();

        assert_eq!(
            dpa_config,
            DpaConfig {
                enabled: true,
                mqtt_endpoint: "mqtt.forge".to_string(),
                mqtt_broker_port: 1884,
                hb_interval: chrono::TimeDelta::minutes(2),
                monitor_run_interval: std::time::Duration::from_secs(60),
                subnet_ip: Ipv4Addr::UNSPECIFIED,
                subnet_mask: 0_i32,
                auth: MqttAuthConfig::default(),
            }
        );
    }

    #[test]
    fn deserialize_dpu_config() {
        let toml = r#"
[dpu_config]
dpu_enable_secure_boot = true
num_of_vfs = 64
"#;

        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();

        assert!(config.dpu_config.dpu_enable_secure_boot);
        assert_eq!(config.dpu_config.num_of_vfs, 64);
        assert!(!config.dpu_config.dpu_models.is_empty());
    }

    /// Validates the hard limit on generated BlueField virtual functions.
    #[test]
    fn deserialize_dpu_config_rejects_too_many_vfs() {
        let toml = r#"
[dpu_config]
num_of_vfs = 127
"#;

        // Extracting the config should fail before runtime provisioning.
        let error = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .merge(Toml::string(toml))
            .extract::<CarbideConfig>()
            .unwrap_err();

        // Surface a clear operator-facing message for the invalid value.
        assert!(
            error
                .to_string()
                .contains("dpu_config.num_of_vfs must be <= 126"),
            "{error}"
        );
    }

    #[test]
    fn deserialize_supernic_firmware_profiles() {
        let toml = r#"
[supernic_firmware_profiles.900-9D3B4-00CV-TA0.MT_0000000884]
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "https://firmware.example.com/fw-32.43.1014.bin"
reset = true

[supernic_firmware_profiles.900-9D3B4-00CV-TB0.MT_0000000885]
part_number = "900-9D3B4-00CV-TB0"
psid = "MT_0000000885"
version = "32.44.0000"
firmware_url = "ssh://firmwarehost/path/to/fw.bin"
        "#;

        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();

        // Two part numbers, each with one PSID.
        assert_eq!(config.supernic_firmware_profiles.len(), 2);

        let profile = config
            .get_supernic_firmware_profile("900-9D3B4-00CV-TA0", "MT_0000000884")
            .expect("should find profile");
        assert_eq!(profile.firmware_spec.version, "32.43.1014");
        assert_eq!(
            profile.flash_spec.firmware_url,
            "https://firmware.example.com/fw-32.43.1014.bin"
        );
        assert!(profile.flash_options.reset);

        let profile2 = config
            .get_supernic_firmware_profile("900-9D3B4-00CV-TB0", "MT_0000000885")
            .expect("should find second profile");
        assert_eq!(profile2.firmware_spec.psid, "MT_0000000885");
        assert!(!profile2.flash_options.reset);

        assert!(
            config
                .get_supernic_firmware_profile("NONEXISTENT", "NOPE")
                .is_none()
        );
    }

    #[test]
    fn supernic_firmware_profiles_multiple_psids_per_part_number() {
        let toml = r#"
[supernic_firmware_profiles.900-9D3B4-00CV-TA0.MT_0000000884]
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "https://firmware.example.com/fw-a.bin"

[supernic_firmware_profiles.900-9D3B4-00CV-TA0.MT_0000000999]
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000999"
version = "32.44.0000"
firmware_url = "https://firmware.example.com/fw-b.bin"
        "#;

        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();

        // One part number with two PSIDs.
        assert_eq!(config.supernic_firmware_profiles.len(), 1);
        assert_eq!(
            config
                .supernic_firmware_profiles
                .get("900-9D3B4-00CV-TA0")
                .unwrap()
                .len(),
            2
        );

        let p1 = config
            .get_supernic_firmware_profile("900-9D3B4-00CV-TA0", "MT_0000000884")
            .unwrap();
        assert_eq!(p1.firmware_spec.version, "32.43.1014");

        let p2 = config
            .get_supernic_firmware_profile("900-9D3B4-00CV-TA0", "MT_0000000999")
            .unwrap();
        assert_eq!(p2.firmware_spec.version, "32.44.0000");
    }

    #[test]
    fn get_mlxconfig_profile_lookup() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .extract()
            .unwrap();

        // Profile exists in config.
        let profile = config
            .get_mlxconfig_profile("test-profile")
            .expect("should find test-profile");
        assert_eq!(profile.name, "test-profile");
        assert_eq!(profile.registry.name, "mlx_generic");

        // Second profile also exists.
        let profile2 = config
            .get_mlxconfig_profile("test-profile2")
            .expect("should find test-profile2");
        assert_eq!(profile2.name, "test-profile2");

        // Non-existent profile returns None.
        assert!(config.get_mlxconfig_profile("nonexistent").is_none());
    }

    #[test]
    fn get_mlxconfig_profile_none_when_unconfigured() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();

        // No mlx-config-profiles section at all.
        assert!(config.mlxconfig_profiles.is_none());
        assert!(config.get_mlxconfig_profile("anything").is_none());
    }

    #[test]
    fn supernic_firmware_profiles_empty_by_default() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();

        assert!(config.supernic_firmware_profiles.is_empty());
    }
    #[test]
    fn deserialize_initial_objects() {
        let f = PathBuf::from(format!("{TEST_DATA_DIR}/initial_objects.toml"));
        let config: InitialObjectsConfig = Toml::from_path(f.as_path()).unwrap();
        let pools = config.pools.as_ref().unwrap();
        let networks = config.networks.as_ref().unwrap();
        let vpcs = config.vpcs.as_ref().unwrap();

        assert_eq!(
            networks.get("admin").unwrap(),
            &NetworkDefinition {
                segment_type: NetworkDefinitionSegmentType::Admin,
                prefix: "172.20.0.0/24".parse().unwrap(),
                prefix_v6: None,
                gateway: "172.20.0.1".parse().unwrap(),
                dhcpv6_link_address: None,
                mtu: 9000,
                reserve_first: 5,
                allocation_strategy: Default::default(),
                vpc_name: None,
            }
        );

        assert_eq!(
            networks.get("DEV1-C09-IPMI-01").unwrap(),
            &NetworkDefinition {
                segment_type: NetworkDefinitionSegmentType::Underlay,
                prefix: "172.99.0.0/26".parse().unwrap(),
                prefix_v6: None,
                gateway: "172.99.0.1".parse().unwrap(),
                dhcpv6_link_address: None,
                mtu: 1500,
                reserve_first: 5,
                allocation_strategy: Default::default(),
                vpc_name: None,
            }
        );

        assert_eq!(
            networks.get("ZERO-DPU-HOST-01-SWP7").unwrap(),
            &NetworkDefinition {
                segment_type: NetworkDefinitionSegmentType::HostInband,
                prefix: "10.217.18.192/30".parse().unwrap(),
                prefix_v6: None,
                gateway: "10.217.18.193".parse().unwrap(),
                dhcpv6_link_address: None,
                mtu: 1500,
                reserve_first: 1,
                allocation_strategy: Default::default(),
                vpc_name: Some("zero-dpu-vpc".to_string()),
            }
        );

        assert_eq!(
            vpcs.get("zero-dpu-vpc").unwrap(),
            &VpcDefinition {
                organization_id: Some(FIXTURE_TENANT_ORG_ID.to_string()),
                network_virtualization_type: VpcVirtualizationType::Flat,
                routing_profile_type: None,
                vni: None,
            }
        );

        assert_eq!(
            pools.get("lo-ip").unwrap(),
            &ResourcePoolDef {
                ranges: Vec::new(),
                prefix: Some("10.180.62.1/26".to_string()),
                pool_type: resource_pool::ResourcePoolType::Ipv4,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            pools.get("vlan-id").unwrap(),
            &ResourcePoolDef {
                ranges: vec![resource_pool::Range {
                    auto_assign: true,
                    start: "100".to_string(),
                    end: "501".to_string()
                }],
                prefix: None,
                pool_type: resource_pool::ResourcePoolType::Integer,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            pools.get("fnn-asn").unwrap(),
            &ResourcePoolDef {
                ranges: vec![resource_pool::Range {
                    auto_assign: true,
                    start: "4268000000".to_string(),
                    end: "4268999999".to_string()
                }],
                prefix: None,
                pool_type: resource_pool::ResourcePoolType::Integer,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            pools.get("vni").unwrap(),
            &ResourcePoolDef {
                ranges: vec![resource_pool::Range {
                    auto_assign: true,
                    start: "1024500".to_string(),
                    end: "1024550".to_string()
                }],
                prefix: None,
                pool_type: resource_pool::ResourcePoolType::Integer,
                delegate_prefix_len: None,
            }
        );
        assert_eq!(
            pools.get("vpc-vni").unwrap(),
            &ResourcePoolDef {
                ranges: vec![resource_pool::Range {
                    auto_assign: true,
                    start: "2024500".to_string(),
                    end: "2024550".to_string()
                }],
                prefix: None,
                pool_type: resource_pool::ResourcePoolType::Integer,
                delegate_prefix_len: None,
            }
        );
    }

    #[test]
    fn dpf_docker_image_pull_secret_overrides_non_excluded_services() {
        let cfg = DpfConfig {
            docker_image_pull_secret: Some("my-custom-secret".to_string()),
            ..DpfConfig::default()
        };

        let services = cfg.resolved_mandatory_services();

        // Override applies to every mandatory service ...
        assert_eq!(
            services.dpu_agent.docker_image_pull_secret,
            "my-custom-secret"
        );
        assert_eq!(
            services.dhcp_server.docker_image_pull_secret,
            "my-custom-secret"
        );
        assert_eq!(services.fmds.docker_image_pull_secret, "my-custom-secret");
        assert_eq!(services.otel.docker_image_pull_secret, "my-custom-secret");

        // ... except dts and doca_hbn, which keep the default.
        assert_eq!(
            services.dts.docker_image_pull_secret,
            DEFAULT_DPF_IMAGE_PULL_SECRET
        );
        assert_eq!(
            services.doca_hbn.docker_image_pull_secret,
            DEFAULT_DPF_IMAGE_PULL_SECRET
        );
    }

    #[test]
    fn dpf_docker_image_pull_secret_unset_keeps_per_service_secrets() {
        // No global override -> services keep their own configured secret.
        let cfg = DpfConfig::default();
        assert!(cfg.docker_image_pull_secret.is_none());

        let services = cfg.resolved_mandatory_services();

        assert_eq!(
            services.dpu_agent.docker_image_pull_secret,
            DEFAULT_DPF_IMAGE_PULL_SECRET
        );
        assert_eq!(
            services.dts.docker_image_pull_secret,
            DEFAULT_DPF_IMAGE_PULL_SECRET
        );
        assert_eq!(
            services.doca_hbn.docker_image_pull_secret,
            DEFAULT_DPF_IMAGE_PULL_SECRET
        );
    }

    // Verifies that a [secrets] config section with KMS, routing, and import settings
    // deserializes correctly from TOML.
    #[test]
    fn secrets_config_deserializes_from_toml() {
        #[derive(Deserialize)]
        struct Wrapper {
            secrets: SecretsConfig,
        }

        let toml_str = r#"
            [secrets]
            import_from = "vault"
            import_approach = "missing_only"

            [secrets.kms]
            active = "local"

            [secrets.kms.providers.local]
            type = "integrated"
            keys.default-key = { env = "CARBIDE_SECRETS_KEY_DEFAULT" }
            keys.bmc-key = { file = "/run/secrets/bmc-key" }

            [secrets.kms.providers.prod-transit]
            type = "transit"
            keys = ["my-transit-key"]

            [secrets.routing]
            "/" = "default-key"
            "machines/bmc" = "bmc-key"
        "#;

        let wrapper: Wrapper = toml::from_str(toml_str).expect("parse secrets config");
        let secrets = wrapper.secrets;

        // Verify KMS config: the `type` field selects the enum variant.
        assert_eq!(secrets.kms.active, "local");
        assert_eq!(secrets.kms.providers.len(), 2);
        assert!(matches!(
            &secrets.kms.providers["local"],
            ProviderConfig::Integrated { keys } if keys.len() == 2
        ));
        assert!(matches!(
            &secrets.kms.providers["prod-transit"],
            ProviderConfig::Transit { keys, transit_mount: None } if keys == &["my-transit-key"]
        ));

        // Verify routing.
        assert_eq!(secrets.routing.len(), 2);
        assert_eq!(secrets.routing["/"], "default-key");
        assert_eq!(secrets.routing["machines/bmc"], "bmc-key");

        // Verify import settings.
        assert_eq!(secrets.import_from, Some(ImportSource::Vault));
        assert_eq!(
            secrets.import_approach,
            crate::secrets::ImportApproach::MissingOnly
        );

        // backends/writer were omitted above, so they default to vault-only
        // (env/file are prepended separately) writing to vault.
        assert_eq!(secrets.backends, vec![CredentialBackend::Vault]);
        assert_eq!(secrets.writer, CredentialBackend::Vault);
    }

    // Verifies that a typo'd import source fails config parsing instead of
    // silently skipping the import.
    #[test]
    fn secrets_config_rejects_unknown_import_source() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[expect(dead_code)]
            secrets: SecretsConfig,
        }

        let toml_str = r#"
            [secrets]
            import_from = "valt"

            [secrets.kms]
            active = "local"

            [secrets.kms.providers.local]
            type = "integrated"
            keys.default-key = { env = "CARBIDE_SECRETS_KEY_DEFAULT" }

            [secrets.routing]
            "/" = "default-key"
        "#;

        assert!(toml::from_str::<Wrapper>(toml_str).is_err());
    }

    // Verifies the backends list and writer parse from their enum values --
    // one with Postgres in front of vault (writes to Postgres) and a
    // postgres-only one (vault not read, writes to Postgres).
    #[test]
    fn secrets_config_parses_backends_and_writer() {
        #[derive(Deserialize)]
        struct Wrapper {
            secrets: SecretsConfig,
        }

        let pg_first = r#"
            [secrets]
            backends = ["postgres", "vault"]
            writer = "postgres"

            [secrets.kms]
            active = "local"
            [secrets.kms.providers.local]
            type = "integrated"
            keys.default-key = { env = "K" }

            [secrets.routing]
            "/" = "default-key"
        "#;
        let secrets = toml::from_str::<Wrapper>(pg_first)
            .expect("parse pg-first")
            .secrets;
        assert_eq!(
            secrets.backends,
            vec![CredentialBackend::Postgres, CredentialBackend::Vault]
        );
        assert_eq!(secrets.writer, CredentialBackend::Postgres);

        // Postgres-only reads, writes to postgres too. (The
        // writer-defaults-to-vault case is covered by the deserialize test
        // above, with vault still in backends -- pairing a postgres-only chain
        // with a vault writer is the read-after-write gap run.rs warns about.)
        let postgres_only = r#"
            [secrets]
            backends = ["postgres"]
            writer = "postgres"

            [secrets.kms]
            active = "local"
            [secrets.kms.providers.local]
            type = "integrated"
            keys.default-key = { env = "K" }

            [secrets.routing]
            "/" = "default-key"
        "#;
        let secrets = toml::from_str::<Wrapper>(postgres_only)
            .expect("parse postgres-only")
            .secrets;
        assert_eq!(secrets.backends, vec![CredentialBackend::Postgres]);
        assert_eq!(secrets.writer, CredentialBackend::Postgres);
    }

    // Verifies a typo'd backend or writer value fails parsing rather than
    // silently dropping a backend from the chain.
    #[test]
    fn secrets_config_rejects_unknown_backend() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[expect(dead_code)]
            secrets: SecretsConfig,
        }

        let base_kms = r#"
            [secrets.kms]
            active = "local"
            [secrets.kms.providers.local]
            type = "integrated"
            keys.default-key = { env = "K" }
            [secrets.routing]
            "/" = "default-key"
        "#;

        let bad_backend = format!("[secrets]\nbackends = [\"postgrez\"]\n{base_kms}");
        assert!(toml::from_str::<Wrapper>(&bad_backend).is_err());

        // env/file are local overrides, not backends -- they belong in
        // [credentials.*], not [secrets].backends, so they're rejected here.
        let env_as_backend = format!("[secrets]\nbackends = [\"env\"]\n{base_kms}");
        assert!(toml::from_str::<Wrapper>(&env_as_backend).is_err());

        let bad_writer = format!("[secrets]\nwriter = \"valt\"\n{base_kms}");
        assert!(toml::from_str::<Wrapper>(&bad_writer).is_err());
    }

    // Verifies that a misspelled optional key in [secrets] -- here
    // `import_fom` for `import_from` -- fails to parse instead of leaving
    // the import silently disabled. Without deny_unknown_fields, the typo'd
    // key is ignored and an existing site can boot on empty Postgres.
    #[test]
    fn secrets_config_rejects_misspelled_field() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[expect(dead_code)]
            secrets: SecretsConfig,
        }

        let toml_str = r#"
            [secrets]
            import_fom = "vault"

            [secrets.kms]
            active = "local"

            [secrets.kms.providers.local]
            type = "integrated"
            keys.default-key = { env = "CARBIDE_SECRETS_KEY_DEFAULT" }

            [secrets.routing]
            "/" = "default-key"
        "#;

        assert!(toml::from_str::<Wrapper>(toml_str).is_err());
    }

    // Verifies that a field belonging to the other provider type -- here
    // transit_mount on an integrated provider -- fails to parse instead of
    // being silently ignored.
    #[test]
    fn secrets_config_rejects_unknown_provider_field() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[expect(dead_code)]
            secrets: SecretsConfig,
        }

        let toml_str = r#"
            [secrets.kms]
            active = "local"

            [secrets.kms.providers.local]
            type = "integrated"
            transit_mount = "transit"
            keys.default-key = { env = "CARBIDE_SECRETS_KEY_DEFAULT" }

            [secrets.routing]
            "/" = "default-key"
        "#;

        assert!(toml::from_str::<Wrapper>(toml_str).is_err());
    }

    // Verifies that a provider with the wrong field for its type -- here an
    // integrated provider given transit's key list -- fails to parse
    // instead of deferring the mistake to startup.
    #[test]
    fn secrets_config_rejects_mismatched_provider_fields() {
        #[derive(Deserialize)]
        struct Wrapper {
            #[expect(dead_code)]
            secrets: SecretsConfig,
        }

        let toml_str = r#"
            [secrets.kms]
            active = "local"

            [secrets.kms.providers.local]
            type = "integrated"
            keys = ["not-a-key-map"]

            [secrets.routing]
            "/" = "default-key"
        "#;

        assert!(toml::from_str::<Wrapper>(toml_str).is_err());
    }

    // Verifies that secrets config is optional — a config without [secrets] should have None.
    #[test]
    fn secrets_config_absent_by_default() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();

        assert!(config.secrets.is_none());
    }
}
