# NICo API Configuration Reference

This document describes every section and field in the `nico-api-config.toml`
configuration file, which is deserialized into `NicoConfig` (defined in
`file.rs`). Fields are listed in declaration order. Defaults are noted where
applicable.

---

## `NicoConfig` (top-level)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `listen` | `SocketAddr` | `[::]:1079` | Socket address for the gRPC API server. |
| `listen_only` | `bool` | `false` | Run passively (no background services, RPC/web only). Used in dev mode. |
| `metrics_endpoint` | `Option<SocketAddr>` | — | Socket address for the Prometheus `/metrics` HTTP server. |
| `alt_metric_prefix` | `Option<String>` | — | Alternative metric prefix emitted alongside `nico_` for dashboard migration. |
| `database_url` | `String` | **required** | Postgres connection string for all persistent state. |
| `max_database_connections` | `u32` | `1000` | Maximum database connection pool size. |
| `ib_config` | `Option<IBFabricConfig>` | — | InfiniBand fabric configuration (see [IBFabricConfig](#ibfabricconfig)). |
| `asn` | `u32` | **required** | Autonomous System Number, fixed per environment. Used by nico-dpu-agent for `frr.conf` BGP routing. |
| `dhcp_servers` | `Vec<Ipv4Addr>` | `[]` | DHCP server addresses announced to DPUs during network provisioning. |
| `ntp_servers` | `Vec<Ipv4Addr>` | `[]` | Site-level NTP server IPs used for BMC time configuration and DHCP NTP Server configuration. |
| `route_servers` | `Vec<String>` | `[]` | Route server IPs for L2VPN Ethernet Virtual network support. |
| `enable_route_servers` | `bool` | `false` | Enables route server injection into DPU FRR configs for L2VPN. |
| `deny_prefixes` | `Vec<Ipv4Network>` | `[]` | IPv4 CIDR prefixes that tenant instances are blocked from reaching. Generates iptables DROP rules and nvue ACL policies. |
| `site_fabric_prefixes` | `Vec<IpNetwork>` | `[]` | IP prefixes (v4/v6) assigned for tenant use within this site. |
| `anycast_site_prefixes` | `Vec<Ipv4Network>` | `[]` | Aggregate IPv4 prefixes containing tenant-announced prefixes (e.g., BYOIP). **Deprecated.** Use [`routing_profiles.allowed_anycast_prefixes`](#fnnroutingprofileconfig) instead. |
| `common_tenant_host_asn` | `Option<u32>` | — | ASN that tenants use to peer with the DPU. If unset, any ASN is accepted. |
| `vpc_isolation_behavior` | `VpcIsolationBehaviorType` | `MutualIsolation` | VPC isolation policy: `mutual_isolation` or `open`. |
| `host_naming_strategy` | `HostNamingStrategyKind` | `IpAddress` | How new machine hostnames are derived: `ip_address` (IP-derived, e.g. `10-1-2-3`; the default and backwards-compatible), `fun` (stable adjective-noun handles like `wholesale-walrus`), `serial_number` (a machine's hardware serial -- the primary interface gets the bare serial, secondary interfaces get `serial-<mac>`, BMC interfaces stay IP-named), or `mac_address` (each interface's own MAC, e.g. `0a-1b-2c-3d-4e-5f`). Only `fun` leaves existing hostnames unchanged -- it keeps any real name, whether IP-, serial-, or MAC-derived, so after a switch fun names appear only on newly named interfaces; the others re-derive, so switching to one progressively renames interfaces as they reconcile. Junk placeholder serials (e.g. `To Be Filled By O.E.M.`) fall back to the IP name, and `serial_number` errors on duplicate serials rather than assigning a substitute name. |
| `dpu_network_monitor_pinger_type` | `Option<String>` | — | Pinger implementation type (e.g., `"OobNetBind"`) for DPU link health checks. |
| `tls` | `Option<TlsConfig>` | — | TLS certificate/key paths (see [TlsConfig](#tlsconfig)). |
| `listen_mode` | `ListenMode` | `Tls` | Transport mode: `plaintext_http1`, `plaintext_http2`, or `tls`. |
| `auth` | `Option<AuthConfig>` | — | Authentication/authorization settings (see [AuthConfig](#authconfig)). |
| `pools` | `Option<HashMap<String, ResourcePoolDef>>` | — | Resource pools that allocate IPs, VNIs, etc. Required but `Option` for partial-config merging. |
| `networks` | `Option<HashMap<String, NetworkDefinition>>` | — | Networks created at startup. Alternative: `CreateNetworkSegment` gRPC. `NetworkDefinition` supports dual-stack seed-time segments with optional `prefix_v6` and `dhcpv6_link_address`; config edits do not retrofit prefixes onto an already-seeded segment because seed definitions are snapshotted on first create. |
| `dpu_ipmi_tool_impl` | `Option<String>` | — | IPMI tool implementation for DPU power control (`"prod"` or `"fake"`). |
| `dpu_ipmi_reboot_attempts` | `Option<u32>` | — | Retry count when IPMI errors during DPU reboot. |
| `bmc_session_lockout_threshold` | `u32` | `3` | Consecutive BMC HTTP 401/403 responses before session-token login attempts stop for that BMC. |
| `ib_fabrics` | `HashMap<String, IbFabricDefinition>` | `{}` | InfiniBand fabrics managed by the site. Currently only one fabric is supported. |
| `initial_domain_name` | `Option<String>` | — | Domain to create if none exist. Most sites use a single domain. |
| `initial_dpu_agent_upgrade_policy` | `Option<AgentUpgradePolicyChoice>` | — | Policy for nico-dpu-agent upgrades. Also settable via `nico-admin-cli`. |
| `max_concurrent_machine_updates` | `Option<i32>` | — | **Deprecated.** Use `machine_updater` instead. |
| `machine_update_run_interval` | `Option<u64>` | — | Interval (seconds) at which the machine update manager checks for updates. |
| `retained_boot_interface_window` | `Option<Duration>` | — (forever) | How long a retained boot interface pair (`retained_boot_interfaces` table) stays applicable after its `machine_interfaces` row was deleted. Unset retains forever; set a window (e.g. `30d`) so a MAC reappearing on different hardware doesn't inherit an obsolete Redfish interface id. |
| `site_explorer` | `SiteExplorerConfig` | *(see below)* | SiteExplorer hardware discovery settings (see [SiteExplorerConfig](#siteexplorerconfig)). |
| `nvue_enabled` | `bool` | `true` | DPU agent uses NVUE for config instead of writing files directly. |
| `vpc_peering_policy` | `Option<VpcPeeringPolicy>` | — | Policy for VPC peering based on network virtualization type at creation time. |
| `vpc_peering_policy_on_existing` | `Option<VpcPeeringPolicy>` | — | Policy for whether existing VPC peerings should be active. |
| `attestation_enabled` | `bool` | `false` | Enables TPM-based machine attestation (adds `Measuring` state before `Ready`). |
| `tpm_required` | `bool` | `true` | Require TPM module for machine registration. **Testing only** when `false`. |
| `machine_state_controller` | `MachineStateControllerConfig` | *(see below)* | Machine state controller timing (see [MachineStateControllerConfig](#machinestatecontrollerconfig)). |
| `network_segment_state_controller` | `NetworkSegmentStateControllerConfig` | *(see below)* | Network segment state controller timing. |
| `vpc_prefix_state_controller` | `VpcPrefixStateControllerConfig` | *(see below)* | VPC prefix state controller timing. |
| `ib_partition_state_controller` | `IbPartitionStateControllerConfig` | *(see below)* | IB partition state controller timing. |
| `dpa_interface_state_controller` | `DpaInterfaceStateControllerConfig` | *(see below)* | DPA interface state controller timing. |
| `rack_state_controller` | `RackStateControllerConfig` | *(see below)* | Rack state controller timing. |
| `power_shelf_state_controller` | `PowerShelfStateControllerConfig` | *(see below)* | Power shelf state controller timing. |
| `switch_state_controller` | `SwitchStateControllerConfig` | *(see below)* | Switch state controller timing. |
| `spdm_state_controller` | `SpdmStateControllerConfig` | *(see below)* | SPDM state controller timing. |
| `host_models` | `HashMap<String, Firmware>` | `{}` | Maps host model identifiers to firmware definitions for BMC/UEFI/NIC upgrades. |
| `firmware_global` | `FirmwareGlobal` | *(see below)* | Global firmware update settings (see [FirmwareGlobal](#firmwareglobal)). |
| `machine_updater` | `MachineUpdater` | *(see below)* | Machine update policies (see [MachineUpdater](#machineupdater)). |
| `max_find_by_ids` | `u32` | `100` | Max IDs accepted by `find_*_by_ids` APIs. |
| `network_security_group` | `NetworkSecurityGroupConfig` | *(see below)* | NSG settings (see [NetworkSecurityGroupConfig](#networksecuritygroupconfig)). |
| `min_dpu_functioning_links` | `Option<u32>` | — | Minimum functioning DPU links for healthy status. If unset, all must work. |
| `host_health` | `HostHealthConfig` | *(default)* | Host health monitoring thresholds for hardware health and DPU agent compliance. |
| `observability` | `ObservabilityConfig` | *(default)* | Observability settings shared across all state controllers (see [ObservabilityConfig](#observabilityconfig)). |
| `internet_l3_vni` | `u32` | `100001` | Network infrastructure-provided L3 VNI for FNN VPC Internet connectivity. Combined with `datacenter_asn` for route-target. |
| `measured_boot_collector` | `MeasuredBootMetricsCollectorConfig` | *(see below)* | Measured boot metrics exporter (see [MeasuredBootMetricsCollectorConfig](#measuredbootmetricscollectorconfig)). |
| `machine_validation_config` | `MachineValidationConfig` | *(see below)* | Machine validation tests (see [MachineValidationConfig](#machinevalidationconfig)). |
| `machine_identity` | `MachineIdentityConfig` | *(see below)* | SPIFFE JWT-SVID machine identity (see [MachineIdentityConfig](#machineidentityconfig)). |
| `bypass_rbac` | `bool` | `false` | Disables RBAC enforcement. **Testing/dev only.** |
| `dpu_config` | `DpuConfig` | *(see below)* | DPU firmware and provisioning (see [DpuConfig](#dpuconfig)). |
| `fnn` | `Option<FnnConfig>` | — | FNN L3 VNI overlay networking (see [FnnConfig](#fnnconfig)). |
| `bom_validation` | `BomValidationConfig` | *(see below)* | BOM/SKU validation (see [BomValidationConfig](#bomvalidationconfig)). |
| `bios_profiles` | `BiosProfileVendor` | *(default)* | BIOS profiles by vendor/model for Redfish BIOS management. |
| `selected_profile` | `BiosProfileType` | *(default)* | Default BIOS profile type applied to machines. |
| `dpa_config` | `Option<DpaConfig>` | — | Cluster Interconnect (east-west Ethernet) config (see [DpaConfig](#dpaconfig)). |
| `dsx_exchange_event_bus` | `Option<DsxExchangeEventBusConfig>` | — | MQTT event bus for managed-host state publishing plus BMS metadata subscription and rack/isolation/heartbeat publishing (see [DsxExchangeEventBusConfig](#dsxexchangeeventbusconfig)). |
| `datacenter_asn` | `u32` | `11414` | Datacenter ASN used by FNN for DC-specific route targets. |
| `nvlink_config` | `Option<NvLinkConfig>` | — | NvLink partitioning via NMX-C (see [NvLinkConfig](#nvlinkconfig)). |
| `power_manager_options` | `PowerManagerOptions` | *(see below)* | Power management timing (see [PowerManagerOptions](#powermanageroptions)). |
| `sitename` | `Option<String>` | — | Human-readable site name exposed to tenants via FMDS. |
| `auto_machine_repair_plugin` | `AutoMachineRepairPluginConfig` | *(default)* | Auto-repair configuration for failed machines. |
| `vmaas_config` | `Option<VmaasConfig>` | — | VMaaS configuration for VM system integration (see [VmaasConfig](#vmaasconfig)). |
| `mlxconfig_profiles` | `Option<HashMap<String, MlxConfigProfile>>` | — | Named Mellanox NIC register configuration profiles for superNIC firmware flashing. TOML key: `mlx-config-profiles`. |
| `rack_management_enabled` | `bool` | `false` | Standalone infrastructure manager mode for GB200/GB300/VR144. See doc comment for full behavioral changes. |
| `rms` | `RmsConfig` | *(see below)* | Rack Manager Service configuration for API connectivity and mTLS (see [RmsConfig](#rmsconfig)). |
| `rack_profiles` | `RackProfileConfig` | *(default)* | Rack profile definitions referenced by expected racks. |
| `spdm` | `SpdmConfig` | *(see below)* | SPDM hardware attestation (see [SpdmConfig](#spdmconfig)). |
| `bgp_leaf_session_password` | `Option<BgpLeafSessionPassword>` | — | Selects the credential source for leaf-facing BGP session passwords returned to agents in managed host network config. Supported value: `site_wide`. |
| `site_global_vpc_vni` | `Option<u32>` | — | Forces all VRFs to share a single VNI (Cumulus Linux route-leaking workaround). Limits DPU to one VRF. |
| `dpf` | `DpfConfig` | *(see below)* | DPF (DPU Platform Framework) Kubernetes deployment (see [DpfConfig](#dpfconfig)). |
| `x86_pxe_boot_url_override` | `Option<String>` | — | Override PXE boot URL for x86 machines. |
| `arm_pxe_boot_url_override` | `Option<String>` | — | Override PXE boot URL for ARM machines. |
| `pxe_public_base_url` | `String` | `http://carbide-pxe.forge:8080` | Canonical PXE base URL. |
| `set_http_boot_uri_for_vendors` | `Vec<BMCVendor>` | `[]` | Vendors for which the state controller pins the UEFI HTTP boot URL on the BMC via Redfish `HttpBootUri`. Empty = all machines rely on nico-dhcp option 67 for the URL. |
| `compute_allocation_enforcement` | `ComputeAllocationEnforcement` | `WarnOnly` | Controls enforcement of compute allocations on new instance requests. |
| `supernic_firmware_profiles` | nested `HashMap` | `{}` | SuperNIC firmware profiles keyed by `part_number` then `PSID`. |
| `component_manager` | `Option<ComponentManagerConfig>` | — | Component manager for NvLink switches and power shelves. |

---

### Component Manager RMS Node Type Resolution

When `[component_manager]` uses RMS backends, NICo resolves RMS node types from
rack profiles. The rack profile provides two facts:

- Product family from `product_family`, which is required for RMS-backed
  operations and currently accepts `gb200` or `gb300`.
- Vendor from `rack_capabilities.<role>.vendor` for each role using an RMS
  backend.

NICo validates configured rack profiles at startup when any component-manager
backend is set to `rms`. The component-manager backend fields default to `rms`,
so deployments that only want one RMS role must explicitly set the other backend
fields to non-RMS values. Startup validation checks the product family and only
the vendor fields for enabled RMS roles. For example, if only
`power_shelf_backend = "rms"` after the other backend fields are set to non-RMS
values, then only `rack_capabilities.power_shelf.vendor` is required as a vendor
field.

Use these canonical vendor names in config:

| Role | Canonical values |
|------|------------------|
| Compute, when `compute_tray_backend = "rms"` | `NVIDIA`, `Lenovo` |
| Switch, when `nv_switch_backend = "rms"` | `NVIDIA` |
| Power shelf, when `power_shelf_backend = "rms"` | `LiteOn`, `Delta` |

The `product_family` value is not normalized. It must exactly match one of the
accepted lowercase values, such as `gb200` or `gb300`; values like `GB200` are
rejected. Vendor matching is more forgiving. Vendor values are trimmed,
case-insensitive, and ignore spaces, hyphens, and underscores, so `NVIDIA`,
`nvidia`, `LiteOn`, `liteon`, `Lite-On`, and `lite_on` all work. Common company
suffix text also works when the normalized value starts with the canonical
vendor, but the canonical values above are preferred for operator-supplied
config.

The examples below only show the component-manager and rack-profile fields.
Configure `[rms]` separately when NICo needs to call RMS.

Example: GB200 rack where all component-manager roles use RMS:

```toml
[component_manager]
compute_tray_backend = "rms"
nv_switch_backend = "rms"
power_shelf_backend = "rms"

[rack_profiles.NVL72]
product_family = "gb200"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

[rack_profiles.NVL72.rack_capabilities.compute]
vendor = "NVIDIA"

[rack_profiles.NVL72.rack_capabilities.switch]
vendor = "NVIDIA"

[rack_profiles.NVL72.rack_capabilities.power_shelf]
vendor = "LiteOn"
```

Example: GB300 rack with Lenovo compute trays and Delta power shelves:

```toml
[component_manager]
compute_tray_backend = "rms"
nv_switch_backend = "rms"
power_shelf_backend = "rms"

[rack_profiles.NVL72_GB300]
product_family = "gb300"
rack_hardware_topology = "gb300_nvl72r1_c2g4_topology"

[rack_profiles.NVL72_GB300.rack_capabilities.compute]
vendor = "Lenovo"

[rack_profiles.NVL72_GB300.rack_capabilities.switch]
vendor = "nvidia"

[rack_profiles.NVL72_GB300.rack_capabilities.power_shelf]
vendor = "delta"
```

Example: only the component-manager power shelf backend uses RMS. The compute
and switch component-manager backends are explicitly set to real non-RMS values
so component-manager startup validation only requires the power shelf vendor
field:

```toml
[component_manager]
compute_tray_backend = "core"
nv_switch_backend = "nsm"
power_shelf_backend = "rms"

[component_manager.nsm]
url = "http://nsm.example.internal:50052"

[rack_profiles.NVL72_POWER]
product_family = "gb200"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

[rack_profiles.NVL72_POWER.rack_capabilities.power_shelf]
vendor = "Lite-On"
```

Each rack that uses an RMS-backed operation must have a `rack_profile_id`
matching a key under `[rack_profiles]`. Startup validation does not scan
existing rack database rows, so missing or unknown per-rack profile IDs are
still checked when an RMS operation runs.

The separate site-explorer machine-ingestion RMS slot/tray lookup also uses the
rack profile for RMS node type resolution. If that path is enabled for machines
with rack IDs, the profile also needs compute product-family and vendor data even
when `compute_tray_backend` is not `rms`.

Supported RMS product-family values are exact-match `gb200` and `gb300`. The
optional `rack_hardware_topology` field remains available for topology-specific
flows.

---

## Sub-Structs

### `TlsConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `root_cafile_path` | `String` | `""` | Root CA certificate for client validation. |
| `identity_pemfile_path` | `String` | `""` | Server identity certificate PEM. |
| `identity_keyfile_path` | `String` | `""` | Server identity private key. |
| `admin_root_cafile_path` | `String` | `""` | Admin root CA for admin client validation. |

### `AuthConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `permissive_mode` | `bool` | — | Enable permissive authorization (dev mode). |
| `casbin_policy_file` | `Option<PathBuf>` | — | Path to Casbin CSV policy file. |
| `cli_certs` | `Option<AllowedCertCriteria>` | — | Additional allowed cert criteria for nico-admin-cli. |
| `trust` | `Option<TrustConfig>` | — | SPIFFE trust domain and allowed paths for client certs. |

### `IBFabricConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enables InfiniBand fabric management. |
| `max_partition_per_tenant` | `i32` | `31` | Maximum IB partitions per tenant (1-31). |
| `allow_insecure` | `bool` | `false` | Allow insecure fabric configs that skip tenant isolation. |
| `mtu` | `IBMtu` | *(default)* | MTU for IB fabric traffic. |
| `rate_limit` | `IBRateLimit` | *(default)* | Rate limit for IB traffic. |
| `service_level` | `IBServiceLevel` | *(default)* | QoS service level for IB packets. |
| `fabric_monitor_run_interval` | `Duration` | `60s` | Interval for the IB fabric monitor. |

### `NvLinkConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enables NvLink partitioning. |
| `monitor_run_interval` | `Duration` | `60s` | NvLink monitor polling interval. |
| `nmx_c_tls_ca_cert_path` | `Option<String>` | — | Extra CA bundle for verifying the NMX-C server over HTTPS. |
| `nmx_c_tls_client_cert_path` | `Option<String>` | — | Client certificate for mTLS to NMX-C. |
| `nmx_c_tls_client_key_path` | `Option<String>` | — | Client private key for mTLS to NMX-C. |
| `nmx_c_tls_authority` | `Option<String>` | — | TLS server name used for SNI and certificate verification. |
| `allow_insecure` | `bool` | `false` | Skip TLS verification for NMX-C. |

### `SiteExplorerConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enables hardware discovery. |
| `run_interval` | `Duration` | `120s` | Interval between exploration runs. |
| `concurrent_explorations` | `u64` | `30` | Max nodes explored in parallel. |
| `explorations_per_run` | `u64` | `90` | Max nodes explored per run. |
| `create_machines` | `bool` | `true` | When false, SiteExplorer skips creating ManagedHost state machines; the DPU agent (scout) must self-register via DiscoverMachine gRPC endpoint with create_machine=true. Dynamically toggleable. |
| `machines_created_per_run` | `u64` | `4` | Max ManagedHosts created per run. |
| `rotate_switch_nvos_credentials` | `bool` | `false` | Auto-rotate switch NVOS admin credentials. |
| `override_target_ip` | `Option<String>` | — | **Deprecated.** Use `bmc_proxy`. Debug BMC IP override. |
| `override_target_port` | `Option<u16>` | — | **Deprecated.** Use `bmc_proxy`. Debug BMC port override. |
| `bmc_proxy` | `HostPortPair` | — | BMC proxy host:port for integration testing/dev. |
| `allow_changing_bmc_proxy` | `Option<bool>` | *(auto)* | Allow runtime changes to `bmc_proxy`. Auto-detected from initial config. |
| `reset_rate_limit` | `Duration` | `1h` | Minimum time between SiteExplorer-initiated BMC resets. |
| `admin_segment_type_non_dpu` | `bool` | `false` | Non-DPU hosts use `HostInband` admin segment type. |
| `allocate_secondary_vtep_ip` | `bool` | `false` | Allocate secondary VTEP IP for GENEVE traffic intercept. |
| `create_power_shelves` | `bool` | `false` | Auto-create Power Shelf state machines. |
| `explore_power_shelves_from_static_ip` | `bool` | `false` | Discover power shelves via static IP. |
| `power_shelves_created_per_run` | `u64` | `1` | Max power shelves created per run. |
| `create_switches` | `bool` | `false` | Auto-create Switch state machines. |
| `switches_created_per_run` | `u64` | `9` | Max switches created per run. |
| `use_onboard_nic` | `bool` | `false` | Use onboard NIC instead of DPU NICs. |
| `explore_mode` | `SiteExplorerExploreMode` | `LibRedfish` | Redfish backend: `libredfish`, `nv-redfish`, or `compare-result`. |
| `explore_drive_firmware` | `bool` | `false` | Reads drive firmware versions from each host's Redfish Storage subsystems and reports each enabled drive with a usable model and version as a `Drive_<Model>_<DriveId>` firmware inventory entry. Adds one request per Storage subsystem and per drive to every exploration. Takes effect only when `explore_mode` is `nv-redfish`: `libredfish` ignores it, and `compare-result` reads the drives but returns the libredfish report, which omits them. |

### `StateControllerConfig`

Shared by all `*StateControllerConfig` structs (machine, network segment, VPC prefix, IB
partition, DPA interface, rack, power shelf, switch, SPDM).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `iteration_time` | `Duration` | `30s` | Target duration for one state controller iteration. |
| `max_object_handling_time` | `Duration` | `3m` | Timeout for evaluating/advancing a single object's state. |
| `max_concurrency` | `usize` | `10` | Max objects advanced in parallel. |
| `processor_dispatch_interval` | `Duration` | `2s` | Max wait time when checking for and dispatching new tasks. |
| `processor_log_interval` | `Duration` | `60s` | How often the processor emits log messages. |
| `metric_emission_interval` | `Duration` | `60s` | How often aggregate metrics are recalculated. |
| `metric_hold_time` | `Duration` | `5m` | How long per-object metrics are held before eviction. |

### `ObservabilityConfig`

TOML section: `[observability]`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `per_object_metrics_for_classifications` | `Vec<HealthAlertClassification>` | `[]` | Health alert classifications for which the per-object metric `carbide_object_unhealthy_by_classification_count` is emitted, labeled with `object_type` (e.g. `machine`, `switch`, `rack`, `power_shelf`) and `object_id`. Each entry adds up to one extra time series per matching object, so it defaults to empty (disabled) to keep metric cardinality bounded. When empty, the metric is not registered or exposed at all; aggregate health metrics are unaffected regardless. |

### `MachineStateControllerConfig`

Extends `StateControllerConfig` with:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `dpu_wait_time` | `Duration` | `5m`    | Time before a DPU is considered definitively down. |
| `power_down_wait` | `Duration` | `2m`    | Wait after power-down before powering on. |
| `failure_retry_time` | `Duration` | `90m`   | Time before re-triggering reboot if machine hasn't called back. |
| `dpu_up_threshold` | `Duration` | `5m`    | Max time without DPU health report before assuming it's down. |
| `scout_reporting_timeout` | `Duration` | `5m`    | Duration without scout report before host is unhealthy. |
| `uefi_boot_wait` | `Duration` | `5m`    | Wait time for UEFI boot completion after host reboot. |
| `max_bios_config_retries` | `u32` | `3` | Shared retry budget for automated host boot-configuration convergence across BIOS recovery and boot-order verification. |
| `polling_bios_setup_stuck_threshold` | `Duration` | `15m` | Time in PollingBiosSetup with `is_bios_setup == false` before recovery escalation. |

### `NetworkSegmentStateControllerConfig`

Extends `StateControllerConfig` with:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `network_segment_drain_time` | `Duration` | `5m` | Time a network segment must have 0 allocated IPs before release. |

### `VpcPrefixStateControllerConfig`

Extends `StateControllerConfig` with:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `vpc_prefix_drain_time` | `Duration` | `5m` | Time a VPC prefix must have 0 referencing network prefixes before release. |

### `FirmwareGlobal`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `autoupdate` | `bool` | `false` | Enable automatic host firmware updates. |
| `host_enable_autoupdate` | `Vec<String>` | `[]` | Host models to force-enable autoupdate. |
| `host_disable_autoupdate` | `Vec<String>` | `[]` | Host models to force-disable autoupdate. |
| `run_interval` | `Duration` | `30s` | Firmware manager polling interval. |
| `max_uploads` | `usize` | `4` | Max concurrent firmware uploads. |
| `concurrency_limit` | `usize` | `16` | Max concurrent firmware flashing operations. |
| `firmware_directory` | `PathBuf` | `/opt/nico/firmware` | Firmware binary storage directory. |
| `host_firmware_upgrade_retry_interval` | `Duration` | `60m` | Retry delay for failed host firmware upgrades. |
| `instance_updates_manual_tagging` | `bool` | `true` | Require manual tagging before firmware updates. |
| `no_reset_retries` | `bool` | `false` | Disable retry logic after BMC resets. |
| `hgx_bmc_gpu_reboot_delay` | `Duration` | `30s` | Delay after GPU reboot before HGX BMC access. |
| `requires_manual_upgrade` | `bool` | `false` | Force all firmware upgrades to require admin approval. |

### `MachineUpdater`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `instance_autoreboot_period` | `Option<TimePeriod>` | — | UTC time window for automatic machine reboots. |
| `max_concurrent_machine_updates_absolute` | `Option<i32>` | — | Hard cap on concurrent machine updates. |
| `max_concurrent_machine_updates_percent` | `Option<i32>` | — | Percentage cap on concurrent updates (lesser of absolute/percent is used). |

### `PowerManagerOptions`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable power management. |
| `next_try_duration_on_success` | `Duration` | `5m` | Retry interval after successful power operation. |
| `next_try_duration_on_failure` | `Duration` | `2m` | Retry interval after failed power operation. |
| `wait_duration_until_host_reboot` | `Duration` | `15m` | Wait after power-down before powering on host. |

### `VmaasConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `allow_instance_vf` | `bool` | `true` | Allow VFs on instance creation. |
| `hbn_reps` | `Option<String>` | — | HBN representors created by DPUs. |
| `hbn_sfs` | `Option<String>` | — | HBN SF representors created by DPUs. |
| `bridging` | `Option<TrafficInterceptBridging>` | — | Advanced traffic-intercept routing and bridging options. |
| `public_prefixes` | `Vec<Ipv4Network>` | **required** | Publicly routable IPv4 CIDR prefixes used by traffic-intercept users. |
| `secondary_vtep_aggregate_prefixes` | `Vec<IpNetwork>` | `[]` | IPv4 or IPv6 aggregate prefixes used only for routing and filtering. IP allocation is provided by the secondary VTEP resource pool. |
| `secondary_overlay_support` | `bool` | `true` | Whether secondary overlay VTEP IPs are expected for DPUs. |

### `TrafficInterceptBridging`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `internal_bridge_routing_prefix` | `Ipv4Network` | **required** | Prefix used for internal routing between HBN and intercept bridges within the DPU. |
| `hbn_bridge` | `String` | `"br-hbn"` | Bridge that intercept patch ports attach to during BlueField provisioning. |
| `vf_intercept_bridge_name` | `String` | `"br-dpu"` | Bridge between VM-owned VFs and br-hbn. |
| `vf_intercept_bridge_port` | `String` | `"patch-br-dpu-to-hbn"` | Patch port on the VF intercept bridge side. |
| `vf_intercept_bridge_sf` | `String` | **required** | SF used for internal routing of VF traffic. |
| `host_representor_intercept_bridging` | `HashMap<String, HostInterceptBridging>` | `{}` | Host-owned PF/VF representor bridge layout keyed by representor name. Non-skipped entries are sent to BlueField provisioning as `<representor>:<bridge>:<patch_port>`. |

### `HostInterceptBridging`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `bridge` | `String` | **required** | Bridge that sits between the host PF/VF representor and br-hbn or br-sfc. |
| `patch_port` | `String` | **required** | Patch port on this bridge that connects it toward HBN or SFC. |
| `skip_create` | `bool` | `false` | When true, the entry is sent to DPU agents but omitted from provisioning-time bridge creation. |

### `DpuConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `dpu_nic_firmware_initial_update_enabled` | `bool` | `false` | Enable DPU NIC firmware updates on initial discovery. |
| `dpu_nic_firmware_reprovision_update_enabled` | `bool` | `true` | Enable DPU NIC firmware updates on reprovisioning. |
| `dpu_models` | `HashMap<String, Firmware>` | *(BF2+BF3 defaults)* | DPU model firmware definitions. |
| `dpu_nic_firmware_update_versions` | `Vec<String>` | *(BF2+BF3 NIC versions)* | DPU NIC firmware version strings. |
| `dpu_enable_secure_boot` | `bool` | `false` | Enable secure boot flow for DPU provisioning via Redfish. |
| `num_of_vfs` | `u32` | `16` | Number of VFs configured per DPU PF during BlueField provisioning. Max `126`. |

### `NetworkSecurityGroupConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_network_security_group_size` | `u32` | `200` | Max expanded rules per NSG. |
| `stateful_acls_enabled` | `bool` | `true` | Enable stateful ACLs (toggled on DPU via nvue). |
| `policy_overrides` | `Vec<NetworkSecurityGroupRule>` | `[]` | NSG rules injected before user-defined rules. |

### `FnnConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `admin_vpc` | `Option<AdminFnnConfig>` | — | FNN configuration for the admin network VPC. |
| `common_internal_route_target` | `Option<RouteTargetConfig>` | — | Double-tag for internal tenant routes (consumed by the network infrastructure). |
| `additional_route_target_imports` | `Vec<RouteTargetConfig>` | `[]` | Extra route targets imported on DPU VRFs. |
| `routing_profiles` | `HashMap<String, FnnRoutingProfileConfig>` | `{}` | Named per-VPC routing profiles (see [FnnRoutingProfileConfig](#fnnroutingprofileconfig)). |

### `FnnRoutingProfileConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `route_target_imports` | `Vec<RouteTargetConfig>` | `[]` | Route targets imported into DPU VRFs for VPC routes. |
| `route_targets_on_exports` | `Vec<RouteTargetConfig>` | `[]` | Route targets added to routes exported by the DPU. |
| `internal` | `bool` | `false` | Whether the profile uses internal VNI allocation. |
| `leak_default_route_from_underlay` | `bool` | `false` | Leak the default route from the underlay/default VRF into tenant VRFs. |
| `leak_tenant_host_routes_to_underlay` | `bool` | `false` | Leak tenant host routes into the underlay/default VRF. |
| `tenant_leak_communities_accepted` | `bool` | `false` | Honor route-leak communities sent by the tenant host OS. |
| `accepted_leaks_from_underlay` | `Vec<PrefixFilterPolicyEntry>` | `[]` | Specific underlay/default VRF prefixes allowed to leak into tenant VRFs. Routing only; does not affect ACLs. |
| `allowed_anycast_prefixes` | `Vec<PrefixFilterPolicyEntry>` | `[]` | IPv4 or IPv6 prefixes that tenant hosts are allowed to announce to the DPU as anycast routes. |
| `access_tier` | `u32` | `0` | Routing profile access tier. Lower values grant broader access. |

### `PrefixFilterPolicyEntry`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `prefix` | `IpNetwork` | **required** | IPv4 or IPv6 CIDR prefix accepted by a prefix-list policy. |

### `DpaConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable Cluster Interconnect Network. |
| `mqtt_endpoint` | `String` | `"mqtt.nico"` | MQTT broker host for DPA. |
| `mqtt_broker_port` | `u16` | `1884` | MQTT broker port. |
| `subnet_ip` | `Ipv4Addr` | `0.0.0.0` | Base IPv4 address of the DPA subnet. |
| `subnet_mask` | `i32` | `0` | CIDR prefix length for the DPA subnet. |
| `hb_interval` | `Duration` | `2m` | Heartbeat interval for DPA health checks. |
| `auth` | `MqttAuthConfig` | *(none)* | MQTT authentication settings. |

### `DsxExchangeEventBusConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable the DSX Exchange Event Bus for managed-host state publishing, BMS metadata subscription, and BMS rack/isolation/heartbeat publishing. |
| `mqtt_endpoint` | `String` | `"mqtt.nico"` | MQTT broker host. |
| `mqtt_broker_port` | `u16` | `1884` | MQTT broker port. |
| `publish_timeout` | `Duration` | `1s` | Timeout for MQTT publish operations. |
| `queue_capacity` | `usize` | `1024` | Event buffer size for DSX publish work (events dropped when full). |
| `auth` | `MqttAuthConfig` | *(none)* | MQTT authentication settings. |

### `DpfConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable DPF Kubernetes deployment. |
| `bfb_url` | `String` | `""` | BlueField firmware bundle URL. |
| `deployment_name` | `Option<String>` | — | Kubernetes deployment name. |
| `services` | `Option<Vec<DpfServiceConfig>>` | — | Additional Helm services. |

### `RmsConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `api_url` | `Option<String>` | — | RMS API URL for rack-level firmware upgrades and power sequencing. |
| `root_ca_path` | `Option<String>` | — | Path to the root CA certificate for TLS verification. |
| `client_cert` | `Option<String>` | — | Path to the client certificate PEM for mTLS. |
| `client_key` | `Option<String>` | — | Path to the client private key PEM for mTLS. |
| `enforce_tls` | `bool` | `true` | Enforce TLS when connecting to RMS. |

### `SpdmConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable SPDM hardware attestation. |
| `nras_config` | `Option<nras::Config>` | — | NRAS configuration for secure boot verification. |

### `MachineIdentityConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Master switch for machine identity APIs (opt-in; set `true` with `current_encryption_key_id` and credentials). |
| `algorithm` | `String` | `"ES256"` | Signing algorithm for per-org keys. |
| `token_ttl_min_sec` | `u32` | `60` | Minimum token TTL in seconds. |
| `token_ttl_max_sec` | `u32` | `86400` | Maximum token TTL in seconds. |
| `token_endpoint_http_proxy` | `Option<String>` | — | HTTP proxy for token endpoint calls (SSRF mitigation). |

### `MeasuredBootMetricsCollectorConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable measured boot metrics export. |
| `run_interval` | `Duration` | `60s` | Polling interval for boot measurement data. |

### `MachineValidationConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable machine validation tests. |
| `test_selection_mode` | `MachineValidationTestSelectionMode` | `Default` | `Default`, `EnableAll`, or `DisableAll`. |
| `run_interval` | `Duration` | `60s` | Validation check interval. |
| `stale_run_timeout` | `Duration` | `24h` | Grace period before an active validation run is considered stale. Values below `90s` are raised to `90s` to avoid marking healthy heartbeat-based runs stale. |
| `tests` | `Vec<MachineValidationTestConfig>` | `[]` | Per-test enable/disable overrides. |

### `BomValidationConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable BOM/SKU validation. |
| `ignore_unassigned_machines` | `bool` | `false` | Let machines without a SKU bypass validation. |
| `allow_allocation_on_validation_failure` | `bool` | `false` | Keep machines allocatable even when validation fails. |
| `find_match_interval` | `Duration` | `5m` | Interval between SKU match attempts. |
| `auto_generate_missing_sku` | `bool` | `false` | Auto-create missing SKUs from expected machines. |
| `auto_generate_missing_sku_interval` | `Duration` | `5m` | Interval between auto-generate attempts. |

### `MqttAuthConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `auth_mode` | `MqttAuthMode` | `None` | `none`, `basic_auth`, or `oauth2`. |
| `oauth2` | `Option<MqttOAuth2Config>` | — | OAuth2 settings (required when `auth_mode` is `oauth2`). |

### `MqttOAuth2Config`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `token_url` | `String` | **required** | OAuth2 token endpoint URL. |
| `scopes` | `Vec<String>` | `[]` | OAuth2 scopes to request. |
| `http_timeout` | `Duration` | `30s` | Token endpoint HTTP timeout. |
| `username` | `String` | `"oauth2token"` | Username in MQTT CONNECT packet. |
