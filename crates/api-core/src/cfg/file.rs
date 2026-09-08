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

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use bmc_vendor::BMCVendor;
use carbide_authn::config::{AllowedCertCriteria, TrustConfig};
use carbide_dpf::types::{DpfProxyDetails, DpuDeploymentType};
use carbide_firmware::FirmwareConfig;
use carbide_firmware::defaults::{
    BF2_BMC_VERSION, BF2_CEC_VERSION, BF2_NIC_VERSION, BF2_UEFI_VERSION, BF3_BMC_VERSION,
    BF3_CEC_VERSION, BF3_NIC_VERSION, BF3_UEFI_VERSION,
};
use carbide_host_support::bootstrap_ca::BootstrapCaSource;
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
use carbide_utils::none_if_empty::NoneIfEmpty;
use chrono::Duration;
use db::host_naming::HostNamingStrategyKind;
use duration_str::{deserialize_duration, deserialize_duration_chrono};
use figment::Figment;
use figment::providers::Serialized;
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
use model::network_security_group::{
    NetworkSecurityGroupRule, NetworkSecurityGroupRuleAction, NetworkSecurityGroupRuleDirection,
    NetworkSecurityGroupRuleNet, NetworkSecurityGroupRuleProtocol,
};
use model::network_segment::NetworkDefinition;
use model::resource_pool::define::ResourcePoolDef;
use model::tenant::identity_config::SigningAlgorithm;
use model::vpc::VpcConfig;
pub use model::vpc::{PrefixFilterPolicyEntry, RouteTargetConfig};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};

use crate::CarbideError;

pub(crate) const DEFAULT_DPU_NUM_OF_VFS: u32 = carbide_dpf::DEFAULT_DPU_NUM_OF_VFS;
pub(crate) const MAX_DPU_NUM_OF_VFS: u32 = 126;

// Deployment selectors must never reuse labels whose values NICo supplies independently.
// The shared marker would make every deployment select every DPUNode, while the contextual
// host-BMC label is overwritten with an address during registration and would match none.
const RESERVED_DPF_DEPLOYMENT_NODE_LABELS: [(&str, &str); 2] = [
    (
        carbide_dpf::DPU_ENABLED_NODE_LABEL,
        "the shared DPF-enabled node marker",
    ),
    (
        carbide_machine_controller::dpf::HOST_BMC_IP_LABEL,
        "the per-node host BMC address",
    ),
];

const DPF_DEPLOYMENT_NAME_MAX_LENGTH: usize = 20;
const DPF_FLAVOR_HASH_SUFFIX_LENGTH: usize = 17;
const KUBERNETES_LABEL_NAME_MAX_LENGTH: usize = 63;
const GB200_RESOURCE_SUFFIX: &str = "-gb200";
// The DPUDeployment CRD allows only 20 characters. The default BF3 name already
// uses 18, so `-g` is the longest suffix that preserves it without truncation.
const GB200_DEPLOYMENT_SUFFIX: &str = "-g";

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
#[serde(deny_unknown_fields)]
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

    /// Whether unknown configuration fields should prevent startup.
    ///
    /// Defaults to `false`, which logs each unknown field and continues so
    /// configuration can be deployed independently of the supporting binary.
    #[serde(default)]
    pub deny_unknown_fields: bool,

    /// How long a caller may wait for a connection from the pool before the
    /// attempt fails (sqlx's own default). It trips on a stalled database or
    /// a saturated pool alike. Default is 30s.
    #[serde(
        default = "default_database_pool_acquire_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub database_pool_acquire_timeout: std::time::Duration,

    /// How long a pooled database connection may sit unused before the
    /// pool closes it. Pins sqlx's implicit default explicitly, keeping the
    /// pool's idle reaping well inside the Postgres server's sixty-minute
    /// idle-session reaper. Default is 10m.
    #[serde(
        default = "default_database_pool_idle_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub database_pool_idle_timeout: std::time::Duration,

    /// Maximum age of a pooled database connection before it is closed and
    /// replaced, so the pool keeps re-balancing onto the current primary
    /// after a database failover. Pins sqlx's implicit default explicitly.
    /// Default is 30m.
    #[serde(
        default = "default_database_pool_max_lifetime",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub database_pool_max_lifetime: std::time::Duration,

    /// Bounds the number of API requests that may execute or wait for
    /// execution. The limits are shared by gRPC and admin HTTP traffic.
    #[serde(default)]
    pub api_admission_control: ApiAdmissionControlConfig,

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

    /// List of IP prefixes (in CIDR notation) that tenant instances are not allowed to reach.
    ///
    /// FNN supports IPv4 and IPv6 prefixes. All non-FNN virtualizers apply only IPv4 prefixes.
    #[serde(default)]
    pub deny_prefixes: Vec<IpNetwork>,

    /// List of IP prefixes (in CIDR notation) that are assigned for tenant
    /// use within this site. Supports both IPv4 and IPv6 prefixes.
    #[serde(default)]
    pub site_fabric_prefixes: Vec<IpNetwork>,

    /// Opts this site into exact tenant VpcPrefix reuse checks.
    ///
    /// Defaults to `false`. The complete eligibility, rejection, and database
    /// fallback contract is documented under "Tenant prefix overlap checks"
    /// in `crates/api-core/src/cfg/README.md`.
    #[serde(default)]
    pub tenant_prefix_overlap_enabled: bool,

    /// Maximum number of tenant-managed SitePrefixes retained for one tenant
    /// at this site. Prefixes awaiting removal still count against this limit
    /// and keep their CIDR reserved.
    #[serde(default = "default_max_site_prefixes_per_tenant")]
    pub max_site_prefixes_per_tenant: u32,

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

    /// Cap on outstanding Redfish sessions per calling service identity per
    /// BMC. Every `GetBmcCredentials` call mints a fresh session, so this is
    /// what bounds a caller's session slots on the BMC: a mint that pushes
    /// past the cap revokes that caller's oldest sessions. Size it to the
    /// caller's replica count plus headroom for restarts.
    /// Default is 4 (two replicas, each holding a current and a next
    /// session). Values below 1 are treated as 1.
    #[serde(default = "default_bmc_max_sessions_per_caller")]
    pub bmc_max_sessions_per_caller: usize,

    /// When `true`, `GetBmcCredentials` may return
    /// `UsernamePassword` credentials for BMCs whose Redfish ServiceRoot
    /// does not expose `SessionService`. When `false` (the default), such
    /// BMCs surface a `NoSessionService` error to the caller and no
    /// basic-auth fallback is performed. See the "Basic-auth fallback"
    /// section of `crates/api/src/credentials/bmc_session_manager.rs` for
    /// the full semantics.
    #[serde(default)]
    pub allow_bmc_basic_auth_fallback: bool,

    /// Allows machine discovery to trust the caller-supplied interface ID
    /// instead of selecting an interface from the request's remote IP.
    /// This is intended only for test environments using machine-a-tron
    /// in single-IP mode.
    #[serde(default)]
    pub allow_insecure_discovery: bool,

    /// Controls whether NICo may reconcile a boot interface selection recorded as
    /// `RedfishChassisId` or `RedfishSerialNumber` after ordering DPU-attached Admin interfaces
    /// by the `domain:bus:device.function` PCI addresses in scout's `HardwareInfo`.
    ///
    /// The setting is read at startup and defaults to `false`. NICo records available comparisons
    /// in structured logs and `carbide_scout_pci_evaluations_total` regardless of this setting.
    /// When `false`, it does not change the selection. When `true`, reconciliation requires at
    /// least two eligible interfaces, a complete and unique candidate, `ManagedHostState::Ready`
    /// or `ManagedHostState::HostInit` with `MachineState::Discovered`, no `Instance` or primary
    /// interface prediction, and no conflicting or integrated-NIC primary.
    ///
    /// If the selected MAC is already desired and primary, NICo changes only the source to
    /// `ScoutReportPci`. Otherwise it updates the desired target and primary together and enqueues
    /// the state handler. A `Ready` host enters `BootConfiguring`; `HostInit` completes its reboot
    /// handshake first.
    #[serde(default)]
    pub scout_boot_interface_correction_enabled: bool,

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

    /// Deprecated compatibility key. This setting no longer affects runtime
    /// behavior; keep accepting it temporarily so existing site files can be
    /// migrated without weakening unknown-field validation.
    #[doc(hidden)]
    #[serde(default, rename = "force_dpu_nic_mode", skip_serializing)]
    pub deprecated_force_dpu_nic_mode: Option<bool>,

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

    /// Site-wide enable for passive BMC credential rotation. When
    /// `false` (the default), a Ready host never enters `RotatingBmc` on its own
    /// even if a device lags the staged site-wide target. This is the fleet
    /// kill-switch for rolling the feature out site-by-site; the operator
    /// force-converge escape hatch (`TriggerBmcCredentialRotation`) bypasses it.
    #[serde(default)]
    pub bmc_rotation_enabled: bool,

    /// Site-wide enable for passive UEFI credential rotation. When
    /// `false` (the default), a Ready host never enters `RotatingHostUefi` on its
    /// own even if it lags the staged site-wide target. This is the fleet
    /// kill-switch for rolling the feature out site-by-site; the operator
    /// force-converge escape hatch (`TriggerUefiCredentialRotation`) bypasses it.
    #[serde(default)]
    pub uefi_rotation_enabled: bool,

    /// Site-wide enable for NIC lockdown IKM rotation. When `false` (the
    /// default), the SuperNIC lock/unlock flow keeps deriving keys from (and
    /// re-locking cards at) their current tracked IKM version, so a staged
    /// `RotateCredential(lockdown_ikm)` bumps the site-wide target without any
    /// card migrating -- the deliberate cutover flip. When `true`, the
    /// assignment-cycle lock derives from the staged site-wide target, so cards
    /// migrate to the new IKM as tenants cycle. Unlock always derives from the
    /// version a card is actually locked under regardless of this flag, so
    /// flipping it off never bricks an already-migrated card. This is the fleet
    /// kill-switch for rolling the feature out site-by-site.
    #[serde(default, alias = "lockdown_ikm_rotation_enabled")]
    pub nic_lockdown_ikm_rotation_enabled: bool,

    /// Site-wide enable for factory-resetting the host BMC during tenant
    /// release. When `false` (the default), tenant release skips the BMC
    /// factory-reset sub-flow entirely and proceeds directly to `PowerCycle`.
    /// When `true`, the release flow factory-resets the BMC, waits for it to
    /// return, restores the device's previous per-device credential, then
    /// continues with the existing power-cycle / boot-order repair. Opt-in
    /// per site for rollout.
    #[serde(default)]
    pub bmc_factory_reset_on_instance_termination_enabled: bool,

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

    /// ExtensionServiceStateController related configuration parameter
    #[serde(default)]
    pub extension_service_state_controller: ExtensionServiceStateControllerConfig,

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

    /// Controls the DPU ToR BGP health checks. If unset, both uplinks are
    /// checked; zero disables the checks, and values above two produce a
    /// configuration health alert. With one, either established link satisfies
    /// the minimum, but a failed p0 is still reported because PXE boot depends
    /// on the primary session.
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

    /// Node-auth: bearer JWTs that Scout / DPU-agent self-sign with their
    /// existing mTLS client-certificate key, accepted alongside (or instead
    /// of) mTLS. Section `[node_auth]`.
    #[serde(default)]
    pub node_auth: NodeAuthConfig,

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

    /// EwEthersConfig refers to East West Ethernet (aka
    /// Cluster Interconnect Network) configuration
    #[serde(default)]
    pub ewethers_config: Option<EwEthersConfig>,

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
    /// provisioning-time host-representor bridging.
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
    /// 1. skip DPU management and use DPUs as NICs (set the site-wide `[site_explorer] dpu_policy = "nic"`, or per-host `ExpectedMachine.dpu_policy`)
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

    /// URL template for the "Logs" link on machine and endpoint detail
    /// pages. The placeholder `{search}` is replaced with the machine ID
    /// or BMC IP. When empty, the Logs link is hidden.
    #[serde(default)]
    pub web_ui_logs_link_template: String,

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

    /// Operator-managed static credential sources. These settings contain
    /// only source locations and reload policy; credential values stay in the
    /// referenced file or process environment. When a file is configured, the
    /// local environment/file chain is read first for non-UFM credentials.
    /// `credentials.ufm_source` exclusively selects local or persistent
    /// backend ownership for all UFM credentials.
    #[serde(default)]
    pub credentials: CredentialsConfig,

    /// IP cleanup on lease expiry
    #[serde(default)]
    pub dhcp_lease_expiry_handling: bool,

    /// Certificate vending backend. Selected independently of the credential
    /// store; absent means certs are issued from the credential Vault.
    #[serde(default)]
    pub certificates: CertificatesConfig,
}

/// Global admission limits for business requests handled by nico-api.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApiAdmissionControlConfig {
    /// Whether admission control is active.
    #[serde(default = "default_to_true")]
    pub enabled: bool,

    /// Maximum number of requests executing business handlers concurrently.
    #[serde(default = "default_api_admission_max_work_in_flight")]
    pub max_work_in_flight: usize,

    /// Maximum number of requests waiting for an execution slot.
    #[serde(default = "default_api_admission_max_pending")]
    pub max_pending: usize,

    /// Default maximum number of concurrently executing requests for one
    /// authenticated client.
    #[serde(default = "default_api_admission_max_work_in_flight_per_client")]
    pub max_work_in_flight_per_client: usize,

    /// Default hard pending-request bound for one authenticated client.
    #[serde(default = "default_api_admission_max_pending_per_client")]
    pub max_pending_per_client: usize,

    /// Maximum time a pending request may wait for execution.
    #[serde(
        default = "default_api_admission_pending_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub pending_timeout: std::time::Duration,

    /// How long empty client scheduling state remains cached.
    #[serde(
        default = "default_api_admission_client_idle_timeout",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub client_idle_timeout: std::time::Duration,

    /// Capacity overrides keyed by exact SPIFFE service identifier.
    #[serde(default)]
    pub service_limits: BTreeMap<String, ApiAdmissionServiceLimitsConfig>,
}

/// Admission limits for one trusted internal service identity.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ApiAdmissionServiceLimitsConfig {
    pub max_work_in_flight: usize,
    pub max_pending: usize,
    #[serde(
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub pending_timeout: std::time::Duration,
}

impl Default for ApiAdmissionControlConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_work_in_flight: default_api_admission_max_work_in_flight(),
            max_pending: default_api_admission_max_pending(),
            max_work_in_flight_per_client: default_api_admission_max_work_in_flight_per_client(),
            max_pending_per_client: default_api_admission_max_pending_per_client(),
            pending_timeout: default_api_admission_pending_timeout(),
            client_idle_timeout: default_api_admission_client_idle_timeout(),
            service_limits: BTreeMap::new(),
        }
    }
}

impl ApiAdmissionControlConfig {
    pub(crate) fn admission_limits(
        &self,
    ) -> eyre::Result<Option<crate::admission::AdmissionLimits>> {
        if !self.enabled {
            return Ok(None);
        }

        crate::admission::AdmissionLimits::new(
            self.max_work_in_flight,
            self.max_pending,
            self.max_work_in_flight_per_client,
            self.max_pending_per_client,
            self.pending_timeout,
            self.client_idle_timeout,
        )
        .map(Some)
        .map_err(|error| eyre::eyre!("api_admission_control.{error}"))
    }

    /// Reject invalid bounds before the API listener starts.
    pub fn validate(&self) -> eyre::Result<()> {
        let Some(_limits) = self.admission_limits()? else {
            return Ok(());
        };
        for (service_id, limits) in &self.service_limits {
            eyre::ensure!(
                !service_id.trim().is_empty(),
                "api_admission_control.service_limits contains an empty service identifier"
            );
            crate::admission::ClientLimits::new(
                limits.max_work_in_flight,
                limits.max_pending,
                limits.pending_timeout,
                self.max_work_in_flight,
                self.max_pending,
            )
            .map_err(|error| {
                eyre::eyre!("api_admission_control.service_limits.{service_id}.{error}")
            })?;
        }
        Ok(())
    }
}

/// `[certificates]` config section: selects the backend that vends machine and
/// service certificates, independently of where credentials are stored.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CertificatesConfig {
    /// Which backend issues certificates. Defaults to sharing the credential
    /// Vault client (historical behavior).
    #[serde(default)]
    pub backend: CertBackendKind,

    /// Connection settings for a dedicated certificate Vault. Required when
    /// `backend = "dedicated_vault"`, ignored otherwise.
    #[serde(default)]
    pub dedicated_vault: Option<DedicatedVaultSettings>,
}

/// Non-secret sources for operator-managed credentials.
///
/// The file source is optional. When present, it takes precedence over the
/// legacy environment-selected file source and is read before credential
/// backends such as Vault or Postgres. `ufm_source` controls whether UFM reads
/// preserve that local-first order or use one authoritative source.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialsConfig {
    /// Selects the read precedence and mutation policy for UFM credentials.
    /// Defaults to local-first reads with persistent-backend fallback.
    #[serde(default)]
    pub ufm_source: UfmCredentialSource,

    /// A watched file containing static credentials. Its contents are never
    /// embedded in `CarbideConfig`.
    #[serde(default)]
    pub file: Option<CredentialFileSourceConfig>,
}

/// Configuration for the watched static-credentials file.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialFileSourceConfig {
    /// Absolute or working-directory-relative path to the JSON or YAML file
    /// containing static credentials.
    pub path: PathBuf,

    /// Nonzero interval used to detect projected-Secret replacements that do
    /// not emit a filesystem watch event. Defaults to 60 seconds.
    #[serde(
        default = "CredentialFileSourceConfig::default_poll_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub poll_interval: std::time::Duration,
}

/// Read precedence and mutation policy for site UFM credentials.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UfmCredentialSource {
    /// Preserve the existing local-first behavior: read environment/file UFM
    /// entries before the persistent backend and mutate the backend.
    #[default]
    LocalFirst,
    /// Read and mutate UFM credentials in the configured persistent backend.
    /// Local environment/file UFM entries are ignored.
    Backend,
    /// Read UFM credentials only from the local environment/file sources.
    /// Every configured fabric must be present when IB management starts, and
    /// persistent backend mutation is disabled.
    Local,
}

impl CredentialFileSourceConfig {
    /// Returns the polling interval used when `poll_interval` is omitted.
    pub const fn default_poll_interval() -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }
}

impl CredentialsConfig {
    /// Returns whether local sources authoritatively own UFM credentials.
    pub fn uses_authoritative_local_ufm_credentials(&self) -> bool {
        self.ufm_source == UfmCredentialSource::Local
    }
}

/// Tag selecting the certificate backend. The matching settings (if any) live
/// in their own sub-table, so the choice is explicit rather than inferred.
// The shared `Vault` suffix is intentional: both current backends are Vault
// backends. The lint resolves once a non-Vault backend is added.
#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CertBackendKind {
    /// Reuse the credential store's Vault client — one client, one token lease.
    #[default]
    SharedVault,
    /// Use a dedicated Vault configured under `[certificates.dedicated_vault]`.
    DedicatedVault,
}

/// `[certificates.dedicated_vault]` settings.
///
/// The connection-identifying fields are required, so a partial section fails
/// to parse rather than silently inheriting the credential Vault's process-wide
/// `VAULT_*` environment configuration.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DedicatedVaultSettings {
    /// Vault address, e.g. `https://vault-certs.example:8200`.
    pub address: String,
    /// PKI secrets-engine mount path on the target Vault.
    pub pki_mount_location: String,
    /// PKI role used to sign leaf certificates.
    pub pki_role_name: String,
    /// Token for root-token auth; required only when the pod has no Kubernetes
    /// service-account token.
    #[serde(default)]
    pub token: Option<String>,
    /// CA bundle that signs the target Vault's TLS cert. Defaults to the site
    /// root / `VAULT_CACERT`.
    #[serde(default)]
    pub vault_cacert: Option<String>,
    /// Optional Vault Enterprise or HCP Vault Dedicated namespace for this
    /// dedicated certificate client. Takes precedence over `VAULT_NAMESPACE`.
    #[serde(default)]
    pub namespace: Option<String>,
}

// Hand-rolled so the root `token` is never printed verbatim in logs or errors;
// only its presence is shown. Serialization is handled separately by
// `CarbideConfig::redacted()`, which clears the token before the config is
// serialized for the admin API or config-dump paths.
impl std::fmt::Debug for DedicatedVaultSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DedicatedVaultSettings")
            .field("address", &self.address)
            .field("pki_mount_location", &self.pki_mount_location)
            .field("pki_role_name", &self.pki_role_name)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field("vault_cacert", &self.vault_cacert)
            .field("namespace", &self.namespace)
            .finish()
    }
}

impl CertificatesConfig {
    /// Convert the parsed section into the runtime certificate config, failing
    /// fast if a dedicated backend was selected without its settings.
    pub fn to_certificate_config(&self) -> eyre::Result<carbide_secrets::CertificateConfig> {
        let backend = match self.backend {
            CertBackendKind::SharedVault => carbide_secrets::CertBackend::SharedVault,
            CertBackendKind::DedicatedVault => {
                let dedicated = self.dedicated_vault.as_ref().ok_or_else(|| {
                    eyre::eyre!(
                        "[certificates] backend = \"dedicated_vault\" requires a \
                         [certificates.dedicated_vault] section"
                    )
                })?;
                carbide_secrets::CertBackend::DedicatedVault(
                    carbide_secrets::DedicatedVaultConfig {
                        address: dedicated.address.clone(),
                        pki_mount_location: dedicated.pki_mount_location.clone(),
                        pki_role_name: dedicated.pki_role_name.clone(),
                        token: dedicated.token.clone(),
                        vault_cacert: dedicated.vault_cacert.clone(),
                        namespace: dedicated.namespace.clone(),
                    },
                )
            }
        };
        Ok(carbide_secrets::CertificateConfig { backend })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
            rack_profiles: self.rack_profiles.clone(),

            selected_profile: self.selected_profile,
            bios_profiles: self.bios_profiles.clone(),
            oem_manager_profiles: self.oem_manager_profiles.clone(),

            ewethers_enabled: self.is_ewethers_enabled(),
            astra_enabled: self.is_astra_enabled(),
            dpf_enabled: self.dpf.enabled,
            dpu_service_sync_enabled: self.dpf.dpu_service_sync_enabled,
            spdm_enabled: self.spdm.enabled,
            bmc_rotation_enabled: self.bmc_rotation_enabled,
            uefi_rotation_enabled: self.uefi_rotation_enabled,
            nic_lockdown_ikm_rotation_enabled: self.nic_lockdown_ikm_rotation_enabled,
            bmc_factory_reset_on_instance_termination_enabled: self
                .bmc_factory_reset_on_instance_termination_enabled,

            dpu_enable_secure_boot: self.dpu_config.dpu_enable_secure_boot,
            restart_ovs_on_use_admin_network_change: self
                .dpu_config
                .restart_ovs_on_use_admin_network_change,
        }
    }
}

/// Observability settings shared across all state controllers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    /// Health alert classifications for which an additional per-object metric
    /// (`carbide_object_unhealthy_by_classification_count`) is emitted,
    /// labeled with the object's type and id (e.g. `object_type="machine"`,
    /// `object_id="<machine_id>"`).
    #[serde(default)]
    pub per_object_metrics_for_classifications: Vec<HealthAlertClassification>,

    /// Per-object state progress metrics, served on a dedicated endpoint
    /// (see `docs/design/per-object-state-metrics.md`).
    #[serde(default)]
    pub per_object_state_metrics: PerObjectStateMetricsConfig,
}

/// Configuration for the per-object state metrics endpoint. Off by default:
/// the series cost O(fleet) cardinality, so operators opt in and scrape the
/// dedicated endpoint at their own cadence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PerObjectStateMetricsConfig {
    /// Whether the per-object state metrics endpoint is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// The address the dedicated endpoint listens on.
    #[serde(default = "default_per_object_state_metrics_listen_address")]
    pub listen_address: SocketAddr,
    /// Object types to emit state series for; defaults to all. An empty list
    /// disables emission even when `enabled = true`.
    #[serde(default = "default_per_object_state_metrics_object_types")]
    pub object_types: Vec<PerObjectStateMetricObjectType>,
}

impl Default for PerObjectStateMetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_address: default_per_object_state_metrics_listen_address(),
            object_types: default_per_object_state_metrics_object_types(),
        }
    }
}

fn default_per_object_state_metrics_listen_address() -> SocketAddr {
    SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 9091)
}

fn default_per_object_state_metrics_object_types() -> Vec<PerObjectStateMetricObjectType> {
    PerObjectStateMetricObjectType::ALL.to_vec()
}

/// Object types that can emit per-object state series. An enum so an
/// unrecognized token fails config deserialization instead of silently
/// emitting nothing for the intended type.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PerObjectStateMetricObjectType {
    Machine,
    Switch,
    Rack,
    PowerShelf,
    NetworkSegment,
    VpcPrefix,
    SpdmAttestation,
    IbPartition,
}

impl PerObjectStateMetricObjectType {
    const ALL: [Self; 8] = [
        Self::Machine,
        Self::Switch,
        Self::Rack,
        Self::PowerShelf,
        Self::NetworkSegment,
        Self::VpcPrefix,
        Self::SpdmAttestation,
        Self::IbPartition,
    ];

    /// The `object_type` label token; must match what the controllers record.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::Switch => "switch",
            Self::Rack => "rack",
            Self::PowerShelf => "power_shelf",
            Self::NetworkSegment => "network_segment",
            Self::VpcPrefix => "vpc_prefix",
            Self::SpdmAttestation => "spdm_attestation",
            Self::IbPartition => "ib_partition",
        }
    }
}

/// One external tool link rendered in the admin web UI's "Tools"
/// sidebar.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    /// running `nico-admin-cli secrets re-wrap`.
    ///
    /// Example:
    /// ```toml
    /// [secrets.routing]
    /// "/" = "default-key"
    /// "machines/bmc" = "bmc-key"
    /// ```
    pub routing: std::collections::HashMap<String, String>,

    /// The credential *backend* read order, highest priority first (first match
    /// wins). Enabled local-override readers (env, file) are normally tried
    /// ahead of these; `credentials.ufm_source` can suppress local UFM entries
    /// or make them authoritative. This list only orders the persistent
    /// backends. Order is the operator's choice -- list the backends you want,
    /// in the priority you want. Defaults to `["vault"]` -- with the local
    /// overrides, that is the env -> file -> vault chain.
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
    /// reads and writes flow. When `credentials.ufm_source = "local"`, UFM
    /// paths are excluded so the import preserves local ownership.
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
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum DpfDpuAgentBootstrapCa {
    /// Preserve the legacy behavior: download the trust anchor from nico-pxe.
    LegacyDownload {
        /// Optional full endpoint override. When omitted, the DPU agent uses its
        /// built-in legacy endpoint.
        #[serde(default)]
        url: Option<url::Url>,
    },
    /// Copy the trust anchor projected from a Kubernetes object.
    Mounted {
        /// Kubernetes object type containing the trust anchor.
        object_kind: DpfBootstrapCaObjectKind,
        /// Name of the Kubernetes object in the DPU cluster.
        name: String,
        /// Key within the Kubernetes object.
        #[serde(default = "default_dpf_bootstrap_ca_key")]
        key: String,
    },
}

impl Default for DpfDpuAgentBootstrapCa {
    fn default() -> Self {
        Self::LegacyDownload { url: None }
    }
}

impl DpfDpuAgentBootstrapCa {
    /// Validate values that cannot be constrained by deserialization alone.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::LegacyDownload { url: Some(url) }
                if !matches!(url.scheme(), "http" | "https") =>
            {
                Err("dpf.dpu_agent_bootstrap_ca.url must use http or https".to_string())
            }
            Self::Mounted { name, .. } if name.trim().is_empty() => Err(
                "dpf.dpu_agent_bootstrap_ca.name must not be empty for mounted sources".to_string(),
            ),
            Self::Mounted { name, .. } if !is_valid_kubernetes_object_name(name) => Err(
                "dpf.dpu_agent_bootstrap_ca.name must be a valid Kubernetes DNS subdomain for mounted sources"
                    .to_string(),
            ),
            Self::Mounted { key, .. } if key.trim().is_empty() => Err(
                "dpf.dpu_agent_bootstrap_ca.key must not be empty for mounted sources".to_string(),
            ),
            Self::Mounted { key, .. } if !is_valid_kubernetes_data_key(key) => Err(
                "dpf.dpu_agent_bootstrap_ca.key must be a valid Kubernetes Secret or ConfigMap data key for mounted sources"
                    .to_string(),
            ),
            _ => Ok(()),
        }
    }
}

const KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH: usize = 253;

// Secret and ConfigMap names use Kubernetes' DNS-1123 subdomain validation.
fn is_valid_kubernetes_object_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH
        && value.split('.').all(|label| {
            let bytes = label.as_bytes();
            bytes
                .first()
                .is_some_and(|byte| is_dns_1123_alphanumeric(*byte))
                && bytes
                    .last()
                    .is_some_and(|byte| is_dns_1123_alphanumeric(*byte))
                && bytes
                    .iter()
                    .all(|byte| is_dns_1123_alphanumeric(*byte) || *byte == b'-')
        })
}

fn is_dns_1123_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

// Kubernetes applies this validation to keys in both ConfigMap and Secret data.
fn is_valid_kubernetes_data_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH
        && value != "."
        && !value.starts_with("..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_valid_kubernetes_label_key(value: &str) -> bool {
    let (prefix, name) = value
        .split_once('/')
        .map_or((None, value), |(prefix, name)| (Some(prefix), name));
    let bytes = name.as_bytes();

    prefix.is_none_or(is_valid_kubernetes_object_name)
        && name.len() <= KUBERNETES_LABEL_NAME_MAX_LENGTH
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && is_valid_kubernetes_data_key(name)
}

/// Kubernetes object kinds supported as DPF DPU-agent trust-anchor sources.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DpfBootstrapCaObjectKind {
    Secret,
    ConfigMap,
}

fn default_dpf_bootstrap_ca_key() -> String {
    "ca.crt".to_string()
}

/// Supplies Serde's legacy PF_TOTAL_SF default when the operator omits the reserve.
fn default_dpf_pf_total_sf_reserved() -> u32 {
    carbide_dpf::DEFAULT_PF_TOTAL_SF_RESERVED
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DpfConfig {
    /// Enables DPF deployment.
    #[serde(default)]
    pub enabled: bool,
    /// Opts the DPF namespace into deployment-scoped DPUServiceInterfaces.
    /// Changing modes requires operators to remove old-mode NICo resources and
    /// re-ingest DPUs; NICo neither detects nor deletes those resources.
    #[serde(default)]
    pub deployment_scoped_service_interfaces: bool,
    /// SF capacity reserved beyond configured NICo-managed service endpoints.
    /// Without intercept bridging, this remains the complete legacy `PF_TOTAL_SF` value.
    #[serde(default = "default_dpf_pf_total_sf_reserved")]
    pub pf_total_sf_reserved: u32,
    /// Whether carbide rolls a changed DPUService out on its own, by releasing
    /// the DPF maintenance hold for hosts it has confirmed are already running
    /// the software their DPUDeployment declares.
    ///
    /// This selects *who* opens the gate, never whether one exists. DPF is
    /// always configured to park a changed DPUService behind a maintenance hold
    /// (`upgradePolicy.applyNodeEffect`), so no service update ever reaches a DPU
    /// without something having checked its side effects first.
    ///
    /// On by default. Setting it to false does not resume unchecked rollout: it
    /// means the held DPUs wait for an operator to release them deliberately,
    /// rather than for carbide to do it on the host's next idle sweep. Hosts
    /// still awaiting reprovisioning, and hosts carrying a live tenant instance,
    /// keep their hold either way.
    #[serde(default = "default_to_true")]
    pub dpu_service_sync_enabled: bool,
    /// Optional override for the Kubernetes `imagePullSecrets` entry used to pull service
    /// docker images. When set, it is applied to every mandatory service except `dts` and
    /// `doca_hbn`, which take a pull secret only from their per-service config. It is also
    /// the fallback for deployment-specific extra services, whose own pull-secret setting
    /// takes precedence. This overrides any `docker_image_pull_secret` set in the applicable
    /// mandatory-service sections.
    #[serde(default)]
    pub docker_image_pull_secret: Option<String>,
    /// Selects how the DPF-managed DPU agent obtains the API trust anchor.
    #[serde(default)]
    pub dpu_agent_bootstrap_ca: DpfDpuAgentBootstrapCa,
    /// Mandatory Helm services to deploy alongside DPF.
    #[serde(default)]
    pub services: Box<DpfMandatoryServicesConfig>,
    /// Deployment-type-specific Helm services.  Extra services are
    /// assigned to a deployment type by the resolver.
    #[serde(default)]
    pub extra_services: Box<DpfExtraServicesConfig>,
    /// Optional proxy configuration for the DPU. When set, containerd on the DPU is
    /// configured to route outbound HTTPS traffic through the specified proxy.
    #[serde(default)]
    pub proxy: Option<DpfProxyDetails>,
    /// `bf.cfg` lines appended to every deployment's DPUFlavor `bfcfgParameters`.
    ///
    /// Each entry is passed through verbatim; NICo applies no quoting or interpretation. The
    /// BFB installer sources `bf.cfg` as shell, so values needing quotes must carry their own:
    /// `extra_bfcfg_parameters = ["ubuntu_PASSWORD='$6$...'"]`.
    ///
    /// Entries containing the Go template delimiter `{{` are rejected at startup, since BF4
    /// Astra renders its DPUFlavorTemplate body and could not pass them through.
    ///
    /// [`DpfDeploymentConfig::extra_bfcfg_parameters`] appends to this list for a single
    /// deployment.
    ///
    /// WARNING: Changing this will generate a new DPUFlavor, reprovisioning every DPU at the
    /// site. `bf.cfg` is applied at install, so that reprovision is also what delivers a
    /// changed value to DPUs that are already installed.
    #[serde(default)]
    pub extra_bfcfg_parameters: Vec<String>,
    /// Per-generation DPUDeployment configurations. BF3 is always present with sensible
    /// defaults; BF4 variants are opt-in.
    #[serde(default)]
    pub deployments: DpfDeploymentsConfig,
}

impl Default for DpfConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            deployment_scoped_service_interfaces: false,
            pf_total_sf_reserved: default_dpf_pf_total_sf_reserved(),
            dpu_service_sync_enabled: default_to_true(),
            docker_image_pull_secret: None,
            dpu_agent_bootstrap_ca: DpfDpuAgentBootstrapCa::default(),
            services: Box::default(),
            extra_services: Box::default(),
            proxy: None,
            extra_bfcfg_parameters: Vec::new(),
            deployments: DpfDeploymentsConfig::default(),
        }
    }
}

impl DpfConfig {
    /// Rejects Astra unless deployment-scoped ServiceInterfaces are enabled.
    ///
    /// Astra is supported only by the BF4+CX9 deployment class and has a
    /// different interface inventory. Legacy global ServiceInterfaces cannot
    /// safely distinguish it from BF3 or generic BF4 nodes.
    pub(crate) fn validate_service_interface_scoping(&self) -> eyre::Result<()> {
        eyre::ensure!(
            self.deployments.bf4_astra.is_none() || self.deployment_scoped_service_interfaces,
            "dpf.deployments.bf4_astra requires \
             dpf.deployment_scoped_service_interfaces=true"
        );
        Ok(())
    }

    /// Returns the top-level mandatory services with the optional
    /// [`Self::docker_image_pull_secret`] override applied. The override affects every
    /// mandatory service except `dts` and `doca_hbn`, which take a pull secret only
    /// from their per-service config.
    pub fn resolved_mandatory_services(&self) -> DpfMandatoryServicesConfig {
        let mut services = (*self.services).clone();
        self.apply_pull_secret_override(&mut services);
        services
    }

    /// Returns the services for `deployment`: the deployment's own
    /// [`DpfDeploymentConfig::services`] override when set, otherwise the top-level
    /// [`Self::services`], plus the extra services supported by `deployment_type`. A
    /// deployment-specific extra-service entry replaces the site-wide definition for that
    /// deployment. The optional
    /// [`Self::docker_image_pull_secret`] override is applied to the mandatory services and,
    /// when an extra service has no per-service secret, to the selected extra services
    /// (see [`Self::resolved_mandatory_services`]).
    pub fn resolved_services_for(
        &self,
        deployment: &DpfDeploymentConfig,
        deployment_type: DpuDeploymentType,
    ) -> DpfResolvedMandatoryServicesConfig {
        let mut base = deployment
            .services
            .as_deref()
            .cloned()
            .unwrap_or_else(|| (*self.services).clone());
        self.apply_pull_secret_override(&mut base);

        let mut extra = self.extra_services.for_deployment_type(deployment_type);
        for extra_service in extra_service_types(deployment_type) {
            if let Some(override_config) = deployment.extra_services.get(extra_service) {
                override_config.apply_to(
                    extra
                        .get_mut(extra_service)
                        .expect("selected extra service must have a site-wide definition"),
                );
            }
        }
        self.apply_extra_pull_secret_override(&mut extra);

        DpfResolvedMandatoryServicesConfig { base, extra }
    }

    /// Returns the site-wide `bf.cfg` parameters followed by `deployment`'s own.
    ///
    /// Appends rather than overrides, unlike [`Self::resolved_services_for`], so a deployment
    /// setting one parameter of its own does not have to restate the site-wide list.
    pub fn resolved_bfcfg_parameters_for(&self, deployment: &DpfDeploymentConfig) -> Vec<String> {
        let mut parameters = self.extra_bfcfg_parameters.clone();
        parameters.extend_from_slice(&deployment.extra_bfcfg_parameters);
        parameters
    }

    /// Applies the optional [`Self::docker_image_pull_secret`] override to every
    /// mandatory service except `dts` and `doca_hbn`, which take a pull secret only
    /// from their per-service config. No-op when the override is unset.
    fn apply_pull_secret_override(&self, services: &mut DpfMandatoryServicesConfig) {
        if let Some(secret) = &self.docker_image_pull_secret {
            let secret = Some(secret.clone());
            services.dpu_agent.docker_image_pull_secret = secret.clone();
            services.dhcp_server.docker_image_pull_secret = secret.clone();
            services.fmds.docker_image_pull_secret = secret.clone();
            services.otel.docker_image_pull_secret = secret;
        }
    }

    /// Uses the top-level pull secret for selected extra services that have not set their own.
    /// A site-wide or deployment-local extra-service secret takes precedence, so one deployment
    /// can pull an Astra image from a different private registry.
    fn apply_extra_pull_secret_override(
        &self,
        services: &mut BTreeMap<DpfExtraService, DpfServiceConfig>,
    ) {
        let Some(secret) = &self.docker_image_pull_secret else {
            return;
        };
        for service in services.values_mut() {
            if service.docker_image_pull_secret.is_none() {
                service.docker_image_pull_secret = Some(secret.clone());
            }
        }
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
#[derive(Clone, Debug, Serialize)]
pub struct DpfMandatoryServicesConfig {
    pub dts: DpfServiceConfig,
    pub doca_hbn: DpfServiceConfig,
    pub dpu_agent: DpfServiceConfig,
    pub dhcp_server: DpfServiceConfig,
    pub fmds: DpfServiceConfig,
    pub otel: DpfServiceConfig,
}

impl<'de> Deserialize<'de> for DpfMandatoryServicesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // `#[serde(default)]` only handles an absent `services` field. For a present,
        // partial table, start with every service default and overlay the supplied fields.
        let configured = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let mut services = Self::default();
        for (name, configured) in configured {
            const SERVICE_FIELDS: &[&str] = &[
                "dts",
                "doca_hbn",
                "dpu_agent",
                "dhcp_server",
                "fmds",
                "otel",
            ];
            let service = match name.as_str() {
                "dts" => &mut services.dts,
                "doca_hbn" => &mut services.doca_hbn,
                "dpu_agent" => &mut services.dpu_agent,
                "dhcp_server" => &mut services.dhcp_server,
                "fmds" => &mut services.fmds,
                "otel" => &mut services.otel,
                _ => return Err(serde::de::Error::unknown_field(&name, SERVICE_FIELDS)),
            };
            let merged = Figment::from(Serialized::defaults(std::mem::take(service)))
                .merge(Serialized::defaults(configured))
                .extract();
            *service = match merged {
                Ok(service) => service,
                Err(error) => match error.kind {
                    figment::error::Kind::UnknownField(field, expected) => {
                        return Err(serde::de::Error::unknown_field(&field, expected));
                    }
                    _ => return Err(serde::de::Error::custom(error)),
                },
            };
        }
        Ok(services)
    }
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

/// Deployment-type-specific service that supplements the mandatory base set.
///
/// Modelled as an enum (rather than a string key) so the resolver that populates
/// the extras and the consumer that projects them into service definitions stay in
/// sync at compile time.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)] // DOCA is part of the service identity, not a redundant prefix.
pub enum DpfExtraService {
    /// DOCA Weave DHCP agent service.
    DocaWeaveDhcpAgent,
    /// DOCA Weave Flow Controller service.
    DocaWeaveFlowController,
    /// DOCA Xplane service.
    DocaXplane,
}

const BF4_ASTRA_EXTRA_SERVICES: &[DpfExtraService] = &[
    DpfExtraService::DocaWeaveDhcpAgent,
    DpfExtraService::DocaWeaveFlowController,
    DpfExtraService::DocaXplane,
];

fn extra_service_types(deployment_type: DpuDeploymentType) -> &'static [DpfExtraService] {
    match deployment_type {
        DpuDeploymentType::Bf3 | DpuDeploymentType::Bf3Gb200 | DpuDeploymentType::Bf4Generic => &[],
        DpuDeploymentType::Bf4Astra => BF4_ASTRA_EXTRA_SERVICES,
    }
}

impl DpfExtraService {
    fn default_config(self) -> DpfServiceConfig {
        match self {
            Self::DocaWeaveDhcpAgent => {
                crate::dpf_services::default_doca_weave_dhcp_agent_service()
            }
            Self::DocaWeaveFlowController => {
                crate::dpf_services::default_doca_weave_flow_controller_service()
            }
            Self::DocaXplane => crate::dpf_services::default_doca_xplane_service(),
        }
    }
}

/// Helm extra services that supplement the mandatory DPF services for specific deployment types.
///
/// The site-wide definitions use the same partial-override rules as
/// [`DpfMandatoryServicesConfig`]. [`DpfConfig::resolved_services_for`]
/// picks the extra services valid for a deployment type, and skips
/// them if the extra service is not valid for a deployment type. For
/// example, configuring a Weave service never adds it to BF3
/// or generic BF4 deployments.
#[derive(Clone, Debug, Serialize)]
pub struct DpfExtraServicesConfig {
    pub doca_weave_dhcp_agent: DpfServiceConfig,
    pub doca_weave_flow_controller: DpfServiceConfig,
    pub doca_xplane: DpfServiceConfig,
}

impl DpfExtraServicesConfig {
    fn for_deployment_type(
        &self,
        deployment_type: DpuDeploymentType,
    ) -> BTreeMap<DpfExtraService, DpfServiceConfig> {
        extra_service_types(deployment_type)
            .iter()
            .map(|service| (*service, self.config_for(*service).clone()))
            .collect()
    }

    fn config_for(&self, service: DpfExtraService) -> &DpfServiceConfig {
        match service {
            DpfExtraService::DocaWeaveDhcpAgent => &self.doca_weave_dhcp_agent,
            DpfExtraService::DocaWeaveFlowController => &self.doca_weave_flow_controller,
            DpfExtraService::DocaXplane => &self.doca_xplane,
        }
    }
}

impl<'de> Deserialize<'de> for DpfExtraServicesConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let configured = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let mut services = Self::default();
        for (name, configured) in configured {
            const SERVICE_FIELDS: &[&str] = &[
                "doca_weave_dhcp_agent",
                "doca_weave_flow_controller",
                "doca_xplane",
            ];
            let service = match name.as_str() {
                "doca_weave_dhcp_agent" => &mut services.doca_weave_dhcp_agent,
                "doca_weave_flow_controller" => &mut services.doca_weave_flow_controller,
                "doca_xplane" => &mut services.doca_xplane,
                _ => return Err(serde::de::Error::unknown_field(&name, SERVICE_FIELDS)),
            };
            let merged = Figment::from(Serialized::defaults(std::mem::take(service)))
                .merge(Serialized::defaults(configured))
                .extract();
            *service = match merged {
                Ok(service) => service,
                Err(error) => match error.kind {
                    figment::error::Kind::UnknownField(field, expected) => {
                        return Err(serde::de::Error::unknown_field(&field, expected));
                    }
                    _ => return Err(serde::de::Error::custom(error)),
                },
            };
        }
        Ok(services)
    }
}

impl Default for DpfExtraServicesConfig {
    fn default() -> Self {
        Self {
            doca_weave_dhcp_agent: DpfExtraService::DocaWeaveDhcpAgent.default_config(),
            doca_weave_flow_controller: DpfExtraService::DocaWeaveFlowController.default_config(),
            doca_xplane: DpfExtraService::DocaXplane.default_config(),
        }
    }
}

/// `DpfResolvedMandatoryServicesConfig` - the compounded list of mandatory services
/// depending on deployment type.
pub struct DpfResolvedMandatoryServicesConfig {
    /// Base mandatory services present for every deployment type.
    pub base: DpfMandatoryServicesConfig,
    /// Deployment-type-specific extra services. Keyed by [`DpfExtraService`] in a
    /// [`BTreeMap`] so iteration order is deterministic.
    pub extra: BTreeMap<DpfExtraService, DpfServiceConfig>,
}

/// Configuration for a single Helm-based DPF service.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// Secret to use to pull the docker images. `None` when the service pulls from
    /// a public registry (`dts` and `doca_hbn` default to this); when set, an
    /// `imagePullSecrets` entry is emitted in the service's Helm values.
    #[serde(default)]
    pub docker_image_pull_secret: Option<String>,
    /// Chart-native values deep-merged over NICo's generated template values.
    /// Tables merge recursively. Scalars and arrays replace generated values.
    #[serde(default)]
    pub extra_helm_values: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Partial deployment-local override for an extra DPF service.
///
/// The resolver applies this after the built-in and site-wide definitions, so a deployment can
/// pin one field such as `helm_version` without repeating its shared registry and image fields.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DpfServiceConfigOverride {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub helm_repo_url: Option<String>,
    #[serde(default)]
    pub helm_chart: Option<String>,
    #[serde(default)]
    pub helm_version: Option<String>,
    #[serde(default)]
    pub docker_repo_url: Option<String>,
    #[serde(default)]
    pub docker_image_tag: Option<String>,
    #[serde(default)]
    pub docker_image_pull_secret: Option<String>,
    #[serde(default)]
    pub extra_helm_values: Option<serde_json::Map<String, serde_json::Value>>,
}

impl DpfServiceConfigOverride {
    fn apply_to(&self, config: &mut DpfServiceConfig) {
        if let Some(value) = &self.name {
            config.name.clone_from(value);
        }
        if let Some(value) = &self.helm_repo_url {
            config.helm_repo_url.clone_from(value);
        }
        if let Some(value) = &self.helm_chart {
            config.helm_chart.clone_from(value);
        }
        if let Some(value) = &self.helm_version {
            config.helm_version.clone_from(value);
        }
        if let Some(value) = &self.docker_repo_url {
            config.docker_repo_url.clone_from(value);
        }
        if let Some(value) = &self.docker_image_tag {
            config.docker_image_tag.clone_from(value);
        }
        if let Some(value) = &self.docker_image_pull_secret {
            config.docker_image_pull_secret = Some(value.clone());
        }
        if let Some(value) = &self.extra_helm_values {
            config.extra_helm_values = Some(value.clone());
        }
    }
}

/// Per-deployment DPF configuration for named entries under `[dpf.deployments]`.
///
/// `flavor_name`, `deployment_name`, and `node_label_key` are required when a
/// `[dpf.deployments.<name>]` block is written; `bfb_url` and `services` are
/// optional. When `services` is omitted, the deployment inherits the top-level
/// `[dpf.services]` (see [`DpfConfig::resolved_services_for`]). Extra services
/// are configured per deployment in `extra_services`.
///
/// The `Default` impl (BF3 values) is used when the entire
/// `[dpf.deployments.bf3]` block is absent, via `#[serde(default)]` on the
/// `bf3` field of [`DpfDeploymentsConfig`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DpfDeploymentConfig {
    /// URL to the BlueField firmware bundle (BFB) for DPU provisioning
    /// (BF3-class DPUs). Exactly one of `bfb_url` or `bluefield_software`
    /// must be set per deployment (see
    /// [`DpfDeploymentsConfig::validate_provisioning_sources`]).
    #[serde(default)]
    pub bfb_url: Option<String>,
    /// BlueFieldSoftware spec for BF4-class DPUs. When set, a `BlueFieldSoftware`
    /// CR is created and referenced by the DPUDeployment instead of a BFB.
    /// Mutually exclusive with `bfb_url`.
    #[serde(default)]
    pub bluefield_software: Option<DpfBlueFieldSoftwareConfig>,
    /// Kubernetes DPUFlavor CR name.
    pub flavor_name: String,
    /// Kubernetes DPUDeployment CR name.
    pub deployment_name: String,
    /// Label key applied to DPUNode CRs for this deployment's node selector.
    pub node_label_key: String,
    /// Optional per-deployment override of the mandatory Helm services. When set,
    /// these services are deployed for this deployment instead of the top-level
    /// [`DpfConfig::services`]. When absent, the top-level services are inherited.
    #[serde(default)]
    pub services: Option<Box<DpfMandatoryServicesConfig>>,

    /// Deployment-specific Helm services. BF4 Astra receives built-in DOCA Weave
    /// DHCP agent, Weave flow controller, and DOCA Xplane definitions; configured
    /// entries overlay the corresponding site-wide extra-service definition.
    #[serde(default)]
    pub extra_services: BTreeMap<DpfExtraService, DpfServiceConfigOverride>,

    /// `bf.cfg` lines for this deployment's DPUFlavor only, for parameters that apply to one
    /// DPU generation but not the others.
    ///
    /// Appends to the site-wide [`DpfConfig::extra_bfcfg_parameters`]; it does not replace it,
    /// unlike `services`. See [`DpfConfig::resolved_bfcfg_parameters_for`].
    ///
    /// WARNING: Changing this will generate a new DPUFlavor, reprovisioning this deployment's
    /// DPUs.
    #[serde(default)]
    pub extra_bfcfg_parameters: Vec<String>,
}

impl Default for DpfDeploymentConfig {
    fn default() -> Self {
        Self {
            bfb_url: Some(default_dpf_bfb_url()),
            bluefield_software: None,
            flavor_name: default_dpf_flavor_name(),
            deployment_name: default_dpf_deployment_name(),
            node_label_key: default_dpf_node_label_key(),
            services: None,
            extra_services: BTreeMap::new(),
            extra_bfcfg_parameters: Vec::new(),
        }
    }
}

impl DpfDeploymentConfig {
    /// Derives the GB200 BF3 deployment from the configured BF3 source and services.
    pub(crate) fn bf3_gb200(&self) -> Self {
        Self {
            flavor_name: append_bounded_identifier_suffix(
                &self.flavor_name,
                GB200_RESOURCE_SUFFIX,
                KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH - DPF_FLAVOR_HASH_SUFFIX_LENGTH,
            ),
            deployment_name: append_bounded_identifier_suffix(
                &self.deployment_name,
                GB200_DEPLOYMENT_SUFFIX,
                DPF_DEPLOYMENT_NAME_MAX_LENGTH,
            ),
            node_label_key: append_gb200_label_suffix(&self.node_label_key),
            ..self.clone()
        }
    }
}

/// Appends `suffix` while reserving its bytes inside `max_len`.
///
/// Kubernetes identifiers are ASCII. If truncation lands on a separator,
/// remove it so the suffix still ends a valid segment. Startup validation
/// checks both the configured and derived identifiers after this step.
fn append_bounded_identifier_suffix(value: &str, suffix: &str, max_len: usize) -> String {
    let max_prefix_len = max_len.saturating_sub(suffix.len());
    let prefix = value
        .char_indices()
        .take_while(|(index, ch)| index + ch.len_utf8() <= max_prefix_len)
        .map(|(_, ch)| ch)
        .collect::<String>();
    let prefix = prefix.trim_end_matches(['-', '.', '_']);

    format!("{prefix}{suffix}")
}

/// Adds the GB200 suffix to the name segment of a Kubernetes label key.
fn append_gb200_label_suffix(label_key: &str) -> String {
    let Some((prefix, name)) = label_key.rsplit_once('/') else {
        return append_bounded_identifier_suffix(
            label_key,
            GB200_RESOURCE_SUFFIX,
            KUBERNETES_LABEL_NAME_MAX_LENGTH,
        );
    };
    let name = append_bounded_identifier_suffix(
        name,
        GB200_RESOURCE_SUFFIX,
        KUBERNETES_LABEL_NAME_MAX_LENGTH,
    );

    format!("{prefix}/{name}")
}

/// BlueFieldSoftware spec for BF4-class DPU provisioning. Mirrors the `spec` of
/// the `provisioning.dpu.nvidia.com/v1alpha1` `BlueFieldSoftware` CR.
///
/// The PLDM firmware bundle is PSID-specific, so `pldm_fw_bundle` maps each PSID
/// to its bundle URL. One `BlueFieldSoftware` CR and one DPUDeployment are
/// created per PSID (see
/// [`DpfDeploymentConfig::per_psid_deployment_name`] and
/// [`DpfDeploymentConfig::per_psid_node_label_key`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DpfBlueFieldSoftwareConfig {
    /// OS ISO URL used by the DPU OS installation flow (`spec.osIso`). Shared
    /// across all PSIDs.
    pub os_iso: String,
    /// Map of PSID → PLDM firmware bundle URL (`spec.pldmFwBundle`). Each entry
    /// fans out to its own `BlueFieldSoftware` CR and DPUDeployment.
    #[serde(default)]
    pub pldm_fw_bundle: BTreeMap<String, String>,
}

/// Named DPUDeployment configurations under `[dpf.deployments]`.
/// Each entry creates its own provisioning source, DPUFlavor, and DPUDeployment
/// CR at startup.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DpfDeploymentsConfig {
    /// BF3 deployment. Present by default with sensible values; override individual
    /// fields in `[dpf.deployments.bf3]` when the site uses non-default names or BFBs.
    #[serde(default)]
    pub bf3: DpfDeploymentConfig,
    /// BF4 generic deployment (NICo + BF4 via DPF).
    #[serde(default)]
    pub bf4_generic: Option<DpfDeploymentConfig>,
    /// BF4 astra deployment (NICo + BF4 with Astra via DPF)
    #[serde(default)]
    pub bf4_astra: Option<DpfDeploymentConfig>,
}

impl DpfDeploymentsConfig {
    /// Returns all active deployment configs as `(name, config)` pairs.
    /// Add new deployments here when they are introduced.
    fn all(&self) -> Vec<(&'static str, &DpfDeploymentConfig)> {
        let mut v = vec![("bf3", &self.bf3)];
        if let Some(bf4) = &self.bf4_generic {
            v.push(("bf4_generic", bf4));
        }
        if let Some(bf4_astra) = &self.bf4_astra {
            v.push(("bf4_astra", bf4_astra));
        }
        v
    }

    /// Validates the final Kubernetes identifiers, their uniqueness, and reserved labels.
    /// `DPU_ENABLED_NODE_LABEL` and `HOST_BMC_IP_LABEL` are reserved for the
    /// shared DPF node marker and host BMC IP. Neither can be used as the
    /// `node_label_key` for an individual deployment.
    /// Returns every error so the operator can fix them all in one pass.
    pub fn validate_unique_identifiers(&self) -> eyre::Result<()> {
        let bf3_gb200 = self.bf3.bf3_gb200();
        let mut deployments = self.all();
        deployments.push(("bf3_gb200", &bf3_gb200));
        let mut errors: Vec<String> = Vec::new();

        let name_vals: Vec<(&str, &str)> = deployments
            .iter()
            .map(|(n, c)| (*n, c.deployment_name.as_str()))
            .collect();
        let flavor_vals: Vec<(&str, &str)> = deployments
            .iter()
            .map(|(n, c)| (*n, c.flavor_name.as_str()))
            .collect();
        let label_vals: Vec<(&str, &str)> = deployments
            .iter()
            .map(|(n, c)| (*n, c.node_label_key.as_str()))
            .collect();
        let checks = [
            ("deployment_name", &name_vals),
            ("flavor_name", &flavor_vals),
            ("node_label_key", &label_vals),
        ];
        for (field, values) in &checks {
            let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
            for (name, value) in values.iter() {
                if let Some(prev) = seen.insert(value, name) {
                    errors.push(format!(
                        "{field} {value:?} is shared by deployments {prev:?} and {name:?}"
                    ));
                }
            }
        }

        for (deployment, name) in &name_vals {
            if name.len() > DPF_DEPLOYMENT_NAME_MAX_LENGTH || !is_valid_kubernetes_object_name(name)
            {
                errors.push(format!(
                    "deployment_name {name:?} for deployment {deployment:?} is not a valid DPUDeployment name"
                ));
            }
        }

        for (deployment, name) in &flavor_vals {
            if name.len() + DPF_FLAVOR_HASH_SUFFIX_LENGTH > KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH
                || !is_valid_kubernetes_object_name(name)
            {
                errors.push(format!(
                    "flavor_name {name:?} for deployment {deployment:?} cannot form a valid hash-suffixed DPUFlavor name"
                ));
            }
        }

        // This is intentionally a local configuration check. Querying current DPUNode labels
        // cannot establish safety: these keys have fixed NICo semantics before any node exists.
        for (deployment, label_key) in &label_vals {
            if !is_valid_kubernetes_label_key(label_key) {
                errors.push(format!(
                    "node_label_key {label_key:?} for deployment {deployment:?} is not a valid Kubernetes label key"
                ));
            }
            if let Some((_, purpose)) = RESERVED_DPF_DEPLOYMENT_NODE_LABELS
                .iter()
                .find(|(reserved, _)| label_key == reserved)
            {
                errors.push(format!(
                    "node_label_key {label_key:?} for deployment {deployment:?} is reserved for {purpose}"
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(eyre::eyre!(
                "DPF deployment configuration has invalid identifiers:\n  - {}",
                errors.join("\n  - ")
            ))
        }
    }

    /// Validates that each active deployment specifies exactly one provisioning
    /// source: either `bfb_url` (BF3) or `bluefield_software` (BF4), never both
    /// and never neither. This mirrors the DPUDeployment CRD rule requiring
    /// exactly one of `spec.dpus.bfb` / `spec.dpus.blueFieldSoftware`. Returns an
    /// error listing every offending deployment so they can be fixed in one pass.
    ///
    /// Additionally enforces the hard rule that the `bf3` deployment is BFB-only:
    /// it must use `bfb_url` and must never set `bluefield_software` (BF4-only).
    pub fn validate_provisioning_sources(&self) -> eyre::Result<()> {
        let mut errors: Vec<String> = Vec::new();

        // BF3 is BFB-only. `bluefield_software` is BF4-specific and is never
        // valid on the bf3 deployment, regardless of whether bfb_url is also set.
        if self.bf3.bluefield_software.is_some() {
            errors.push(
                "deployment \"bf3\" must not set bluefield_software; BF3 uses bfb_url only"
                    .to_string(),
            );
        }

        // BF4 is BlueFieldSoftware-only. `bfb_url` is BF3-specific; bf4_generic and
        // bf4_astra deployments must use `bluefield_software`. Reject the BFB-only case
        // here so it fails at config validation rather than later at SDK startup.
        for (name, cfg) in [
            ("bf4_generic", self.bf4_generic.as_ref()),
            ("bf4_astra", self.bf4_astra.as_ref()),
        ] {
            if cfg.is_some_and(|c| c.bfb_url.is_some() && c.bluefield_software.is_none()) {
                errors.push(format!(
                    "deployment \"{name}\" must set bluefield_software; BF4 does not support bfb_url"
                ));
            }
        }

        for (name, cfg) in self.all() {
            match (&cfg.bfb_url, &cfg.bluefield_software) {
                (Some(_), Some(_)) => errors.push(format!(
                    "deployment {name:?} sets both bfb_url and bluefield_software; set exactly one"
                )),
                (None, None) => errors.push(format!(
                    "deployment {name:?} sets neither bfb_url nor bluefield_software; set exactly one"
                )),
                // Exactly one PSID entry is allowed for now. Multi-PSID support
                // is pending a DPF change that lets one `BlueFieldSoftware` CR
                // carry a PSID→PLDM map; until then a single BF4 deployment uses
                // the one entry's PLDM bundle.
                (None, Some(bfs)) if bfs.pldm_fw_bundle.len() != 1 => errors.push(format!(
                    "deployment {name:?} bluefield_software.pldm_fw_bundle must have exactly one \
                     PSID → PLDM bundle URL entry (found {}).",
                    bfs.pldm_fw_bundle.len()
                )),
                _ => {}
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(eyre::eyre!(
                "DPF deployment configuration has invalid provisioning sources:\n  - {}",
                errors.join("\n  - ")
            ))
        }
    }
}

/// Machine identity (SPIFFE JWT-SVID) configuration.
/// Loaded from `[machine_identity]` section in config.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Node-auth (Scout / DPU-agent bearer JWT) configuration.
/// Loaded from `[node_auth]` section in config.
///
/// There is no server-side signing key: nodes self-sign tokens with their
/// existing mTLS client-certificate key and the API validates the embedded
/// `x5c` chain against the client-cert root CA (see
/// `docs/design/machine-identity/node-auth-jwt.md`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeAuthConfig {
    /// Master switch. When false, no bearer authenticator is installed and
    /// nodes keep authenticating via mTLS client certs only.
    #[serde(default = "node_auth_default_enabled")]
    pub enabled: bool,
    /// Maximum accepted token lifetime, in seconds. Nodes mint 5-minute
    /// tokens; this bounds how far a (compromised) client can stretch `exp`.
    #[serde(default = "node_auth_default_max_token_ttl_sec")]
    pub max_token_ttl_sec: u32,
    /// Whether machine mTLS client certificates are accepted as node identity.
    /// On by default. Scoped to MACHINE certs only — service and admin-CLI
    /// client certs on the same listener are unaffected. Disable only once the
    /// fleet presents bearer tokens.
    #[serde(default = "node_auth_default_mtls_enabled")]
    pub mtls_enabled: bool,
    /// Whether DPF-deployed fmds is rendered in token mode, overriding the
    /// value otherwise derived from [`enabled`](Self::enabled).
    ///
    /// Unset (the default) means "follow `enabled`", which is what a site
    /// wants almost always. The override exists because the two halves of a
    /// change do not land at the same time: the API stops accepting bearer
    /// tokens the moment it restarts, while fmds keeps presenting them until
    /// DPF has rolled every DaemonSet. Without a separate knob there is no way
    /// to order those steps, so disabling node-auth necessarily opens a window
    /// where fmds is authenticating with a credential the API no longer takes.
    ///
    /// Setting it to `false` while `enabled` is still `true` moves fmds back
    /// to client certificates first; once that roll has landed, `enabled` can
    /// be turned off with nothing depending on tokens. See "Disabling
    /// node-auth" in `docs/design/machine-identity/node-auth-jwt.md`.
    ///
    /// `true` with `enabled = false` is rejected at startup: fmds would
    /// present tokens to an API that refuses them, which is the outage the
    /// override exists to prevent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmds_use_node_tokens: Option<bool>,
}

/// Upper bound on accepted token lifetime. Node tokens are minted locally on
/// demand, so anything beyond a day is almost certainly a misconfiguration.
pub const NODE_AUTH_MAX_TOKEN_TTL_SEC: u32 = 86_400;

impl NodeAuthConfig {
    /// Whether DPF-deployed fmds should be rendered in token mode.
    ///
    /// The override when set, otherwise [`enabled`](Self::enabled). Call this
    /// rather than reading `enabled` directly wherever fmds helm values are
    /// built, so the staging path stays available.
    #[must_use]
    pub fn fmds_use_node_tokens(&self) -> bool {
        self.fmds_use_node_tokens.unwrap_or(self.enabled)
    }

    /// Validates node-auth settings. Call unconditionally at startup: the
    /// lockout check applies even when [`enabled`](Self::enabled) is false.
    ///
    /// That check assumes a TLS listener. On a plaintext `listen_mode` the
    /// accept path yields no peer certificates, so `mtls_enabled = true`
    /// satisfies it while authenticating nobody; only the bearer half of the
    /// dependency is enforced here, because refusing plaintext outright would
    /// break local development for a mode nothing ships.
    pub fn validate(&self) -> eyre::Result<()> {
        if !self.enabled && !self.mtls_enabled {
            return Err(eyre::eyre!(
                "[node_auth] enabled = false and mtls_enabled = false would leave nodes with no \
                 way to authenticate; enable at least one of bearer tokens or machine mTLS"
            ));
        }
        // Checked before the `enabled` early-return below: this combination is
        // precisely the one where `enabled` is false, and it would deploy fmds
        // to present tokens the API refuses.
        if self.fmds_use_node_tokens == Some(true) && !self.enabled {
            return Err(eyre::eyre!(
                "[node_auth] fmds_use_node_tokens = true requires enabled = true; fmds would \
                 present bearer tokens to an API that does not accept them"
            ));
        }
        if !self.enabled {
            // Remaining checks only constrain token validation.
            return Ok(());
        }
        if self.max_token_ttl_sec == 0 {
            return Err(eyre::eyre!(
                "[node_auth] max_token_ttl_sec must be greater than zero"
            ));
        }
        // A cap below what the shipped clients mint accepts at startup and then
        // rejects every token the fleet presents -- and with mtls_enabled =
        // false that is a total lockout, with nothing naming the setting. The
        // clients mint a fixed lifetime, so the cap has to clear it.
        if u64::from(self.max_token_ttl_sec) < ::rpc::node_jwt::NODE_JWT_TTL_SECS {
            return Err(eyre::eyre!(
                "[node_auth] max_token_ttl_sec {} is below the {} s lifetime node clients \
                 mint, so every token they present would be rejected",
                self.max_token_ttl_sec,
                ::rpc::node_jwt::NODE_JWT_TTL_SECS
            ));
        }
        if self.max_token_ttl_sec > NODE_AUTH_MAX_TOKEN_TTL_SEC {
            return Err(eyre::eyre!(
                "[node_auth] max_token_ttl_sec {} exceeds maximum {NODE_AUTH_MAX_TOKEN_TTL_SEC}",
                self.max_token_ttl_sec
            ));
        }
        Ok(())
    }
}

fn node_auth_default_enabled() -> bool {
    false
}
fn node_auth_default_max_token_ttl_sec() -> u32 {
    900
}
fn node_auth_default_mtls_enabled() -> bool {
    true
}

impl Default for NodeAuthConfig {
    fn default() -> Self {
        Self {
            enabled: node_auth_default_enabled(),
            max_token_ttl_sec: node_auth_default_max_token_ttl_sec(),
            mtls_enabled: node_auth_default_mtls_enabled(),
            // Unset: follow `enabled`. Only a site staging a change sets it.
            fmds_use_node_tokens: None,
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
#[serde(deny_unknown_fields)]
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

/// Fabric Nearest Neighbor (FNN) configuration for L3 VNI-based overlay networking.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
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

/// A named routing-profile definition whose unset properties use effective
/// defaults unless a VPC supplies an inline override.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct FnnRoutingProfileConfig {
    /// These are used for import policies to import routes
    /// that match these targets.
    #[serde(default)]
    pub route_target_imports: Option<Vec<RouteTargetConfig>>,

    /// These are used for tagging routes exported by the DPU
    #[serde(default)]
    pub route_targets_on_exports: Option<Vec<RouteTargetConfig>>,

    /// Is this an internal or external tenant/VPC profile
    #[serde(default)]
    pub internal: Option<bool>,

    /// Opts VPCs based on this profile into exact tenant VpcPrefix reuse checks.
    ///
    /// This base-profile setting defaults to `false` and cannot be overridden
    /// by a VPC. The complete eligibility, rejection, and database fallback
    /// contract is documented under "Tenant prefix overlap checks" in
    /// `crates/api-core/src/cfg/README.md`.
    #[serde(default)]
    pub tenant_prefix_overlap_eligible: bool,

    /// Should DPUs leak the default route from the
    /// underlay into the tenant VRF?
    #[serde(default)]
    pub leak_default_route_from_underlay: Option<bool>,

    /// Should DPUs leak the routes for the host IPs into
    /// into the underlay?
    #[serde(default)]
    pub leak_tenant_host_routes_to_underlay: Option<bool>,

    /// Are route-leak communities sent by the host OS honored by the DPU for allowing
    /// routes advertised by the host OS to be leaked into the underlay?
    #[serde(default)]
    pub tenant_leak_communities_accepted: Option<bool>,

    /// An explicit/granular list of prefixes that should
    /// be allowed to leak from the default VRF into the tenant
    /// VRF.
    ///
    /// These are purely for routing purposes and will not have any
    /// impact on ACLs.
    #[serde(default)]
    pub accepted_leaks_from_underlay: Option<Vec<PrefixFilterPolicyEntry>>,

    /// Prefixes that tenant hosts are allowed to announce
    /// to the DPU as anycast routes.
    #[serde(default)]
    pub allowed_anycast_prefixes: Option<Vec<PrefixFilterPolicyEntry>>,

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
    pub access_tier: Option<u32>,
}

impl FnnRoutingProfileConfig {
    /// `is_eligible_for_tenant_prefix_overlap` returns whether the resolved
    /// profile meets every profile condition for exact prefix reuse.
    ///
    /// Evaluate the profile returned by [`FnnConfig::resolve_vpc_routing_profile`],
    /// not the raw base profile, so VPC overrides participate in the decision.
    /// The caller adds the site-wide conditions for these prefix writers.
    /// Peering and VPC policy changes are tracked in
    /// <https://github.com/NVIDIA/infra-controller/issues/5114>, and retained
    /// Instance paths are tracked in
    /// <https://github.com/NVIDIA/infra-controller/issues/5115>. Startup and
    /// complete writer coverage are tracked in
    /// <https://github.com/NVIDIA/infra-controller/issues/5116>. All three must
    /// land before the database cutover in
    /// <https://github.com/NVIDIA/infra-controller/issues/3892>.
    pub(crate) fn is_eligible_for_tenant_prefix_overlap(&self) -> bool {
        // Keep this exhaustive so new profile fields require an explicit eligibility decision.
        let Self {
            tenant_prefix_overlap_eligible,
            route_target_imports,
            route_targets_on_exports,
            // External profiles cannot participate in exact prefix reuse.
            internal,
            leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted,
            accepted_leaks_from_underlay,
            allowed_anycast_prefixes,
            // Access tiers control profile selection, not rendered routes.
            access_tier: _,
        } = self;

        *tenant_prefix_overlap_eligible
            && *internal == Some(true)
            && route_target_imports.as_ref().is_none_or(Vec::is_empty)
            && route_targets_on_exports.as_ref().is_none_or(Vec::is_empty)
            && !leak_default_route_from_underlay.unwrap_or_default()
            && !leak_tenant_host_routes_to_underlay.unwrap_or_default()
            && !tenant_leak_communities_accepted.unwrap_or_default()
            && accepted_leaks_from_underlay
                .as_ref()
                .is_none_or(Vec::is_empty)
            && allowed_anycast_prefixes.as_ref().is_none_or(Vec::is_empty)
    }
}

impl FnnConfig {
    /// Resolves the named routing profile and applies properties set on the VPC.
    pub(crate) fn resolve_vpc_routing_profile(
        &self,
        vpc: &VpcConfig,
    ) -> Result<Cow<'_, FnnRoutingProfileConfig>, CarbideError> {
        let profile_type =
            vpc.routing_profile_type
                .as_ref()
                .ok_or_else(|| CarbideError::Internal {
                    message: "tenant routing profile type not found in VPC record".to_string(),
                })?;
        let base_profile =
            self.routing_profiles
                .get(profile_type)
                .ok_or_else(|| CarbideError::NotFoundError {
                    kind: "routing_profile_type",
                    id: profile_type.to_string(),
                })?;

        // Apply properties explicitly set on the VPC over the named base profile.
        let Some(overrides) = vpc.routing_profile_overrides.as_ref() else {
            return Ok(Cow::Borrowed(base_profile));
        };

        Ok(Cow::Owned(FnnRoutingProfileConfig {
            route_target_imports: overrides
                .route_target_imports
                .clone()
                .or_else(|| base_profile.route_target_imports.clone()),
            route_targets_on_exports: overrides
                .route_targets_on_exports
                .clone()
                .or_else(|| base_profile.route_targets_on_exports.clone()),
            leak_default_route_from_underlay: overrides
                .leak_default_route_from_underlay
                .or(base_profile.leak_default_route_from_underlay),
            leak_tenant_host_routes_to_underlay: overrides
                .leak_tenant_host_routes_to_underlay
                .or(base_profile.leak_tenant_host_routes_to_underlay),
            tenant_leak_communities_accepted: overrides
                .tenant_leak_communities_accepted
                .or(base_profile.tenant_leak_communities_accepted),
            accepted_leaks_from_underlay: overrides
                .accepted_leaks_from_underlay
                .clone()
                .or_else(|| base_profile.accepted_leaks_from_underlay.clone()),
            allowed_anycast_prefixes: overrides
                .allowed_anycast_prefixes
                .clone()
                .or_else(|| base_profile.allowed_anycast_prefixes.clone()),
            // These base-profile properties cannot be overridden on a VPC.
            tenant_prefix_overlap_eligible: base_profile.tenant_prefix_overlap_eligible,
            internal: base_profile.internal,
            access_tier: base_profile.access_tier,
        }))
    }
}

impl From<&FnnRoutingProfileConfig> for rpc::forge::RoutingProfile {
    fn from(profile: &FnnRoutingProfileConfig) -> Self {
        Self {
            tenant_leak_communities_accepted: profile
                .tenant_leak_communities_accepted
                .unwrap_or_default(),
            leak_default_route_from_underlay: profile
                .leak_default_route_from_underlay
                .unwrap_or_default(),
            leak_tenant_host_routes_to_underlay: profile
                .leak_tenant_host_routes_to_underlay
                .unwrap_or_default(),
            accepted_leaks_from_underlay: profile
                .accepted_leaks_from_underlay
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|entry| rpc::forge::PrefixFilterPolicyEntry {
                    prefix: entry.prefix.to_string(),
                })
                .collect(),
            allowed_anycast_prefixes: profile
                .allowed_anycast_prefixes
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|entry| rpc::forge::PrefixFilterPolicyEntry {
                    prefix: entry.prefix.to_string(),
                })
                .collect(),
            route_target_imports: profile
                .route_target_imports
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|route_target| rpc::common::RouteTarget {
                    asn: route_target.asn,
                    vni: route_target.vni,
                })
                .collect(),
            route_targets_on_exports: profile
                .route_targets_on_exports
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|route_target| rpc::common::RouteTarget {
                    asn: route_target.asn,
                    vni: route_target.vni,
                })
                .collect(),
        }
    }
}

impl From<&FnnRoutingProfileConfig> for rpc::forge::VpcEffectiveRoutingProfile {
    fn from(profile: &FnnRoutingProfileConfig) -> Self {
        let routing_profile = rpc::forge::RoutingProfile::from(profile);
        Self {
            route_target_imports: routing_profile.route_target_imports,
            route_targets_on_exports: routing_profile.route_targets_on_exports,
            leak_default_route_from_underlay: routing_profile.leak_default_route_from_underlay,
            leak_tenant_host_routes_to_underlay: routing_profile
                .leak_tenant_host_routes_to_underlay,
            tenant_leak_communities_accepted: routing_profile.tenant_leak_communities_accepted,
            accepted_leaks_from_underlay: routing_profile.accepted_leaks_from_underlay,
            allowed_anycast_prefixes: routing_profile.allowed_anycast_prefixes,
            internal: profile.internal.unwrap_or_default(),
            access_tier: profile.access_tier.unwrap_or_default(),
        }
    }
}

/// FNN configuration specific to the admin network.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
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
    /// Which configuration keys were explicitly provided by the merged
    /// sources, mapped to source labels — see [`super::provenance`]. Empty
    /// for configs that weren't produced by `parse_carbide_config` (test
    /// fixtures, programmatic construction).
    pub fn explicit_value_paths(&self) -> BTreeMap<String, String> {
        self.config_ctx
            .as_ref()
            .map(super::provenance::explicit_value_paths)
            .unwrap_or_default()
    }

    /// Returns a version of CarbideConfig where secrets are erased
    pub fn redacted(&self) -> Self {
        let mut config = self.clone();
        if let Some(host_index) = config.database_url.find('@') {
            let host = config.database_url.split_at(host_index).1;
            config.database_url = format!("postgres://redacted{host}");
        }
        // The dedicated certificate Vault's root token is a secret; the
        // redacted config is serialized to JSON for the admin API and config
        // dumps, so drop it before it leaves the process.
        if let Some(dedicated) = config.certificates.dedicated_vault.as_mut()
            && dedicated.token.is_some()
        {
            dedicated.token = Some("redacted".to_string());
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

    pub fn is_ewethers_enabled(&self) -> bool {
        let Some(conf) = &self.ewethers_config else {
            return false;
        };

        conf.enabled
    }

    pub fn is_svpc_enabled(&self) -> bool {
        let Some(conf) = &self.ewethers_config else {
            return false;
        };

        conf.svpc_enabled
    }

    pub fn is_astra_enabled(&self) -> bool {
        let Some(conf) = &self.ewethers_config else {
            return false;
        };

        conf.astra_enabled
    }

    pub fn get_dpa_subnet_ip(&self) -> Result<Ipv4Addr, eyre::Report> {
        let Some(conf) = &self.ewethers_config else {
            tracing::error!("get_dpa_subnet_ip: DPA config missing");
            return Err(eyre::eyre!("get_dpa_subnet_ip: DPA config missing"));
        };

        Ok(conf.subnet_ip)
    }

    pub fn get_dpa_subnet_mask(&self) -> Result<i32, eyre::Report> {
        let Some(conf) = &self.ewethers_config else {
            tracing::error!("get_dpa_subnet_mask: DPA config missing");
            return Err(eyre::eyre!("get_dpa_subnet_mask: DPA config missing"));
        };

        Ok(conf.subnet_mask)
    }

    pub fn mqtt_broker_host(&self) -> Option<String> {
        self.ewethers_config
            .as_ref()
            .map(|conf| conf.svpc.mqtt_endpoint.clone())
    }

    pub fn mqtt_broker_port(&self) -> Option<u16> {
        self.ewethers_config
            .as_ref()
            .map(|conf| conf.svpc.mqtt_broker_port)
    }

    pub fn get_hb_interval(&self) -> Option<chrono::TimeDelta> {
        self.ewethers_config
            .as_ref()
            .map(|conf| conf.svpc.hb_interval)
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

/// Extension-service state-controller configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ExtensionServiceStateControllerConfig {
    /// Common state-controller configuration.
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// IbPartitionStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IbPartitionStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// DpaInterfaceStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DpaInterfaceStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

/// PowerShelfStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PowerShelfStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,

    /// When `true`, the power shelf Ready handler accepts rack-level
    /// `power_shelf_reprovisioning_requested` and enters
    /// `ReProvisioning::WaitingForRackFirmwareUpgrade`.
    ///
    /// Defaults to `false` so power shelves stay out of rack firmware wait
    /// unless explicitly enabled.
    ///
    /// Configured in `nico-api-config.toml`:
    ///
    /// ```toml
    /// [power_shelf_state_controller]
    /// rack_firmware_reprovisioning_enabled = true
    /// ```
    #[serde(default)]
    pub rack_firmware_reprovisioning_enabled: bool,
}

/// RackStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RackStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,

    /// Switch mTLS services for NMX cluster setup. Accepted and ignored: rack
    /// maintenance does not configure switch certificates. Per-switch
    /// certificate configuration uses
    /// `[switch_state_controller].switch_mtls_services`.
    #[serde(default)]
    pub nmx_cluster_switch_mtls_services: Vec<component_manager::config::SwitchMtlsService>,
}

impl RackStateControllerConfig {
    /// Returns configured NMX cluster switch mTLS services, or the ScaleUpFabric
    /// defaults when the field was omitted or left empty in config.
    pub fn effective_nmx_cluster_switch_mtls_services_as_i32(&self) -> Vec<i32> {
        component_manager::config::switch_mtls_services_as_i32(
            &component_manager::config::effective_nmx_cluster_switch_mtls_services(
                &self.nmx_cluster_switch_mtls_services,
            ),
        )
    }
}

/// SwitchStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SwitchStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,

    /// Switch services that receive installed mTLS certificates during RMS
    /// `configure_switch_certificate` calls initiated by the switch state
    /// machine or the direct `ComponentConfigureSwitchCertificate` RPC path.
    ///
    /// When this field is omitted or empty, all supported services are used.
    ///
    /// Configured in `nico-api-config.toml`:
    ///
    /// ```toml
    /// [switch_state_controller]
    /// switch_mtls_services = [
    ///   "nvue_api",
    ///   "scale_up_fabric_telemetry",
    /// ]
    /// ```
    #[serde(default)]
    pub switch_mtls_services: Vec<component_manager::config::SwitchMtlsService>,
}

impl SwitchStateControllerConfig {
    /// Returns the configured switch mTLS services, or all supported services
    /// when the field was omitted or left empty in config.
    pub fn effective_switch_mtls_services_as_i32(&self) -> Vec<i32> {
        component_manager::config::switch_mtls_services_as_i32(
            &component_manager::config::effective_switch_mtls_services(&self.switch_mtls_services),
        )
    }
}

/// SpdmStateController related config
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpdmStateControllerConfig {
    /// Common state controller configs
    #[serde(default = "StateControllerConfig::default")]
    pub controller: StateControllerConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

pub const fn default_database_pool_acquire_timeout() -> std::time::Duration {
    // sqlx's own default; exposing the setting changes no behavior.
    std::time::Duration::from_secs(30)
}

pub const fn default_database_pool_idle_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(10 * 60)
}

pub const fn default_database_pool_max_lifetime() -> std::time::Duration {
    std::time::Duration::from_secs(30 * 60)
}

const fn default_api_admission_max_work_in_flight() -> usize {
    64
}

const fn default_api_admission_max_pending() -> usize {
    1024
}

const fn default_api_admission_max_work_in_flight_per_client() -> usize {
    8
}

const fn default_api_admission_max_pending_per_client() -> usize {
    64
}

const fn default_api_admission_pending_timeout() -> std::time::Duration {
    // This is intentionally half of the normal ten-second client timeout,
    // leaving roughly half of the deadline for handler execution and response
    // delivery. Keep the ratio fixed until scale data justifies another knob.
    std::time::Duration::from_secs(5)
}

const fn default_api_admission_client_idle_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(5 * 60)
}

pub const fn default_bmc_session_lockout_threshold() -> u32 {
    3
}

pub const fn default_bmc_max_sessions_per_caller() -> usize {
    4
}

/// DpuConfig related internal configuration
#[derive(Clone, Debug, Serialize)]
pub struct DpuConfig {
    /// How booting DPUs obtain the CA used to authenticate Carbide.
    #[serde(default)]
    pub bootstrap_ca_source: BootstrapCaSource,

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

    /// Number of deterministic HBN interfaces reserved for service-VPC attachments.
    #[serde(default)]
    pub service_vpc_slot_count: u32,

    /// Additional SF capacity that is not assigned to an HBN interface.
    #[serde(default)]
    pub additional_managed_sf: u32,

    /// Restart OVS on DPU agents whenever the host switches between
    /// admin and tenant networking. Required in some environments to
    /// ensure OVS picks up the changed network configuration.
    #[serde(default)]
    pub restart_ovs_on_use_admin_network_change: bool,
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
        #[serde(deny_unknown_fields)]
        struct PartialDpuConfig {
            #[serde(default)]
            bootstrap_ca_source: Option<BootstrapCaSource>,
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
            #[serde(default)]
            service_vpc_slot_count: Option<u32>,
            #[serde(default)]
            additional_managed_sf: Option<u32>,
            #[serde(default)]
            restart_ovs_on_use_admin_network_change: Option<bool>,
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
            bootstrap_ca_source: partial
                .bootstrap_ca_source
                .unwrap_or(default.bootstrap_ca_source),
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
            service_vpc_slot_count: partial
                .service_vpc_slot_count
                .unwrap_or(default.service_vpc_slot_count),
            additional_managed_sf: partial
                .additional_managed_sf
                .unwrap_or(default.additional_managed_sf),
            restart_ovs_on_use_admin_network_change: partial
                .restart_ovs_on_use_admin_network_change
                .unwrap_or(default.restart_ovs_on_use_admin_network_change),
        })
    }
}

impl Default for DpuConfig {
    // Preingestion is only enabled for BF3 BMC Firmware upgrades. This is to support ingesting DPUs that come
    // with older BMC firmware versions than BF-23.10-5. BF-23.10-5 is the minimum BMC firmware that Site Explorer
    // can support auto-ingestion for.
    fn default() -> Self {
        Self {
            bootstrap_ca_source: BootstrapCaSource::default(),
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
            service_vpc_slot_count: 0,
            additional_managed_sf: 0,
            restart_ovs_on_use_admin_network_change: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkSecurityGroupRuleConfig {
    id: Option<String>,
    src_net: NetworkSecurityGroupRuleNet,
    dst_net: NetworkSecurityGroupRuleNet,
    direction: NetworkSecurityGroupRuleDirection,
    ipv6: bool,
    src_port_start: Option<u32>,
    src_port_end: Option<u32>,
    dst_port_start: Option<u32>,
    dst_port_end: Option<u32>,
    protocol: NetworkSecurityGroupRuleProtocol,
    action: NetworkSecurityGroupRuleAction,
    priority: u32,
}

impl From<NetworkSecurityGroupRuleConfig> for NetworkSecurityGroupRule {
    fn from(rule: NetworkSecurityGroupRuleConfig) -> Self {
        Self {
            id: rule.id,
            src_net: rule.src_net,
            dst_net: rule.dst_net,
            direction: rule.direction,
            ipv6: rule.ipv6,
            src_port_start: rule.src_port_start,
            src_port_end: rule.src_port_end,
            dst_port_start: rule.dst_port_start,
            dst_port_end: rule.dst_port_end,
            protocol: rule.protocol,
            action: rule.action,
            priority: rule.priority,
        }
    }
}

fn deserialize_network_security_group_policy_overrides<'de, D>(
    deserializer: D,
) -> Result<Vec<NetworkSecurityGroupRule>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<NetworkSecurityGroupRuleConfig>::deserialize(deserializer)
        .map(|rules| rules.into_iter().map(Into::into).collect())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSecurityGroupConfig {
    /// The maximum number of unique rules allowed for
    /// a network security group after rules are expanded.
    /// (src port range * dst port range * src prefix list * dst prefix list)
    #[serde(default = "default_max_network_security_group_size")]
    pub max_network_security_group_size: u32,
    /// Whether NSGs may enable stateful egress and the DPU enables its supporting NVUE options.
    ///
    /// When disabled, stateful NSG creation and updates from stateless to stateful are rejected.
    /// Existing stateful NSGs remain editable, but the DPU applies their rules statelessly.
    #[serde(default = "default_to_true")]
    pub stateful_acls_enabled: bool,

    /// A set of NSG rules that will be inserted before any user-defined rules.
    #[serde(
        default,
        deserialize_with = "deserialize_network_security_group_policy_overrides"
    )]
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
#[serde(deny_unknown_fields)]
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

pub fn default_max_site_prefixes_per_tenant() -> u32 {
    8
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
#[serde(deny_unknown_fields)]
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
            max_site_prefixes_per_tenant: value.max_site_prefixes_per_tenant,
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
            ewethers_enabled: value.ewethers_config.clone().unwrap_or_default().enabled,
            svpc_enabled: value
                .ewethers_config
                .clone()
                .unwrap_or_default()
                .svpc_enabled,
            astra_enabled: value
                .ewethers_config
                .clone()
                .unwrap_or_default()
                .astra_enabled,
            mqtt_endpoint: value
                .ewethers_config
                .clone()
                .unwrap_or_default()
                .svpc
                .mqtt_endpoint,
            mqtt_broker_port: value
                .ewethers_config
                .clone()
                .unwrap_or_default()
                .svpc
                .mqtt_broker_port as i32,
            mqtt_hb_interval: value
                .ewethers_config
                .clone()
                .unwrap_or_default()
                .svpc
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
                .ewethers_config
                .clone()
                .unwrap_or_default()
                .subnet_ip
                .to_string(),
            dpa_subnet_mask: value.ewethers_config.unwrap_or_default().subnet_mask,
            dpf_enabled: value.dpf.enabled,
            compile_time_helm_version: crate::dpf_services::COMPILE_TIME_HELM_VERSION.to_string(),
            compile_time_docker_version: crate::dpf_services::COMPILE_TIME_IMAGE_TAG.to_string(),
            restart_ovs_on_use_admin_network_change: value
                .dpu_config
                .restart_ovs_on_use_admin_network_change,
        }
    }
}

fn default_mqtt_endpoint() -> String {
    "mqtt.forge".to_string()
}

fn default_mqtt_broker_port() -> u16 {
    1884
}

pub use carbide_dpa_manager::config::{EwEthersConfig, MqttAuthConfig, MqttAuthMode, SvpcConfig};
use model::vpc::VpcDefinition;

/// DSX Exchange Event Bus configuration for publishing state change events via MQTT 3.1.1.
///
/// When configured, Carbide will publish `ManagedHostState` transitions to
/// `{topic_prefix}/{machineId}/state` (default `NICO/v1/machine`), publish BMS
/// rack leak/isolation values and heartbeat timestamps to metadata-defined DSX
/// topics, and subscribe to `BMS/v1/PUB/Metadata/#` to learn those routing
/// targets.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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

    /// Periodically re-publish current `ManagedHostState` in addition to
    /// publishing on every state change. Lets integrators that cannot poll the
    /// NICo API reconcile transitions they missed off the event bus.
    #[serde(default)]
    pub periodic_state_republish: PeriodicStateRepublishConfig,
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

/// Which managed hosts a periodic republish sweep publishes.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepublishScope {
    /// Republish every managed host on each sweep. Healthy hosts can still be
    /// published less often than unhealthy ones via `healthy_republish_every`.
    #[default]
    All,
    /// Republish only managed hosts that currently have a health alert. Use
    /// this to keep the event bus quiet and only re-advertise hosts that need
    /// attention.
    UnhealthyOnly,
}

/// Maximum number of MQTT publishes per second during a single republish sweep.
/// `0` means unbounded (publish as fast as the broker accepts).
///
/// Wraps the raw count so the pacing semantics live with the type rather than
/// being re-derived at call sites. `#[serde(transparent)]` keeps the config
/// surface a plain integer (e.g. `max_publishes_per_second = 200`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct PublishRate(pub u32);

impl PublishRate {
    /// Delay to insert between publishes to honor this rate, or `None` when
    /// unbounded.
    pub fn pacing_delay(self) -> Option<std::time::Duration> {
        (self.0 > 0).then(|| std::time::Duration::from_secs_f64(1.0 / f64::from(self.0)))
    }
}

const PUBLISH_INTERVAL_MIN: std::time::Duration = std::time::Duration::from_secs(1);
const PUBLISH_INTERVAL_MAX: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Periodic republishing of `ManagedHostState` on the DSX Exchange Event Bus.
///
/// NICo publishes state on every transition, but integrators that cannot poll
/// the NICo API (e.g. network-restricted consumers) can miss a transition and
/// never reconcile. Re-sending current state on a timer lets those consumers
/// self-heal. Republished messages reuse the same topic and JSON payload as
/// change-driven events, so consumers handle them identically.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PeriodicStateRepublishConfig {
    /// Enable periodic republishing. Enabled by default whenever the DSX
    /// Exchange Event Bus itself is enabled. Change-driven publishing is
    /// unaffected by this setting.
    #[serde(default = "PeriodicStateRepublishConfig::default_enabled")]
    pub enabled: bool,

    /// How often a republish sweep runs. Defaults to 5 minutes and is clamped
    /// to the supported range of 1 second through 1 hour.
    #[serde(
        default = "PeriodicStateRepublishConfig::default_interval",
        deserialize_with = "deserialize_duration",
        serialize_with = "as_std_duration"
    )]
    pub interval: std::time::Duration,

    /// Which managed hosts to publish on each sweep.
    #[serde(default)]
    pub scope: RepublishScope,

    /// When `scope = all`, publish healthy hosts only every Nth sweep to reduce
    /// broker noise; hosts with an active health alert are always published on
    /// every sweep. `1` (default) publishes healthy hosts every sweep. `0` is
    /// treated as `1`. Ignored when `scope = unhealthy_only`.
    #[serde(default = "PeriodicStateRepublishConfig::default_healthy_republish_every")]
    pub healthy_republish_every: u32,

    /// Upper bound on publishes per second within a single sweep, to avoid
    /// bursting the broker on large sites. `0` (default) disables pacing and
    /// publishes as fast as the broker accepts.
    #[serde(default)]
    pub max_publishes_per_second: PublishRate,
}

impl Default for PeriodicStateRepublishConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            interval: Self::default_interval(),
            scope: RepublishScope::default(),
            healthy_republish_every: Self::default_healthy_republish_every(),
            max_publishes_per_second: PublishRate(0),
        }
    }
}

impl PeriodicStateRepublishConfig {
    pub const fn default_enabled() -> bool {
        true
    }

    pub fn validate(&self) -> eyre::Result<()> {
        if self.interval.is_zero() {
            return Err(eyre::eyre!(
                "dsx_exchange_event_bus.periodic_state_republish.interval must be > 0s"
            ));
        }
        Ok(())
    }

    pub fn publish_interval(&self) -> std::time::Duration {
        self.interval
            .clamp(PUBLISH_INTERVAL_MIN, PUBLISH_INTERVAL_MAX)
    }

    pub const fn default_interval() -> std::time::Duration {
        std::time::Duration::from_secs(300)
    }

    pub const fn default_healthy_republish_every() -> u32 {
        1
    }
}

/// Auto machine repair plugin related configuration
#[derive(Default, Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct VmaasConfig {
    /// Allow VFs on instance creation.  defaults to true, but will be disabled when
    /// using SDN to manage the instance network configuration for VMs
    #[serde(default = "default_to_true")]
    pub allow_instance_vf: bool,

    /// Select which representors from the configured VF population HBN is expected to use.
    pub hbn_reps: Option<String>,

    /// Provisioning-time topology for bridges inserted between host representors and HBN.
    pub bridging: Option<HostRepresentorBridgingConfig>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HostRepresentorBridgingConfig {
    /// The HBN/SFC bridge that host-representor patch ports attach to during provisioning.
    #[serde(default = "default_hbn_bridge")]
    pub hbn_bridge: String,

    /// The layout of host-owned representors that will have intermediary bridges.
    /// E.g., [{"pf0hpf" => {bridge: "br-host", patch_port: "brh"}}]
    #[serde(default)]
    pub host_representor_intercept_bridging: HashMap<String, HostInterceptBridging>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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

    /// Typed PF/VF identity used only when DPF is enabled.
    pub dpf_interface: Option<DpfInterfaceIdentity>,
}

/// Typed DPF identity for a configured host PF or VF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct DpfInterfaceIdentity {
    /// DPF controller number that owns the selected PF or VF.
    pub controller_id: u8,

    /// Physical-function identifier on the selected controller.
    pub pf_id: u8,

    /// Virtual-function identifier. Omission selects the PF itself.
    pub vf_id: Option<u8>,
}

impl HostRepresentorBridgingConfig {
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
        config.none_if_empty()
    }
}

pub fn default_hbn_bridge() -> String {
    "br-hbn".to_string()
}

#[cfg(test)]
mod tests {

    use std::sync::atomic::Ordering as AtomicOrdering;

    use carbide_network::virtualization::VpcVirtualizationType;
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};
    use figment::Figment;
    use figment::error::Kind;
    use figment::providers::{Env, Format, Toml};
    use model::expected_machine::HostDpuPolicy;
    use model::vpc::VpcRoutingProfileOverrides;

    use super::*;

    /// Disabling both bearer tokens and machine mTLS would lock every node out
    /// of the API; validation must refuse the combination, and each mechanism
    /// alone must pass.
    #[test]
    fn node_auth_rejects_all_methods_disabled() {
        let both_off = NodeAuthConfig {
            enabled: false,
            mtls_enabled: false,
            ..NodeAuthConfig::default()
        };
        assert!(both_off.validate().is_err());

        assert!(NodeAuthConfig::default().validate().is_ok());
        let jwt_only = NodeAuthConfig {
            enabled: true,
            mtls_enabled: false,
            ..NodeAuthConfig::default()
        };
        assert!(jwt_only.validate().is_ok());
    }

    #[test]
    fn node_auth_rejects_the_removed_audience_setting() {
        let error = toml::from_str::<NodeAuthConfig>("audience = \"nico-api-eu\"")
            .expect_err("the node-auth audience is fixed");
        assert!(error.to_string().contains("unknown field `audience`"));
    }

    #[test]
    fn credentials_file_config_contract() {
        scenarios!(
            run = |config: &str| toml::from_str::<CredentialsConfig>(config).map_err(drop);
            "optional source" {
                "" => Yields(CredentialsConfig::default()),
            }

            "valid file source" {
                r#"
ufm_source = "local"

[file]
path = "/var/run/secrets/nico/ufm/credentials.yaml"
poll_interval = "17s"
"# => Yields(CredentialsConfig {
                    ufm_source: UfmCredentialSource::Local,
                    file: Some(CredentialFileSourceConfig {
                        path: PathBuf::from("/var/run/secrets/nico/ufm/credentials.yaml"),
                        poll_interval: std::time::Duration::from_secs(17),
                    }),
                }),
            }

            "file source default poll interval" {
                r#"
[file]
path = "credentials.yaml"
"# => Yields(CredentialsConfig {
                    ufm_source: UfmCredentialSource::LocalFirst,
                    file: Some(CredentialFileSourceConfig {
                        path: PathBuf::from("credentials.yaml"),
                        poll_interval: std::time::Duration::from_secs(60),
                    }),
                }),
            }

            "missing file path" {
                "[file]\npoll_interval = \"17s\"" => Fails,
            }

            "unknown field" {
                "[file]\npath = \"credentials.yaml\"\ntoken = \"not-a-secret-here\"" => Fails,
            }

            "unknown UFM source" {
                "ufm_source = \"fallback\"" => Fails,
            }
        );
    }

    #[test]
    fn ufm_source_contract() {
        value_scenarios!(
            run = |ufm_source| CredentialsConfig {
                ufm_source,
                file: None,
            }.uses_authoritative_local_ufm_credentials();
            "credential source modes" {
                UfmCredentialSource::LocalFirst => false,
                UfmCredentialSource::Backend => false,
                UfmCredentialSource::Local => true,
            }
        );
    }

    /// A cap below the clients' fixed 300 s lifetime is accepted-looking and
    /// fatal: every token the fleet mints exceeds it, so all of them are
    /// rejected. Startup has to refuse rather than let the fleet discover it.
    #[test]
    fn max_token_ttl_below_the_client_lifetime_is_rejected() {
        let too_small = NodeAuthConfig {
            enabled: true,
            max_token_ttl_sec: 60,
            ..NodeAuthConfig::default()
        };
        let err = too_small
            .validate()
            .expect_err("a cap under the client TTL must be refused");
        assert!(
            err.to_string().contains("max_token_ttl_sec"),
            "the error should name the setting, got: {err}"
        );

        // Exactly the client lifetime is the boundary, and is fine.
        let exact = NodeAuthConfig {
            enabled: true,
            max_token_ttl_sec: u32::try_from(::rpc::node_jwt::NODE_JWT_TTL_SECS).expect("fits"),
            ..NodeAuthConfig::default()
        };
        assert!(exact.validate().is_ok());

        // Disabled: the cap is never consulted, so it must not block startup.
        let disabled = NodeAuthConfig {
            enabled: false,
            max_token_ttl_sec: 60,
            ..NodeAuthConfig::default()
        };
        assert!(disabled.validate().is_ok());
    }

    /// Unset means "follow `enabled`", which is what almost every site wants.
    /// The override exists so a disable can be staged: fmds goes back to client
    /// certificates while the API still accepts tokens.
    #[test]
    fn fmds_token_mode_follows_enabled_unless_overridden() {
        let enabled = NodeAuthConfig {
            enabled: true,
            ..NodeAuthConfig::default()
        };
        assert!(enabled.fmds_use_node_tokens(), "unset follows enabled=true");

        let disabled = NodeAuthConfig::default();
        assert!(
            !disabled.fmds_use_node_tokens(),
            "unset follows enabled=false"
        );

        // The staging step: API still accepting tokens, fmds moved off them.
        let staging = NodeAuthConfig {
            enabled: true,
            fmds_use_node_tokens: Some(false),
            ..NodeAuthConfig::default()
        };
        assert!(!staging.fmds_use_node_tokens());
        assert!(
            staging.validate().is_ok(),
            "moving fmds off tokens early is the supported path"
        );
    }

    /// The inverse is the outage the override exists to prevent: fmds
    /// presenting bearer tokens to an API that does not accept them. Refuse it
    /// at startup rather than deploying it.
    #[test]
    fn fmds_token_mode_cannot_outrun_the_api() {
        let ahead = NodeAuthConfig {
            enabled: false,
            mtls_enabled: true,
            fmds_use_node_tokens: Some(true),
            ..NodeAuthConfig::default()
        };
        let err = ahead
            .validate()
            .expect_err("fmds must not present tokens the API refuses");
        assert!(
            err.to_string().contains("fmds_use_node_tokens"),
            "the error should name the setting, got: {err}"
        );
    }

    const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cfg/test_data");

    /// Verifies legacy entries remain valid while typed DPF identities require and preserve their
    /// complete controller, PF, and optional VF selection.
    #[test]
    fn host_intercept_bridging_deserializes_optional_dpf_identity() {
        scenarios!(
            run = |config: &str| {
                toml::from_str::<HostInterceptBridging>(config)
                    .map(|bridging| bridging.dpf_interface)
                    .map_err(drop)
            };
            "legacy compatibility" {
                // Existing pre-DPF entries do not need typed identity.
                "bridge = 'br-host'\npatch_port = 'p-host'" => Yields(None),
            }

            "PF identity" {
                // Omitting vf_id selects the complete configured PF identity.
                "bridge = 'br-host'\npatch_port = 'p-host'\ndpf_interface = { controller_id = 2, pf_id = 3 }" => Yields(Some(DpfInterfaceIdentity {
                    controller_id: 2,
                    pf_id: 3,
                    vf_id: None,
                })),
            }

            "VF identity" {
                // Including vf_id preserves that VF under the complete configured PF identity.
                "bridge = 'br-host'\npatch_port = 'p-host'\ndpf_interface = { controller_id = 2, pf_id = 3, vf_id = 4 }" => Yields(Some(DpfInterfaceIdentity {
                    controller_id: 2,
                    pf_id: 3,
                    vf_id: Some(4),
                })),
            }

            "missing controller identity" {
                // A PF number without its controller cannot select DPF hardware unambiguously.
                "bridge = 'br-host'\npatch_port = 'p-host'\ndpf_interface = { pf_id = 3 }" => Fails,
            }

            "missing PF identity" {
                // A controller without its PF cannot select the required parent interface.
                "bridge = 'br-host'\npatch_port = 'p-host'\ndpf_interface = { controller_id = 2 }" => Fails,
            }
        );
    }

    /// Verifies typed DPF identity cannot alter the legacy sorted `bf.cfg` value.
    #[test]
    fn dpf_identity_does_not_change_legacy_bridging_provisioning_config() {
        // Build equivalent legacy entries with and without a typed DPF identity.
        let make_config = |dpf_interface| HostRepresentorBridgingConfig {
            hbn_bridge: default_hbn_bridge(),
            host_representor_intercept_bridging: HashMap::from([
                (
                    "pf0vf1".to_string(),
                    HostInterceptBridging {
                        bridge: "br-vf1".to_string(),
                        patch_port: "p-vf1".to_string(),
                        skip_create: false,
                        dpf_interface,
                    },
                ),
                (
                    "pf0vf0".to_string(),
                    HostInterceptBridging {
                        bridge: "br-vf0".to_string(),
                        patch_port: "p-vf0".to_string(),
                        skip_create: false,
                        dpf_interface,
                    },
                ),
            ]),
        };
        let typed_identity = Some(DpfInterfaceIdentity {
            controller_id: 1,
            pf_id: 0,
            vf_id: Some(3),
        });

        // Both variants must render the exact historical ordering and wire format.
        let expected = Some("pf0vf0:br-vf0:p-vf0,pf0vf1:br-vf1:p-vf1".to_string());
        assert_eq!(
            make_config(None).host_representor_intercept_bridging_provisioning_config(),
            expected
        );
        assert_eq!(
            make_config(typed_identity).host_representor_intercept_bridging_provisioning_config(),
            expected
        );
    }

    fn vpc_config(
        routing_profile_type: Option<&str>,
        routing_profile_overrides: Option<VpcRoutingProfileOverrides>,
    ) -> VpcConfig {
        VpcConfig {
            tenant_organization_id: "test-tenant".to_string(),
            tenant_keyset_id: None,
            network_virtualization_type: VpcVirtualizationType::Fnn,
            network_security_group_id: None,
            default_nvlink_logical_partition_id: None,
            vni: None,
            routing_profile_type: routing_profile_type.map(str::to_string),
            routing_profile_overrides,
            power_resource_group: None,
            slaac_enabled: false,
        }
    }

    #[test]
    fn tenant_prefix_overlap_site_gate_defaults_false_and_parses() {
        value_scenarios!(
            run = |patch| Figment::new()
                .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                .merge(Toml::string(patch))
                .extract::<CarbideConfig>()
                .expect("tenant prefix overlap site setting must parse")
                .tenant_prefix_overlap_enabled;
            "site setting" {
                "" => false,
                "tenant_prefix_overlap_enabled = false" => false,
                "tenant_prefix_overlap_enabled = true" => true,
            }
        );
    }

    #[test]
    fn fnn_overlap_eligibility_opt_in_defaults_false_and_parses() {
        value_scenarios!(
            run = |input| toml::from_str::<FnnRoutingProfileConfig>(input)
                .expect("FNN overlap eligibility setting must parse")
                .tenant_prefix_overlap_eligible;
            "base profile setting" {
                "" => false,
                "tenant_prefix_overlap_eligible = false" => false,
                "tenant_prefix_overlap_eligible = true" => true,
            }
        );
    }

    #[test]
    fn resolved_profile_overlap_eligibility_includes_vpc_overrides() {
        let fnn = FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([(
                "BASE".to_string(),
                FnnRoutingProfileConfig {
                    tenant_prefix_overlap_eligible: true,
                    internal: Some(true),
                    ..Default::default()
                },
            )]),
            use_vpc_vrf_loopback: false,
        };
        assert!(
            fnn.routing_profiles["BASE"].is_eligible_for_tenant_prefix_overlap(),
            "base profile should be eligible before VPC overrides"
        );

        let vpc = vpc_config(
            Some("BASE"),
            Some(VpcRoutingProfileOverrides {
                leak_default_route_from_underlay: Some(true),
                ..Default::default()
            }),
        );
        let resolved = fnn
            .resolve_vpc_routing_profile(&vpc)
            .expect("base profile must resolve");

        assert!(
            !resolved.is_eligible_for_tenant_prefix_overlap(),
            "resolved VPC leak must make the profile ineligible"
        );
    }

    #[test]
    fn vpc_cannot_override_overlap_eligibility() {
        let error =
            toml::from_str::<VpcRoutingProfileOverrides>("tenant_prefix_overlap_eligible = true")
                .expect_err("overlap eligibility must remain a base-profile setting");

        assert!(
            error
                .to_string()
                .contains("unknown field `tenant_prefix_overlap_eligible`")
        );
    }

    /// Verifies existing routing-profile TOML values deserialize unchanged
    /// after the fields become presence-aware.
    #[test]
    fn fnn_routing_profile_options_accept_existing_toml_syntax() {
        let profile: FnnRoutingProfileConfig = Figment::new()
            .merge(Toml::string(
                r#"
                    route_target_imports = [{ asn = 64512, vni = 10 }]
                    route_targets_on_exports = []
                    internal = true
                    leak_default_route_from_underlay = false
                    leak_tenant_host_routes_to_underlay = true
                    tenant_leak_communities_accepted = false
                    accepted_leaks_from_underlay = [{ prefix = "10.0.0.0/8" }]
                    allowed_anycast_prefixes = []
                    access_tier = 2
                "#,
            ))
            .extract()
            .expect("existing routing-profile syntax must remain valid");

        assert_eq!(
            profile,
            FnnRoutingProfileConfig {
                tenant_prefix_overlap_eligible: false,
                route_target_imports: Some(vec![RouteTargetConfig {
                    asn: 64512,
                    vni: 10,
                }]),
                route_targets_on_exports: Some(vec![]),
                internal: Some(true),
                leak_default_route_from_underlay: Some(false),
                leak_tenant_host_routes_to_underlay: Some(true),
                tenant_leak_communities_accepted: Some(false),
                accepted_leaks_from_underlay: Some(vec![PrefixFilterPolicyEntry {
                    prefix: "10.0.0.0/8".parse().expect("valid test prefix"),
                }]),
                allowed_anycast_prefixes: Some(vec![]),
                access_tier: Some(2),
            }
        );
    }

    /// Verifies seed-time VPC TOML preserves unsupported inline overrides
    /// so startup validation can reject them instead of silently ignoring them.
    #[test]
    fn vpc_definition_preserves_routing_profile_overrides_for_seed_validation() {
        // Parse a seeded VPC with representative unsupported override values.
        let config: InitialObjectsConfig = Figment::new()
            .merge(Toml::string(
                r#"
                    [vpcs.inline-profile]
                    organization_id = "inline-profile-test"
                    network_virtualization_type = "fnn"
                    routing_profile_type = "BASE"

                    [vpcs.inline-profile.routing_profile_overrides]
                    route_target_imports = []
                    leak_default_route_from_underlay = false
                    allowed_anycast_prefixes = [{ prefix = "192.0.2.0/24" }]
                "#,
            ))
            .extract()
            .expect("seed validation must receive configured routing-profile overrides");
        let definition = config
            .vpcs
            .as_ref()
            .expect("configured VPCs")
            .get("inline-profile")
            .expect("inline-profile VPC");

        // Explicit empty and false values remain visible to startup validation.
        assert_eq!(
            definition,
            &VpcDefinition {
                organization_id: Some("inline-profile-test".to_string()),
                network_virtualization_type: VpcVirtualizationType::Fnn,
                routing_profile_type: Some("BASE".to_string()),
                routing_profile_overrides: Some(VpcRoutingProfileOverrides {
                    route_target_imports: Some(vec![]),
                    leak_default_route_from_underlay: Some(false),
                    allowed_anycast_prefixes: Some(vec![PrefixFilterPolicyEntry {
                        prefix: "192.0.2.0/24".parse().expect("valid test prefix"),
                    }]),
                    ..Default::default()
                }),
                vni: None,
            }
        );
    }

    /// Verifies VPC properties override only present fields while
    /// `tenant_prefix_overlap_eligible`, `internal`, and `access_tier` remain owned by the
    /// base profile.
    #[test]
    fn vpc_routing_profile_overrides_are_presence_aware() {
        // Build a complete base and an override containing explicit default values.
        let inherited_export = RouteTargetConfig { asn: 1, vni: 2 };
        let inherited_anycast = PrefixFilterPolicyEntry {
            prefix: "192.0.2.0/24".parse().expect("valid test prefix"),
        };
        let base = FnnRoutingProfileConfig {
            tenant_prefix_overlap_eligible: true,
            route_target_imports: Some(vec![RouteTargetConfig { asn: 3, vni: 4 }]),
            route_targets_on_exports: Some(vec![inherited_export.clone()]),
            internal: Some(true),
            leak_default_route_from_underlay: Some(true),
            leak_tenant_host_routes_to_underlay: Some(true),
            tenant_leak_communities_accepted: Some(true),
            accepted_leaks_from_underlay: Some(vec![PrefixFilterPolicyEntry {
                prefix: "198.51.100.0/24".parse().expect("valid test prefix"),
            }]),
            allowed_anycast_prefixes: Some(vec![inherited_anycast.clone()]),
            access_tier: Some(2),
        };
        let overrides = VpcRoutingProfileOverrides {
            route_target_imports: Some(vec![]),
            leak_default_route_from_underlay: Some(false),
            tenant_leak_communities_accepted: Some(false),
            accepted_leaks_from_underlay: Some(vec![]),
            ..Default::default()
        };
        let fnn = FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::from([("BASE".to_string(), base)]),
            use_vpc_vrf_loopback: false,
        };
        let vpc = vpc_config(Some("BASE"), Some(overrides));

        // Explicit empty and false values override; absent values inherit.
        assert_eq!(
            fnn.resolve_vpc_routing_profile(&vpc).unwrap().as_ref(),
            &FnnRoutingProfileConfig {
                tenant_prefix_overlap_eligible: true,
                route_target_imports: Some(vec![]),
                route_targets_on_exports: Some(vec![inherited_export]),
                internal: Some(true),
                leak_default_route_from_underlay: Some(false),
                leak_tenant_host_routes_to_underlay: Some(true),
                tenant_leak_communities_accepted: Some(false),
                accepted_leaks_from_underlay: Some(vec![]),
                allowed_anycast_prefixes: Some(vec![inherited_anycast]),
                access_tier: Some(2),
            }
        );
    }

    #[test]
    fn vpc_routing_profile_resolution_reports_consistent_errors() {
        let fnn = FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::new(),
            use_vpc_vrf_loopback: false,
        };

        check_values(
            [
                Check {
                    scenario: "routing profile type absent from VPC",
                    input: None,
                    expect: "internal error: tenant routing profile type not found in VPC record"
                        .to_string(),
                },
                Check {
                    scenario: "named routing profile absent from FNN config",
                    input: Some("MISSING"),
                    expect: "routing_profile_type not found: MISSING".to_string(),
                },
            ],
            |profile_type| {
                fnn.resolve_vpc_routing_profile(&vpc_config(profile_type, None))
                    .unwrap_err()
                    .to_string()
            },
        );
    }

    #[test]
    fn deny_prefixes_accept_both_address_families() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::string(
                r#"
                    database_url = "postgres://test"
                    listen = "[::]:1081"
                    asn = 1
                    deny_prefixes = ["192.0.2.0/24", "2001:db8::/32"]
                    anycast_site_prefixes = ["198.51.100.0/24"]
                "#,
            ))
            .extract()
            .expect("dual-stack deny prefixes must parse");

        assert_eq!(
            config.deny_prefixes,
            vec![
                "192.0.2.0/24".parse::<IpNetwork>().unwrap(),
                "2001:db8::/32".parse::<IpNetwork>().unwrap(),
            ]
        );
        assert_eq!(
            config.anycast_site_prefixes,
            vec!["198.51.100.0/24".parse::<Ipv4Network>().unwrap()]
        );
    }

    #[test]
    fn anycast_site_prefixes_reject_ipv6() {
        let result = Figment::new()
            .merge(Toml::string(
                r#"
                    database_url = "postgres://test"
                    listen = "[::]:1081"
                    asn = 1
                    anycast_site_prefixes = ["2001:db8::/32"]
                "#,
            ))
            .extract::<CarbideConfig>();

        assert!(
            result.is_err(),
            "IPv6 anycast site prefixes must be rejected"
        );
    }

    /// Exercises the real `[certificates]` / `[certificates.dedicated_vault]`
    /// TOML contract through Figment (the production config path), rather than
    /// JSON serde. Each case parses a TOML fragment into `CertificatesConfig`
    /// and asserts either the parse outcome or the `to_certificate_config`
    /// mapping/error.
    #[test]
    fn certificates_toml_config_contract() {
        use carbide_secrets::CertBackend;

        enum Expect {
            /// Figment/serde extraction fails (missing required field, unknown key).
            ParseErr,
            /// Extraction succeeds but `to_certificate_config` rejects it.
            ConvertErr,
            Shared,
            Dedicated {
                address: &'static str,
                pki_mount_location: &'static str,
                pki_role_name: &'static str,
                token: Option<&'static str>,
                vault_cacert: Option<&'static str>,
                namespace: Option<&'static str>,
            },
        }

        // The fragments extract into `CertificatesConfig` directly, so the root
        // fields (`backend`, `[dedicated_vault]`) are the same ones that live
        // under the `[certificates]` / `[certificates.dedicated_vault]` tables.
        let cases: &[(&str, &str, Expect)] = &[
            (
                "absent section defaults to shared_vault",
                "",
                Expect::Shared,
            ),
            (
                "explicit shared_vault",
                r#"backend = "shared_vault""#,
                Expect::Shared,
            ),
            (
                "dedicated_vault maps all fields",
                r#"
                    backend = "dedicated_vault"
                    [dedicated_vault]
                    address = "https://vault-certs.example:8200"
                    pki_mount_location = "pki"
                    pki_role_name = "machine"
                    token = "s.abc123"
                    vault_cacert = "/etc/ssl/certs/vault-ca.pem"
                    namespace = "admin/certificates"
                "#,
                Expect::Dedicated {
                    address: "https://vault-certs.example:8200",
                    pki_mount_location: "pki",
                    pki_role_name: "machine",
                    token: Some("s.abc123"),
                    vault_cacert: Some("/etc/ssl/certs/vault-ca.pem"),
                    namespace: Some("admin/certificates"),
                },
            ),
            (
                "dedicated_vault selected without its section fails conversion",
                r#"backend = "dedicated_vault""#,
                Expect::ConvertErr,
            ),
            (
                "dedicated_vault missing required address fails parse",
                r#"
                    backend = "dedicated_vault"
                    [dedicated_vault]
                    pki_mount_location = "pki"
                    pki_role_name = "machine"
                "#,
                Expect::ParseErr,
            ),
            (
                "unknown field rejected by deny_unknown_fields",
                "backend = \"shared_vault\"\ntypo = true",
                Expect::ParseErr,
            ),
        ];

        for (name, toml, expect) in cases {
            let parsed: Result<CertificatesConfig, _> =
                Figment::new().merge(Toml::string(toml)).extract();

            match expect {
                Expect::ParseErr => {
                    assert!(parsed.is_err(), "{name}: expected a parse error");
                }
                Expect::ConvertErr => {
                    let cfg = parsed
                        .unwrap_or_else(|e| panic!("{name}: expected parse to succeed, got {e}"));
                    let err = match cfg.to_certificate_config() {
                        Ok(_) => panic!("{name}: expected conversion to fail"),
                        Err(err) => err,
                    };
                    assert!(
                        err.to_string().contains("dedicated_vault"),
                        "{name}: unexpected error: {err}"
                    );
                }
                Expect::Shared => {
                    let cfg = parsed
                        .unwrap_or_else(|e| panic!("{name}: expected parse to succeed, got {e}"));
                    assert!(
                        matches!(
                            cfg.to_certificate_config().unwrap().backend,
                            CertBackend::SharedVault
                        ),
                        "{name}: expected SharedVault backend"
                    );
                }
                Expect::Dedicated {
                    address,
                    pki_mount_location,
                    pki_role_name,
                    token,
                    vault_cacert,
                    namespace,
                } => {
                    let cfg = parsed
                        .unwrap_or_else(|e| panic!("{name}: expected parse to succeed, got {e}"));
                    match cfg.to_certificate_config().unwrap().backend {
                        CertBackend::DedicatedVault(d) => {
                            assert_eq!(d.address, *address, "{name}: address");
                            assert_eq!(
                                d.pki_mount_location, *pki_mount_location,
                                "{name}: pki_mount_location"
                            );
                            assert_eq!(d.pki_role_name, *pki_role_name, "{name}: pki_role_name");
                            assert_eq!(d.token.as_deref(), *token, "{name}: token");
                            assert_eq!(
                                d.vault_cacert.as_deref(),
                                *vault_cacert,
                                "{name}: vault_cacert"
                            );
                            assert_eq!(d.namespace.as_deref(), *namespace, "{name}: namespace");
                        }
                        other => panic!("{name}: expected dedicated vault backend, got {other:?}"),
                    }
                }
            }
        }
    }

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
            waiting_for_measurements_timeout: Duration::hours(4),
            uefi_boot_wait: Duration::minutes(5),
            max_bios_config_retries: 3,
            polling_bios_setup_stuck_threshold: Duration::minutes(15),
            boot_interface_observation_interval: Duration::hours(2),
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
        "boot_interface_observation_interval": "2h",
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
                waiting_for_measurements_timeout: Duration::hours(4),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
                boot_interface_observation_interval: Duration::hours(2),
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
                waiting_for_measurements_timeout: Duration::hours(4),
                uefi_boot_wait: Duration::minutes(5),
                max_bios_config_retries: 3,
                polling_bios_setup_stuck_threshold: Duration::minutes(15),
                boot_interface_observation_interval: Duration::minutes(10),
            }
        );
    }

    #[test]
    fn reject_nonpositive_boot_interface_observation_intervals() {
        for invalid_interval in ["0s", "-1s"] {
            let config_json =
                format!(r#"{{"boot_interface_observation_interval": "{invalid_interval}"}}"#);
            assert!(
                serde_json::from_str::<MachineStateControllerConfig>(&config_json).is_err(),
                "boot_interface_observation_interval={invalid_interval} must be rejected",
            );
        }
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
    fn periodic_state_republish_defaults_enabled() {
        let config = PeriodicStateRepublishConfig::default();

        assert!(config.enabled);
    }

    #[test]
    fn api_admission_control_only_validates_bounds_when_enabled() {
        type ZeroOut = fn(&mut ApiAdmissionControlConfig);
        let cases: [(&str, ZeroOut); 6] = [
            ("max_work_in_flight", |config| config.max_work_in_flight = 0),
            ("max_pending", |config| config.max_pending = 0),
            ("max_work_in_flight_per_client", |config| {
                config.max_work_in_flight_per_client = 0
            }),
            ("max_pending_per_client", |config| {
                config.max_pending_per_client = 0
            }),
            ("pending_timeout", |config| {
                config.pending_timeout = std::time::Duration::ZERO
            }),
            ("client_idle_timeout", |config| {
                config.client_idle_timeout = std::time::Duration::ZERO
            }),
        ];

        let disabled = ApiAdmissionControlConfig {
            enabled: false,
            max_work_in_flight: 0,
            max_pending: 0,
            max_work_in_flight_per_client: 0,
            max_pending_per_client: 0,
            pending_timeout: std::time::Duration::ZERO,
            client_idle_timeout: std::time::Duration::ZERO,
            service_limits: BTreeMap::new(),
        };
        disabled
            .validate()
            .expect("disabled admission control ignores its bounds");

        for (field, zero_out) in cases {
            let mut config = ApiAdmissionControlConfig::default();
            zero_out(&mut config);
            let error = config
                .validate()
                .expect_err("zero admission values must be rejected");
            assert!(
                error.to_string().contains(field),
                "error must name {field}, got: {error}"
            );
        }
    }

    fn assert_api_admission_semaphore_bound(
        field: &str,
        set_value: fn(&mut ApiAdmissionControlConfig, usize),
    ) {
        let mut config = ApiAdmissionControlConfig::default();
        set_value(&mut config, tokio::sync::Semaphore::MAX_PERMITS);
        config
            .validate()
            .expect("Tokio's semaphore maximum must be accepted");

        set_value(&mut config, tokio::sync::Semaphore::MAX_PERMITS + 1);
        let error = config
            .validate()
            .expect_err("values above Tokio's semaphore maximum must be rejected");
        assert!(
            error.to_string().contains(field),
            "error must name {field}, got: {error}"
        );
        assert!(
            error
                .to_string()
                .contains(&tokio::sync::Semaphore::MAX_PERMITS.to_string()),
            "error must name the maximum, got: {error}"
        );
    }

    #[test]
    fn api_admission_control_validates_max_work_in_flight_upper_bound() {
        assert_api_admission_semaphore_bound("max_work_in_flight", |config, value| {
            config.max_work_in_flight = value;
        });
    }

    #[test]
    fn api_admission_control_validates_max_pending_upper_bound() {
        assert_api_admission_semaphore_bound("max_pending", |config, value| {
            config.max_pending = value;
        });
    }

    #[test]
    fn periodic_state_republish_rejects_zero_interval() {
        for enabled in [true, false] {
            let config = PeriodicStateRepublishConfig {
                enabled,
                interval: std::time::Duration::ZERO,
                ..Default::default()
            };

            let err = config.validate().expect_err("zero interval must error");
            assert!(
                err.to_string().contains(
                    "dsx_exchange_event_bus.periodic_state_republish.interval must be > 0s"
                ),
                "unexpected error: {err}"
            );
        }
    }

    #[test]
    fn periodic_state_republish_clamps_interval() {
        for (configured, expected) in [
            (std::time::Duration::from_millis(500), PUBLISH_INTERVAL_MIN),
            (PUBLISH_INTERVAL_MIN, PUBLISH_INTERVAL_MIN),
            (
                PeriodicStateRepublishConfig::default_interval(),
                PeriodicStateRepublishConfig::default_interval(),
            ),
            (PUBLISH_INTERVAL_MAX, PUBLISH_INTERVAL_MAX),
            (
                std::time::Duration::from_secs(2 * 60 * 60),
                PUBLISH_INTERVAL_MAX,
            ),
        ] {
            let config = PeriodicStateRepublishConfig {
                interval: configured,
                ..Default::default()
            };

            assert_eq!(config.publish_interval(), expected);
        }
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

        // The dedicated certificate Vault's root token must not survive redaction.
        config.certificates.dedicated_vault = Some(DedicatedVaultSettings {
            address: "https://vault-certs.example:8200".to_string(),
            pki_mount_location: "pki".to_string(),
            pki_role_name: "leaf".to_string(),
            token: Some("s.super-secret-root-token".to_string()),
            vault_cacert: None,
            namespace: None,
        });
        let redacted = config.redacted();
        assert_eq!(
            redacted
                .certificates
                .dedicated_vault
                .as_ref()
                .and_then(|d| d.token.as_deref()),
            Some("redacted")
        );
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
        assert!(!config.deny_unknown_fields);
        // Literals on purpose: these pin the documented defaults (30s/10m/30m
        // -- sqlx's own), so silently changing a default fn fails here rather
        // than passing self-referentially.
        assert_eq!(
            config.database_pool_acquire_timeout,
            std::time::Duration::from_secs(30)
        );
        assert_eq!(
            config.database_pool_idle_timeout,
            std::time::Duration::from_secs(10 * 60)
        );
        assert_eq!(
            config.database_pool_max_lifetime,
            std::time::Duration::from_secs(30 * 60)
        );
        assert_eq!(
            config.api_admission_control,
            ApiAdmissionControlConfig {
                enabled: true,
                max_work_in_flight: 64,
                max_pending: 1024,
                max_work_in_flight_per_client: 8,
                max_pending_per_client: 64,
                pending_timeout: std::time::Duration::from_secs(5),
                client_idle_timeout: std::time::Duration::from_secs(5 * 60),
                service_limits: BTreeMap::new(),
            }
        );
        assert!(config.dhcp_servers.is_empty());
        assert!(!config.allow_insecure_discovery);
        assert!(!config.scout_boot_interface_correction_enabled);
        assert!(config.route_servers.is_empty());
        assert!(config.tls.is_none());
        assert!(config.auth.is_none());
        assert!(config.pools.is_none());
        assert!(config.ib_config.is_none());
        assert!(config.ib_fabrics.is_empty());
        assert_eq!(config.credentials, CredentialsConfig::default());
        assert_eq!(
            config.bmc_session_lockout_threshold,
            default_bmc_session_lockout_threshold()
        );
        // The literal, not the helper: this pins the documented default so a
        // drive-by change to the helper cannot silently diverge from the
        // config docs.
        assert_eq!(config.bmc_max_sessions_per_caller, 4);
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
            config.extension_service_state_controller,
            ExtensionServiceStateControllerConfig::default()
        );
        assert_eq!(
            config.ib_partition_state_controller,
            IbPartitionStateControllerConfig::default()
        );
        assert_eq!(config.max_find_by_ids, default_max_find_by_ids());
        assert_eq!(
            config.max_site_prefixes_per_tenant,
            default_max_site_prefixes_per_tenant()
        );
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
    fn insecure_discovery_configuration_is_opt_in() {
        check_values(
            [
                Check {
                    scenario: "omitted",
                    input: "",
                    expect: false,
                },
                Check {
                    scenario: "explicitly disabled",
                    input: "allow_insecure_discovery = false",
                    expect: false,
                },
                Check {
                    scenario: "explicitly enabled",
                    input: "allow_insecure_discovery = true",
                    expect: true,
                },
            ],
            |patch| {
                let config: CarbideConfig = Figment::new()
                    .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                    .merge(Toml::string(patch))
                    .extract()
                    .unwrap();
                config.allow_insecure_discovery
            },
        );
    }

    fn rendered_helm_api_config() -> String {
        let mut config =
            include_str!("../../../../helm/charts/nico-api/files/carbide-api-config.toml")
                .to_string();
        for (template, rendered) in [
            (
                r#"{{ .Values.auth.adminRootCafilePath | default "/etc/forge/carbide-api/site/admin_root_cert_pem" }}"#,
                "/etc/forge/carbide-api/site/admin_root_cert_pem",
            ),
            ("{{ .Values.auth.permissiveMode | default false }}", "false"),
            ("{{ .Values.global.spiffe.trustDomain }}", "example.test"),
            (
                r#"{{ .Values.auth.namespace | default (include "nico-api.namespace" .) }}"#,
                "nico-system",
            ),
            (
                r#"{{ printf "%s/credentials.yaml" (required "nico-api.credentials.file.mountPath is required when credentials.file.existingSecret.name is set" .Values.credentials.file.mountPath) | quote }}"#,
                r#""/var/run/secrets/nico/ufm/credentials.yaml""#,
            ),
            (
                r#"{{ required "nico-api.credentials.file.pollInterval is required when credentials.file.existingSecret.name is set" .Values.credentials.file.pollInterval | quote }}"#,
                r#""60s""#,
            ),
            (
                "{{ .Values.credentials.ufmSource | quote }}",
                r#""local_first""#,
            ),
            (
                "{{ range $i, $cn := .Values.auth.additionalIssuerCns }}{{ if $i }}, {{ end }}{{ $cn | quote }}{{ end }}",
                "",
            ),
            (
                "{{ .Values.service.perObjectStateMetrics.enabled }}",
                "false",
            ),
            ("{{ .Values.service.perObjectStateMetrics.port }}", "9091"),
            (
                "{{ default list .Values.service.perObjectStateMetrics.objectTypes | toJson }}",
                "[]",
            ),
            (
                "{{ .Values.componentManager.computeTrayBackend | quote }}",
                r#""rms""#,
            ),
            (
                "{{ .Values.componentManager.nvSwitchBackend | quote }}",
                r#""rms""#,
            ),
            (
                "{{ .Values.componentManager.powerShelfBackend | quote }}",
                r#""rms""#,
            ),
            (
                "{{ .Values.componentManager.nvSwitchUseStateController }}",
                "false",
            ),
            (
                "{{ .Values.componentManager.powerShelfUseStateController }}",
                "false",
            ),
            (
                "{{ .Values.componentManager.computeTrayUseStateController }}",
                "false",
            ),
            (
                "{{ .Values.rms.apiUrl | quote }}",
                r#""https://rms.example.test""#,
            ),
            ("{{ .Values.rms.enforceTls }}", "true"),
            ("{{ . | quote }}", r#""/tmp/test.pem""#),
        ] {
            config = config.replace(template, rendered);
        }

        let config = config
            .lines()
            .filter(|line| !line.trim_start().starts_with("{{-"))
            .join("\n");
        assert!(
            !config.contains("{{"),
            "all Helm template expressions must be rendered for this test"
        );
        config
    }

    fn rendered_deployment_site_config() -> String {
        let mut config =
            include_str!("../../../../deploy/files/nico-api/nico-api-site-config.toml").to_string();
        for (placeholder, value) in [
            ("MANAGED_HOST_IPMI_POOL_1_GATEWAY_IP", "203.0.113.1"),
            ("MANAGED_HOST_IPMI_POOL_1", "203.0.113.0/24"),
            ("CONTROL_PLANE_IPMI_POOL_1", "198.51.100.0/24"),
            ("ADMIN_NETWORK_GATEWAY_IP", "192.0.2.1"),
            ("ADMIN_NETWORK_IP_POOL", "192.0.2.0/24"),
            ("DPU_LOOPBACK_START_IP", "10.180.62.1"),
            ("DPU_LOOPBACK_END_IP", "10.180.62.62"),
            ("NICO_DHCP_EXTERNAL_IP", "192.0.2.10"),
            ("SITE_FABRIC_PREFIX_1", "10.0.0.0/8"),
            ("ENVIORNMENT_NAME", "test"),
        ] {
            config = config.replace(placeholder, value);
        }
        config
    }

    #[test]
    fn deserialize_shipped_api_configurations() {
        let repository_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let deploy_path =
            format!("{repository_root}/deploy/nico-base/api/config-files/carbide-api-config.toml");
        let docker_config = include_str!("../../../../dev/docker-env/carbide-api-config.toml");
        let webdev_config = include_str!("../../../../dev/webdev-env/carbide-api-config.toml");
        let site_config = rendered_deployment_site_config();
        let helm_config = rendered_helm_api_config();

        let deploy_config = Figment::new()
            .merge(Toml::file(&deploy_path))
            .extract::<CarbideConfig>()
            .expect("the shipped deployment config must match the strict schema");
        assert_eq!(
            deploy_config.dpu_config.dpu_nic_firmware_update_versions,
            [BF2_NIC_VERSION.to_string(), BF3_NIC_VERSION.to_string()]
        );

        for (name, figment) in [
            (
                "deployment base plus site override",
                Figment::new()
                    .merge(Toml::file(&deploy_path))
                    .merge(Toml::string(&site_config)),
            ),
            (
                "Docker development",
                Figment::new()
                    .merge(Toml::string(docker_config))
                    .merge(Toml::string(
                        r#"database_url = "postgres://test:test@localhost/test""#,
                    )),
            ),
            (
                "web development",
                Figment::new().merge(Toml::string(webdev_config)),
            ),
            (
                "rendered Helm",
                Figment::new().merge(Toml::string(&helm_config)),
            ),
        ] {
            figment.extract::<CarbideConfig>().unwrap_or_else(|error| {
                panic!("{name} config must match the strict schema: {error}")
            });
        }
    }

    #[test]
    #[allow(clippy::result_large_err)] // complains about figma::Error which we don't control
    fn deserialize_env_patched_full_config() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("CARBIDE_API_DATABASE_URL", "postgres://othersql");
            jail.set_env("CARBIDE_API_ASN", 777);
            jail.set_env("CARBIDE_API_SCOUT_BOOT_INTERFACE_CORRECTION_ENABLED", true);
            jail.set_env("CARBIDE_API_AUTH", "{permissive_mode=true}");
            jail.set_env(
                "CARBIDE_API_DSX_EXCHANGE_EVENT_BUS",
                r#"{enabled=true,mqtt_endpoint="dsx-exchange",mqtt_broker_port=1883,auth={auth_mode="none"},periodic_state_republish={interval="10s"}}"#,
            );
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
            assert!(config.scout_boot_interface_correction_enabled);
            assert_eq!(
                config.credentials,
                CredentialsConfig {
                    ufm_source: UfmCredentialSource::Backend,
                    file: Some(CredentialFileSourceConfig {
                        path: PathBuf::from("/var/run/secrets/nico/ufm/credentials.yaml"),
                        poll_interval: std::time::Duration::from_secs(17),
                    }),
                }
            );
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
            let dsx_exchange = config.dsx_exchange_event_bus.as_ref().unwrap();
            assert!(dsx_exchange.enabled);
            assert_eq!(dsx_exchange.mqtt_endpoint, "dsx-exchange");
            assert_eq!(dsx_exchange.mqtt_broker_port, 1883);
            assert_eq!(dsx_exchange.auth.auth_mode, MqttAuthMode::None);
            assert_eq!(
                dsx_exchange.periodic_state_republish.interval,
                std::time::Duration::from_secs(10)
            );
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
    #[allow(clippy::result_large_err)]
    fn deserialize_unknown_environment_field_is_rejected() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("CARBIDE_API_UNKNOWN_FIELD", true);

            let error = Figment::new()
                .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                .merge(Env::prefixed("CARBIDE_API_"))
                .extract::<CarbideConfig>()
                .unwrap_err();

            assert!(matches!(
                &error.kind,
                Kind::UnknownField(field, _) if field == "unknown_field"
            ));
            assert_eq!(error.path, vec!["unknown_field".to_string()]);
            Ok(())
        })
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn deserialize_unknown_nested_environment_field_is_rejected() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("CARBIDE_API_SITE_EXPLORER", "{unknown_nested_field=true}");

            let error = Figment::new()
                .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                .merge(Env::prefixed("CARBIDE_API_"))
                .extract::<CarbideConfig>()
                .unwrap_err();

            assert!(matches!(
                &error.kind,
                Kind::UnknownField(field, _) if field == "unknown_nested_field"
            ));
            assert_eq!(
                error.path,
                vec![
                    "site_explorer".to_string(),
                    "unknown_nested_field".to_string()
                ]
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

    /// Every hardware class SiteExplorer can identify is ingested by default:
    /// a config whose `[site_explorer]` section omits the creation flags gets
    /// the same behavior as one with no section at all. Creation stays gated
    /// per device on a matching expected-hardware record, so these defaults
    /// only ingest declared hardware.
    #[test]
    fn site_explorer_creation_flags_default_on() -> eyre::Result<()> {
        let config = serde_json::from_str::<SiteExplorerConfig>("{}")?;
        assert!(config.create_machines.load(AtomicOrdering::Relaxed));
        assert!(config.create_switches.load(AtomicOrdering::Relaxed));
        assert!(config.create_power_shelves.load(AtomicOrdering::Relaxed));
        Ok(())
    }

    /// Verifies the canonical `[site_explorer] dpu_policy = ...` setting and
    /// legacy `dpu_mode` spelling parse correctly. When unset, hosts ultimately
    /// resolve to `HostDpuPolicy::Manage`.
    #[test]
    fn site_explorer_dpu_policy_parses_and_defaults_to_none() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .extract()
            .unwrap();
        assert_eq!(config.site_explorer.dpu_policy, None);

        scenarios!(
            run = |toml_setting| {
                Figment::new()
                    .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                    .merge(Toml::string(&format!(
                        "[site_explorer]\n{toml_setting}\n"
                    )))
                    .extract::<CarbideConfig>()
                    .map(|config| config.site_explorer.dpu_policy)
                    .map_err(drop)
            };
            "canonical policy values" {
                "dpu_policy = \"manage\"" => Yields(Some(HostDpuPolicy::Manage)),
                "dpu_policy = \"nic\"" => Yields(Some(HostDpuPolicy::Nic)),
                "dpu_policy = \"ignore\"" => Yields(Some(HostDpuPolicy::Ignore)),
            }

            "compatibility values" {
                "dpu_policy = \"use_as_nic\"" => Yields(Some(HostDpuPolicy::Nic)),
                "dpu_mode = \"dpu_mode\"" => Yields(Some(HostDpuPolicy::Manage)),
                "dpu_mode = \"nic_mode\"" => Yields(Some(HostDpuPolicy::Nic)),
                "dpu_mode = \"no_dpu\"" => Yields(Some(HostDpuPolicy::Ignore)),
            }
        );
    }

    #[test]
    fn site_explorer_dpu_policy_serializes_only_canonical_key() {
        scenarios!(
            run = |dpu_policy| {
                let config = SiteExplorerConfig {
                    dpu_policy,
                    ..SiteExplorerConfig::default()
                };

                serde_json::to_value(config)
                    .map(|serialized| {
                        (
                            serialized.get("dpu_policy").cloned(),
                            serialized.get("dpu_mode").cloned(),
                        )
                    })
                    .map_err(drop)
            };
            "canonical key only" {
                None => Yields((Some(serde_json::Value::Null), None)),
                Some(HostDpuPolicy::Manage) => Yields((
                    Some(serde_json::Value::String("manage".to_string())),
                    None,
                )),
                Some(HostDpuPolicy::Nic) => Yields((
                    Some(serde_json::Value::String("nic".to_string())),
                    None,
                )),
                Some(HostDpuPolicy::Ignore) => Yields((
                    Some(serde_json::Value::String("ignore".to_string())),
                    None,
                )),
            }
        );
    }

    #[test]
    fn dpu_config_restart_ovs_on_use_admin_network_change_parses_and_displays() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(
                "[dpu_config]\nrestart_ovs_on_use_admin_network_change = true\n",
            ))
            .extract()
            .unwrap();

        assert!(config.dpu_config.restart_ovs_on_use_admin_network_change);

        let runtime_config: rpc::forge::RuntimeConfig = config.into();
        assert!(runtime_config.restart_ovs_on_use_admin_network_change);
    }

    /// Real-world site TOMLs may still carry the now-removed
    /// `force_dpu_nic_mode` setting (top-level and/or under
    /// `[site_explorer]`). Keep that one compatibility exception explicit.
    #[test]
    fn legacy_force_dpu_nic_mode_in_toml_still_parses() {
        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(
                "force_dpu_nic_mode = false\n\
                 [site_explorer]\n\
                 force_dpu_nic_mode = true\n",
            ))
            .extract()
            .expect("legacy force_dpu_nic_mode in TOML must still parse");

        assert_eq!(config.deprecated_force_dpu_nic_mode, Some(false));
        assert_eq!(
            config.site_explorer.deprecated_force_dpu_nic_mode,
            Some(true)
        );
    }

    #[test]
    fn carbide_config_rejects_unknown_fields_across_fixed_schema_levels() {
        scenarios!(
            run = |patch| Figment::new()
                .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
                .merge(Toml::string(patch))
                .extract::<CarbideConfig>()
                .map(drop)
                .map_err(|error| match error.kind {
                    Kind::UnknownField(field, _) => field,
                    other => panic!("expected an unknown-field rejection, got {other:?}"),
                });

            "unknown fields are rejected" {
                "unknown_top_level = true" => Fails,
                "rapid_iterations = true" => Fails,
                "nvue_enabled = false" => Fails,
                "[site_explorer]\nunknown_site_explorer_field = true" => Fails,
                "[tracing]\nunknown_tracing_field = true" => Fails,
                "[machine_state_controller]\nunknown_machine_controller_field = true" => Fails,
                "[machine_state_controller.controller]\nunknown_controller_field = true" => Fails,
                "[auth]\npermissive_mode = true\nunknown_auth_field = true" => Fails,
                "[pools.test-pool]\ntype = \"integer\"\nunknown_pool_field = true" => Fails,
                "[dpu_config]\nunknown_dpu_field = true" => Fails,
                "[fnn]\nunknown_fnn_field = true" => Fails,
                "[fnn.routing_profiles.test]\nunknown_routing_profile_field = true" => Fails,
                "[host_health]\nunknown_host_health_field = true" => Fails,
                "[firmware_global]\nunknown_firmware_field = true" => Fails,
                "[machine_validation_config]\nunknown_validation_field = true" => Fails,
                "[network_security_group]\nunknown_nsg_field = true" => Fails,
                "[machine_identity]\nunknown_identity_field = true" => Fails,
                "[spdm]\nunknown_spdm_field = true" => Fails,
                "[component_manager]\nunknown_component_manager_field = true" => Fails,
                "[dpa_config]\nunknown_dpa_field = true" => Fails,
                "[dsx_exchange_event_bus]\nunknown_event_bus_field = true" => Fails,
                "[host_models.test-model]\nvendor = \"Dell\"\nmodel = \"test\"\ncomponents = {}\nunknown_host_model_field = true" => Fails,
                "[supernic_firmware_profiles.part-number.psid]\npart_number = \"part-number\"\npsid = \"psid\"\nversion = \"1.0\"\nfirmware_url = \"https://example.com/fw.bin\"\nunknown_supernic_field = true" => Fails,
                "[mlx-config-profiles.test]\nname = \"test\"\nregistry_name = \"mlx_generic\"\nconfig = {}\nunknown_mlx_profile_field = true" => Fails,
            }

            "dynamic map keys and valid partial sections remain accepted" {
                "[pools.an-arbitrary-pool-name]\ntype = \"integer\"" => Yields(()),
                "[tracing]\nenabled = true" => Yields(()),
            }
        );
    }

    #[test]
    fn network_security_policy_override_rejects_unknown_rule_fields() {
        const VALID_POLICY: &str = r#"
[[network_security_group.policy_overrides]]
src_net = { Prefix = "0.0.0.0/0" }
dst_net = { Prefix = "0.0.0.0/0" }
direction = "Ingress"
ipv6 = false
protocol = "Any"
action = "Deny"
priority = 1
"#;

        let config = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(VALID_POLICY))
            .extract::<CarbideConfig>()
            .expect("valid policy override parses");
        assert_eq!(config.network_security_group.policy_overrides.len(), 1);

        let invalid_policy =
            VALID_POLICY.replace("priority = 1", "priority = 1\nmisspelled_priority = 2");
        let error = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(&invalid_policy))
            .extract::<CarbideConfig>()
            .unwrap_err();
        assert!(matches!(
            &error.kind,
            Kind::UnknownField(field, _) if field == "misspelled_priority"
        ));
        assert_eq!(
            error.path,
            vec![
                "network_security_group".to_string(),
                "policy_overrides".to_string(),
                "0".to_string(),
                "misspelled_priority".to_string()
            ]
        );
    }

    #[test]
    fn unknown_field_error_identifies_key_and_section() {
        let error = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/min_config.toml")))
            .merge(Toml::string(
                "[site_explorer]\nunknown_site_explorer_field = true",
            ))
            .extract::<CarbideConfig>()
            .unwrap_err();

        assert!(matches!(
            &error.kind,
            Kind::UnknownField(field, _) if field == "unknown_site_explorer_field"
        ));
        assert_eq!(
            error.path,
            vec![
                "site_explorer".to_string(),
                "unknown_site_explorer_field".to_string()
            ]
        );
        assert!(
            error
                .to_string()
                .contains("site_explorer.unknown_site_explorer_field")
        );
    }

    #[test]
    fn initial_objects_config_rejects_unknown_fields() {
        scenarios!(
            run = |input| Figment::new()
                .merge(Toml::string(input))
                .extract::<InitialObjectsConfig>()
                .map(drop)
                .map_err(|error| match error.kind {
                    Kind::UnknownField(field, _) => field,
                    other => panic!("expected an unknown-field rejection, got {other:?}"),
                });

            "unknown fields are rejected" {
                "unknown_top_level = true" => Fails,
                "[pools.test-pool]\ntype = \"integer\"\nunknown_pool_field = true" => Fails,
                "[networks.admin]\ntype = \"admin\"\nprefix = \"172.20.0.0/24\"\ngateway = \"172.20.0.1\"\nmtu = 9000\nreserve_first = 5\nunknown_network_field = true" => Fails,
            }

            "dynamic object names remain accepted" {
                "[pools.an-arbitrary-pool-name]\ntype = \"integer\"" => Yields(()),
            }
        );
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
svpc_enabled=true

[svpc]
mqtt_endpoint = "mqtt.forge"
        "#;

        let dpa_config: EwEthersConfig =
            Figment::new().merge(Toml::string(toml)).extract().unwrap();

        assert_eq!(
            dpa_config,
            EwEthersConfig {
                enabled: true,
                svpc_enabled: true,
                astra_enabled: false,
                monitor_run_interval: std::time::Duration::from_secs(60),
                subnet_ip: Ipv4Addr::UNSPECIFIED,
                subnet_mask: 0_i32,
                svpc: SvpcConfig {
                    mqtt_endpoint: "mqtt.forge".to_string(),
                    mqtt_broker_port: 1884,
                    hb_interval: chrono::TimeDelta::minutes(2),
                    auth: MqttAuthConfig::default(),
                },
            }
        );
    }

    #[test]
    fn deserialize_dpu_config() {
        let toml = r#"
[dpu_config]
bootstrap_ca_source = "embedded"
dpu_enable_secure_boot = true
num_of_vfs = 64
service_vpc_slot_count = 5
additional_managed_sf = 2
"#;

        let config: CarbideConfig = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .merge(Toml::string(toml))
            .extract()
            .unwrap();

        assert_eq!(
            config.dpu_config.bootstrap_ca_source,
            BootstrapCaSource::Embedded
        );
        assert!(config.dpu_config.dpu_enable_secure_boot);
        assert_eq!(config.dpu_config.num_of_vfs, 64);
        assert_eq!(config.dpu_config.service_vpc_slot_count, 5);
        assert_eq!(config.dpu_config.additional_managed_sf, 2);
        assert!(!config.dpu_config.dpu_models.is_empty());
    }

    #[test]
    fn deserialize_dpu_config_rejects_unknown_bootstrap_ca_source() {
        let toml = r#"
[dpu_config]
bootstrap_ca_source = "download"
"#;

        let error = Figment::new()
            .merge(Toml::file(format!("{TEST_DATA_DIR}/full_config.toml")))
            .merge(Toml::string(toml))
            .extract::<CarbideConfig>()
            .unwrap_err();

        assert!(error.to_string().contains("bootstrap_ca_source"), "{error}");
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
    fn deserialize_dpf_dpu_agent_bootstrap_ca_sources() {
        check_values(
            [
                Check {
                    scenario: "default legacy download",
                    input: "",
                    expect: Ok((DpfDpuAgentBootstrapCa::default(), Ok(()))),
                },
                Check {
                    scenario: "custom legacy endpoint",
                    input: r#"
[dpu_agent_bootstrap_ca]
source = "legacy_download"
url = "https://pxe.example.test/site-ca.pem"
"#,
                    expect: Ok((
                        DpfDpuAgentBootstrapCa::LegacyDownload {
                            url: Some(
                                url::Url::parse("https://pxe.example.test/site-ca.pem").unwrap(),
                            ),
                        },
                        Ok(()),
                    )),
                },
                Check {
                    scenario: "mounted ConfigMap CA",
                    input: r#"
[dpu_agent_bootstrap_ca]
source = "mounted"
object_kind = "config_map"
name = "nico-bootstrap-ca-v1"
"#,
                    expect: Ok((
                        DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                            name: "nico-bootstrap-ca-v1".to_string(),
                            key: "ca.crt".to_string(),
                        },
                        Ok(()),
                    )),
                },
            ],
            |input| {
                toml::from_str::<DpfConfig>(input)
                    .map_err(|error| error.to_string())
                    .map(|config| {
                        let validation = config.dpu_agent_bootstrap_ca.validate();
                        (config.dpu_agent_bootstrap_ca, validation)
                    })
            },
        );
    }

    #[test]
    fn deserialize_dpf_dpu_agent_bootstrap_ca_rejects_unknown_fields() {
        let error = toml::from_str::<DpfConfig>(
            r#"
[dpu_agent_bootstrap_ca]
source = "legacy_download"
object_kind = "secret"
"#,
        )
        .unwrap_err();

        assert!(error.to_string().contains("unknown field `object_kind`"));
    }

    /// Verifies deployment scoping is opt-in and is mandatory for BF4+CX9 Astra.
    #[test]
    fn dpf_service_interface_scoping_gates_astra() {
        // Serde omission must preserve the legacy namespace-wide mode.
        assert!(
            !toml::from_str::<DpfConfig>("")
                .unwrap()
                .deployment_scoped_service_interfaces
        );

        value_scenarios!(
            run = |(deployment_scoped_service_interfaces, has_astra)| {
                let mut config = DpfConfig {
                    deployment_scoped_service_interfaces,
                    ..Default::default()
                };
                config.deployments.bf4_astra = has_astra.then(DpfDeploymentConfig::default);
                config.validate_service_interface_scoping().is_ok()
            };
            "legacy default" {
                // Omission preserves existing global ServiceInterfaces without a scoping migration.
                (false, false) => true,
            }

            "explicit scoped mode" {
                // Operators opt into the migration independently of Astra enablement.
                (true, false) => true,
            }

            "unsafe Astra mode" {
                // Astra's distinct inventory cannot be represented by legacy global resources.
                (false, true) => false,
            }

            "scoped Astra mode" {
                // The BF4+CX9 deployment is valid only after opting into isolated resources.
                (true, true) => true,
            }
        );
    }

    /// Verifies the reserved SF setting preserves the legacy pool by default while allowing an
    /// operator to reserve additional platform-specific capacity.
    #[test]
    fn dpf_pf_total_sf_reserved_defaults_and_deserializes() {
        // Exercise omission and an explicit override through the operator-facing TOML contract.
        value_scenarios!(
            run = |input| toml::from_str::<DpfConfig>(input).unwrap().pf_total_sf_reserved;
            "omitted reserve" {
                // Omission must retain the existing PF_TOTAL_SF value for inventory-free sites.
                "" => carbide_dpf::DEFAULT_PF_TOTAL_SF_RESERVED,
            }

            "explicit reserve" {
                // Operators may size headroom for their DPF and firmware consumers.
                "pf_total_sf_reserved = 47" => 47,
            }
        );

        // Programmatic defaults must match deserialization defaults used by production config.
        assert_eq!(
            DpfConfig::default().pf_total_sf_reserved,
            carbide_dpf::DEFAULT_PF_TOTAL_SF_RESERVED
        );
    }

    #[test]
    fn empty_dpf_service_uses_its_defaults() {
        let config = toml::from_str::<DpfConfig>("[services.dpu_agent]").unwrap();
        let expected_agent = crate::dpf_services::default_dpu_agent_service();
        let expected_fmds = crate::dpf_services::default_fmds_service();

        assert_eq!(config.services.dpu_agent.name, expected_agent.name);
        assert_eq!(
            config.services.dpu_agent.helm_chart,
            expected_agent.helm_chart
        );
        assert_eq!(config.services.fmds.name, expected_fmds.name);
    }

    #[test]
    fn top_level_dpf_service_overlays_its_defaults() {
        let config = toml::from_str::<DpfConfig>(
            r#"
[services.dpu_agent]
helm_version = "configured-version"

[services.dpu_agent.extra_helm_values.fmds]
sign_proxy_url = "http://dsx-imds.dpf-operator-system.svc.cluster.local:8080"
"#,
        )
        .unwrap();
        let expected = crate::dpf_services::default_dpu_agent_service();

        assert_eq!(config.services.dpu_agent.name, expected.name);
        assert_eq!(
            config.services.dpu_agent.docker_repo_url,
            expected.docker_repo_url
        );
        assert_eq!(config.services.dpu_agent.helm_version, "configured-version");
        assert_eq!(
            config.services.dpu_agent.extra_helm_values.unwrap()["fmds"]["sign_proxy_url"],
            "http://dsx-imds.dpf-operator-system.svc.cluster.local:8080"
        );
    }

    #[test]
    fn dpf_extra_bfcfg_parameters_reach_the_config_verbatim() {
        // The strings reach bf.cfg unquoted and uninterpreted, so parsing must not alter them.
        let parse = |body: &str| {
            toml::from_str::<DpfConfig>(body)
                .expect("dpf config must parse")
                .extra_bfcfg_parameters
        };

        value_scenarios!(
            run = |body: &str| parse(body);

            "absent key defaults to no extra parameters" {
                "enabled = true\n" => Vec::<String>::new(),
            }

            "explicit empty list is accepted" {
                "extra_bfcfg_parameters = []\n" => Vec::<String>::new(),
            }

            "a quoted password hash survives parsing unchanged" {
                // Single-quoted in bf.cfg so the hash's `$` sections are not shell-expanded.
                "extra_bfcfg_parameters = [\"ubuntu_PASSWORD='$6$sa.lt$h/a.sh'\"]\n"
                    => vec!["ubuntu_PASSWORD='$6$sa.lt$h/a.sh'".to_string()],
            }

            "configured order is preserved" {
                "extra_bfcfg_parameters = [\"FIRST=1\", \"SECOND=2\"]\n"
                    => vec!["FIRST=1".to_string(), "SECOND=2".to_string()],
            }
        );
    }

    #[test]
    fn deployment_bfcfg_parameters_append_to_the_site_wide_list() {
        // Append, not override, so a deployment setting its own parameter keeps the site-wide
        // list rather than replacing it.
        let resolved = |body: &str| {
            let config = toml::from_str::<DpfConfig>(body).expect("dpf config must parse");
            config.resolved_bfcfg_parameters_for(&config.deployments.bf3)
        };
        let deployment_keys = "\nflavor_name = \"f\"\ndeployment_name = \"d\"\n\
                               node_label_key = \"k\"\n";

        value_scenarios!(
            run = |body: &str| resolved(body);

            "neither level configured yields nothing" {
                "enabled = true\n" => Vec::<String>::new(),
            }

            "site-wide only applies to the deployment" {
                "extra_bfcfg_parameters = [\"SITE=1\"]\n" => vec!["SITE=1".to_string()],
            }

            "deployment-only applies without a site-wide list" {
                &format!("[deployments.bf3]{deployment_keys}\
                          extra_bfcfg_parameters = [\"OWN=1\"]\n")
                    => vec!["OWN=1".to_string()],
            }

            "site-wide comes first, then the deployment's own" {
                &format!("extra_bfcfg_parameters = [\"SITE=1\", \"SITE=2\"]\n\
                          [deployments.bf3]{deployment_keys}\
                          extra_bfcfg_parameters = [\"OWN=1\"]\n")
                    => vec!["SITE=1".to_string(), "SITE=2".to_string(), "OWN=1".to_string()],
            }

            "a deployment list does not replace the site-wide one" {
                // A site-wide password survives a deployment adding a parameter of its own.
                &format!("extra_bfcfg_parameters = [\"ubuntu_PASSWORD='$6$sa.lt$h/a.sh'\"]\n\
                          [deployments.bf3]{deployment_keys}\
                          extra_bfcfg_parameters = [\"OWN=1\"]\n")
                    => vec![
                        "ubuntu_PASSWORD='$6$sa.lt$h/a.sh'".to_string(),
                        "OWN=1".to_string(),
                    ],
            }
        );
    }

    #[test]
    fn dpf_service_helm_values_require_a_table() {
        for value in ["true", "[\"value\"]"] {
            let config = format!("[services.dpu_agent]\nextra_helm_values = {value}\n");

            assert!(toml::from_str::<DpfConfig>(&config).is_err(), "{value}");
        }
    }

    #[test]
    fn deployment_dpf_service_overlays_its_defaults() {
        let config = toml::from_str::<DpfConfig>(
            r#"
[deployments.bf4_generic]
flavor_name = "bf4-flavor"
deployment_name = "bf4-deployment"
node_label_key = "carbide.nvidia.com/bf4"

[deployments.bf4_generic.services.dpu_agent]
helm_version = "configured-version"

[deployments.bf4_generic.services.dpu_agent.extra_helm_values.fmds]
sign_proxy_url = "http://bf4-dsx-imds.dpf-operator-system.svc.cluster.local:8080"

[deployments.bf4_generic.services.fmds]
"#,
        )
        .unwrap();
        let services = config.deployments.bf4_generic.unwrap().services.unwrap();
        let expected_agent = crate::dpf_services::default_dpu_agent_service();
        let expected_fmds = crate::dpf_services::default_fmds_service();

        assert_eq!(services.dpu_agent.name, expected_agent.name);
        assert_eq!(services.dpu_agent.helm_version, "configured-version");
        assert_eq!(services.fmds.name, expected_fmds.name);
        assert_eq!(
            services.dpu_agent.extra_helm_values.unwrap()["fmds"]["sign_proxy_url"],
            "http://bf4-dsx-imds.dpf-operator-system.svc.cluster.local:8080"
        );
    }

    #[test]
    fn dpf_deployment_extra_services_are_configurable() {
        let config = toml::from_str::<DpfConfig>(
            r#"
[deployments.bf4_astra]
flavor_name = "astra-flavor"
deployment_name = "astra-deployment"
node_label_key = "carbide.nvidia.com/astra"

[extra_services.doca_weave_dhcp_agent]
docker_image_tag = "site-tag"

[deployments.bf4_astra.extra_services.doca_weave_dhcp_agent]
helm_version = "development-version"

[deployments.bf4_astra.extra_services.doca_weave_flow_controller]
helm_version = "flow-controller-dev"
"#,
        )
        .unwrap();

        let deployment = config.deployments.bf4_astra.as_ref().unwrap();
        let configured = deployment
            .extra_services
            .get(&DpfExtraService::DocaWeaveDhcpAgent)
            .unwrap();
        assert_eq!(
            configured.helm_version.as_deref(),
            Some("development-version")
        );

        let resolved = config.resolved_services_for(deployment, DpuDeploymentType::Bf4Astra);
        assert_eq!(
            resolved
                .extra
                .get(&DpfExtraService::DocaWeaveDhcpAgent)
                .unwrap()
                .docker_image_tag,
            "site-tag"
        );
        assert_eq!(
            resolved
                .extra
                .get(&DpfExtraService::DocaWeaveFlowController)
                .unwrap()
                .helm_version,
            "flow-controller-dev"
        );
        assert_eq!(
            resolved
                .extra
                .get(&DpfExtraService::DocaXplane)
                .unwrap()
                .helm_version,
            crate::dpf_services::default_doca_xplane_service().helm_version
        );
    }

    #[test]
    fn bf4_astra_extra_services_default_without_toml() {
        let config = toml::from_str::<DpfConfig>(
            r#"
[deployments.bf4_astra]
flavor_name = "astra-flavor"
deployment_name = "astra-deployment"
node_label_key = "carbide.nvidia.com/astra"
"#,
        )
        .unwrap();
        let deployment = config.deployments.bf4_astra.as_ref().unwrap();
        let resolved = config.resolved_services_for(deployment, DpuDeploymentType::Bf4Astra);
        let dhcp_agent = resolved
            .extra
            .get(&DpfExtraService::DocaWeaveDhcpAgent)
            .unwrap();
        assert_eq!(
            dhcp_agent.name,
            carbide_dpf::types::DOCA_WEAVE_DHCP_AGENT_SERVICE_NAME
        );
        assert_eq!(
            dhcp_agent.helm_version,
            crate::dpf_services::DOCA_WEAVE_DHCP_AGENT_SERVICE_HELM_VERSION
        );
        let flow_controller = resolved
            .extra
            .get(&DpfExtraService::DocaWeaveFlowController)
            .unwrap();
        assert_eq!(
            flow_controller.name,
            carbide_dpf::types::DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_NAME
        );
        assert_eq!(
            flow_controller.helm_version,
            crate::dpf_services::DOCA_WEAVE_FLOW_CONTROLLER_SERVICE_HELM_VERSION
        );
        let xplane = resolved.extra.get(&DpfExtraService::DocaXplane).unwrap();
        assert_eq!(xplane.name, carbide_dpf::types::DOCA_XPLANE_SERVICE_NAME);
        assert_eq!(
            xplane.helm_version,
            crate::dpf_services::default_doca_xplane_service().helm_version
        );
    }

    #[test]
    fn site_extra_services_are_configurable_only_for_supported_deployments() {
        let config = toml::from_str::<DpfConfig>(
            r#"
docker_image_pull_secret = "top-level-pull-secret"

[extra_services.doca_weave_dhcp_agent]
helm_version = "site-weave-version"
docker_image_pull_secret = "site-dpf-image-pull-secret"

[extra_services.doca_weave_flow_controller]
helm_repo_url = "oci://registry.example.test/doca"
"#,
        )
        .unwrap();

        let deployment = DpfDeploymentConfig::default();
        for deployment_type in [
            DpuDeploymentType::Bf3,
            DpuDeploymentType::Bf3Gb200,
            DpuDeploymentType::Bf4Generic,
        ] {
            assert!(
                config
                    .resolved_services_for(&deployment, deployment_type)
                    .extra
                    .is_empty()
            );
        }

        let astra = config.resolved_services_for(&deployment, DpuDeploymentType::Bf4Astra);
        assert_eq!(
            astra.extra[&DpfExtraService::DocaWeaveDhcpAgent].helm_version,
            "site-weave-version"
        );
        assert_eq!(
            astra.extra[&DpfExtraService::DocaWeaveDhcpAgent]
                .docker_image_pull_secret
                .as_deref(),
            Some("site-dpf-image-pull-secret")
        );
        assert_eq!(
            astra.extra[&DpfExtraService::DocaWeaveFlowController].helm_repo_url,
            "oci://registry.example.test/doca"
        );
        for service in [
            DpfExtraService::DocaWeaveFlowController,
            DpfExtraService::DocaXplane,
        ] {
            assert_eq!(
                astra.extra[&service].docker_image_pull_secret.as_deref(),
                Some("top-level-pull-secret")
            );
        }
    }

    #[test]
    fn dpf_dpu_agent_bootstrap_ca_validation_rejects_unsafe_values() {
        struct ValidationInput {
            policy: DpfDpuAgentBootstrapCa,
            expected_error: &'static str,
        }

        check_values(
            [
                Check {
                    scenario: "unsupported URL scheme",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::LegacyDownload {
                            url: Some(url::Url::parse("file:///tmp/site-ca.pem").unwrap()),
                        },
                        expected_error: "must use http or https",
                    },
                    expect: true,
                },
                Check {
                    scenario: "empty object name",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::Secret,
                            name: "  ".to_string(),
                            key: "ca.crt".to_string(),
                        },
                        expected_error: "name must not be empty",
                    },
                    expect: true,
                },
                Check {
                    scenario: "empty object key",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                            name: "nico-bootstrap-ca".to_string(),
                            key: String::new(),
                        },
                        expected_error: "key must not be empty",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object name containing uppercase characters",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::Secret,
                            name: "Nico-bootstrap-ca".to_string(),
                            key: "ca.crt".to_string(),
                        },
                        expected_error: "name must be a valid Kubernetes DNS subdomain",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object name with an empty DNS label",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                            name: "nico..bootstrap-ca".to_string(),
                            key: "ca.crt".to_string(),
                        },
                        expected_error: "name must be a valid Kubernetes DNS subdomain",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object name longer than the Kubernetes limit",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                            name: "a".repeat(KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH + 1),
                            key: "ca.crt".to_string(),
                        },
                        expected_error: "name must be a valid Kubernetes DNS subdomain",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object key containing a path separator",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::Secret,
                            name: "nico-bootstrap-ca".to_string(),
                            key: "certs/ca.crt".to_string(),
                        },
                        expected_error: "key must be a valid Kubernetes Secret or ConfigMap data key",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object key using a reserved parent-directory prefix",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                            name: "nico-bootstrap-ca".to_string(),
                            key: "..ca.crt".to_string(),
                        },
                        expected_error: "key must be a valid Kubernetes Secret or ConfigMap data key",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object key using the reserved current-directory name",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::Secret,
                            name: "nico-bootstrap-ca".to_string(),
                            key: ".".to_string(),
                        },
                        expected_error: "key must be a valid Kubernetes Secret or ConfigMap data key",
                    },
                    expect: true,
                },
                Check {
                    scenario: "object key longer than the Kubernetes limit",
                    input: ValidationInput {
                        policy: DpfDpuAgentBootstrapCa::Mounted {
                            object_kind: DpfBootstrapCaObjectKind::Secret,
                            name: "nico-bootstrap-ca".to_string(),
                            key: "a".repeat(KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH + 1),
                        },
                        expected_error: "key must be a valid Kubernetes Secret or ConfigMap data key",
                    },
                    expect: true,
                },
            ],
            |ValidationInput {
                 policy,
                 expected_error,
             }| {
                policy
                    .validate()
                    .is_err_and(|error| error.contains(expected_error))
            },
        );
    }

    #[test]
    fn dpf_dpu_agent_bootstrap_ca_validation_accepts_kubernetes_references() {
        check_values(
            [
                Check {
                    scenario: "dotted object name and default-style key",
                    input: ("nico.bootstrap-ca-v1".to_string(), "ca.crt".to_string()),
                    expect: Ok(()),
                },
                Check {
                    scenario: "maximum-length object name and key",
                    input: (
                        "a".repeat(KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH),
                        "A".repeat(KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH),
                    ),
                    expect: Ok(()),
                },
                Check {
                    scenario: "uppercase and underscore in data key",
                    input: ("nico-bootstrap-ca".to_string(), ".SITE_CA-V1".to_string()),
                    expect: Ok(()),
                },
            ],
            |(name, key)| {
                DpfDpuAgentBootstrapCa::Mounted {
                    object_kind: DpfBootstrapCaObjectKind::ConfigMap,
                    name,
                    key,
                }
                .validate()
            },
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
        for secret in [
            &services.dpu_agent.docker_image_pull_secret,
            &services.dhcp_server.docker_image_pull_secret,
            &services.fmds.docker_image_pull_secret,
            &services.otel.docker_image_pull_secret,
        ] {
            assert_eq!(secret.as_deref(), Some("my-custom-secret"));
        }

        // ... except dts and doca_hbn, which take a pull secret only from their
        // per-service config (and default to none).
        assert_eq!(services.dts.docker_image_pull_secret, None);
        assert_eq!(services.doca_hbn.docker_image_pull_secret, None);
    }

    #[test]
    fn dpf_docker_image_pull_secret_unset_leaves_all_services_without_a_secret() {
        // With no top-level override and no per-service value, every mandatory service
        // defaults to no pull secret (public-registry pulls) and emits no imagePullSecrets.
        let cfg = DpfConfig::default();
        assert!(cfg.docker_image_pull_secret.is_none());

        let services = cfg.resolved_mandatory_services();

        for secret in [
            &services.dpu_agent.docker_image_pull_secret,
            &services.dhcp_server.docker_image_pull_secret,
            &services.fmds.docker_image_pull_secret,
            &services.otel.docker_image_pull_secret,
            &services.dts.docker_image_pull_secret,
            &services.doca_hbn.docker_image_pull_secret,
        ] {
            assert_eq!(*secret, None);
        }
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

    fn bf4_config(
        bfb_url: Option<&str>,
        bfs: Option<DpfBlueFieldSoftwareConfig>,
    ) -> DpfDeploymentConfig {
        DpfDeploymentConfig {
            bfb_url: bfb_url.map(str::to_string),
            bluefield_software: bfs,
            flavor_name: "bf4-flavor".to_string(),
            deployment_name: "bf4-dep".to_string(),
            node_label_key: "carbide.nvidia.com/bf4".to_string(),
            services: None,
            extra_services: BTreeMap::new(),
            extra_bfcfg_parameters: Vec::new(),
        }
    }

    /// Verifies deployment selectors remain distinct from each other and NICo-owned labels.
    #[test]
    fn validate_dpf_deployment_node_label_keys() {
        // Build the smallest two-deployment configuration needed to exercise selector overlap.
        let deployments = |bf3_label: &str, bf4_label: &str| {
            let bf3 = DpfDeploymentConfig {
                node_label_key: bf3_label.to_string(),
                ..Default::default()
            };
            let bf4 = DpfDeploymentConfig {
                node_label_key: bf4_label.to_string(),
                ..bf4_config(None, None)
            };
            DpfDeploymentsConfig {
                bf3,
                bf4_generic: Some(bf4),
                bf4_astra: None,
            }
        };

        value_scenarios!(
            run = |(bf3_label, bf4_label)| deployments(bf3_label, bf4_label)
                .validate_unique_identifiers()
                .is_ok();
            "distinct deployment labels" {
                ("carbide.nvidia.com/bf3", "carbide.nvidia.com/bf4") => true,
            }

            "duplicate deployment labels" {
                ("carbide.nvidia.com/dpu", "carbide.nvidia.com/dpu") => false,
            }

            "shared DPF marker" {
                (carbide_dpf::DPU_ENABLED_NODE_LABEL, "carbide.nvidia.com/bf4") => false,
            }

            "contextual host BMC label" {
                (
                    "carbide.nvidia.com/bf4",
                    carbide_machine_controller::dpf::HOST_BMC_IP_LABEL,
                ) => false,
            }
        );
    }

    #[test]
    fn gb200_bf3_deployment_derives_distinct_identifiers_and_reuses_bf3_inputs() {
        let bf3 = DpfDeploymentConfig {
            bfb_url: Some("https://example.com/custom.bfb".to_string()),
            flavor_name: "site-bf3-flavor".to_string(),
            deployment_name: "site-bf3-deploy".to_string(),
            node_label_key: "carbide.nvidia.com/site-bf3".to_string(),
            ..Default::default()
        };

        let gb200 = bf3.bf3_gb200();
        assert_eq!(gb200.bfb_url, bf3.bfb_url);
        assert_eq!(gb200.flavor_name, "site-bf3-flavor-gb200");
        assert_eq!(gb200.deployment_name, "site-bf3-deploy-g");
        assert_eq!(gb200.node_label_key, "carbide.nvidia.com/site-bf3-gb200");
    }

    #[test]
    fn gb200_bf3_derived_identifiers_stay_within_kubernetes_limits() {
        let bf3 = DpfDeploymentConfig {
            flavor_name: "f"
                .repeat(KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH - DPF_FLAVOR_HASH_SUFFIX_LENGTH),
            deployment_name: "d".repeat(DPF_DEPLOYMENT_NAME_MAX_LENGTH),
            node_label_key: format!(
                "carbide.nvidia.com/{}",
                "n".repeat(KUBERNETES_LABEL_NAME_MAX_LENGTH)
            ),
            ..Default::default()
        };

        let gb200 = bf3.bf3_gb200();
        let label_name = gb200.node_label_key.rsplit_once('/').unwrap().1;

        assert_eq!(gb200.deployment_name.len(), DPF_DEPLOYMENT_NAME_MAX_LENGTH);
        assert_eq!(
            gb200.flavor_name.len() + DPF_FLAVOR_HASH_SUFFIX_LENGTH,
            KUBERNETES_DNS_SUBDOMAIN_MAX_LENGTH
        );
        assert_eq!(label_name.len(), KUBERNETES_LABEL_NAME_MAX_LENGTH);
        assert!(is_valid_kubernetes_label_key(&gb200.node_label_key));

        let deployments = DpfDeploymentsConfig {
            bf3,
            bf4_generic: None,
            bf4_astra: None,
        };
        assert!(deployments.validate_unique_identifiers().is_ok());
    }

    #[test]
    fn validate_provisioning_sources_accepts_exactly_one() {
        // bf3 default has bfb_url; bf4 has bluefield_software with one PSID.
        let deployments = DpfDeploymentsConfig {
            bf3: DpfDeploymentConfig::default(),
            bf4_generic: Some(bf4_config(
                None,
                Some(DpfBlueFieldSoftwareConfig {
                    os_iso: "http://example.com/os.iso".to_string(),
                    pldm_fw_bundle: BTreeMap::from([(
                        "MT_0000000884".to_string(),
                        "http://example.com/fw.pldm".to_string(),
                    )]),
                }),
            )),
            bf4_astra: None,
        };
        assert!(deployments.validate_provisioning_sources().is_ok());
    }

    #[test]
    fn validate_provisioning_sources_rejects_both_and_neither_and_empty_map() {
        // Both sources set.
        let both = DpfDeploymentsConfig {
            bf3: DpfDeploymentConfig::default(),
            bf4_generic: Some(bf4_config(
                Some("http://example.com/test.bfb"),
                Some(DpfBlueFieldSoftwareConfig {
                    os_iso: "http://example.com/os.iso".to_string(),
                    pldm_fw_bundle: BTreeMap::from([(
                        "MT_0000000884".to_string(),
                        "http://example.com/fw.pldm".to_string(),
                    )]),
                }),
            )),
            bf4_astra: None,
        };
        assert!(both.validate_provisioning_sources().is_err());

        // Neither source set.
        let neither = DpfDeploymentsConfig {
            bf3: DpfDeploymentConfig::default(),
            bf4_generic: Some(bf4_config(None, None)),
            bf4_astra: None,
        };
        assert!(neither.validate_provisioning_sources().is_err());

        // bluefield_software set but empty PSID map.
        let empty_map = DpfDeploymentsConfig {
            bf3: DpfDeploymentConfig::default(),
            bf4_generic: Some(bf4_config(
                None,
                Some(DpfBlueFieldSoftwareConfig {
                    os_iso: "http://example.com/os.iso".to_string(),
                    pldm_fw_bundle: BTreeMap::new(),
                }),
            )),
            bf4_astra: None,
        };
        assert!(empty_map.validate_provisioning_sources().is_err());
    }

    #[test]
    fn validate_provisioning_sources_rejects_bf3_bluefield_software() {
        // bf3 is BFB-only: setting bluefield_software on it is always invalid,
        // even though the same block would be valid on bf4_generic.
        let deployments = DpfDeploymentsConfig {
            bf3: bf4_config(None, Some(bf4_with_psids(&["MT_0000000884"]))),
            bf4_generic: None,
            bf4_astra: None,
        };
        assert!(deployments.validate_provisioning_sources().is_err());
    }

    fn bf4_with_psids(psids: &[&str]) -> DpfBlueFieldSoftwareConfig {
        DpfBlueFieldSoftwareConfig {
            os_iso: "http://example.com/os.iso".to_string(),
            pldm_fw_bundle: psids
                .iter()
                .map(|p| (p.to_string(), format!("http://example.com/{p}.pldm")))
                .collect(),
        }
    }

    #[test]
    fn validate_provisioning_sources_rejects_bf4_bfb_url() {
        check_values(
            [
                Check {
                    scenario: "generic BF4 cannot use a BFB",
                    input: DpfDeploymentsConfig {
                        bf3: DpfDeploymentConfig::default(),
                        bf4_generic: Some(bf4_config(Some("http://example.com/test.bfb"), None)),
                        bf4_astra: None,
                    },
                    expect: true,
                },
                Check {
                    scenario: "Astra BF4 cannot use a BFB",
                    input: DpfDeploymentsConfig {
                        bf3: DpfDeploymentConfig::default(),
                        bf4_generic: None,
                        bf4_astra: Some(bf4_config(Some("http://example.com/test.bfb"), None)),
                    },
                    expect: true,
                },
            ],
            |deployments| deployments.validate_provisioning_sources().is_err(),
        );
    }

    #[test]
    fn validate_provisioning_sources_requires_exactly_one_psid() {
        // Exactly one PSID entry is accepted.
        let one = DpfDeploymentsConfig {
            bf3: DpfDeploymentConfig::default(),
            bf4_generic: Some(bf4_config(None, Some(bf4_with_psids(&["MT_0000000884"])))),
            bf4_astra: None,
        };
        assert!(one.validate_provisioning_sources().is_ok());

        // More than one PSID is rejected (multi-PSID support is pending a DPF change).
        let many = DpfDeploymentsConfig {
            bf3: DpfDeploymentConfig::default(),
            bf4_generic: Some(bf4_config(
                None,
                Some(bf4_with_psids(&["MT_0000000884", "MT_0000000992"])),
            )),
            bf4_astra: None,
        };
        assert!(many.validate_provisioning_sources().is_err());
    }
}
