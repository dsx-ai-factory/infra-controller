# NICo API Configuration Reference

This document describes every section and field in the `nico-api-config.toml`
configuration file, which is deserialized into `NicoConfig` (defined in
`file.rs`). Fields are listed in declaration order. Defaults are noted where
applicable.

Unknown fields are reported after the base file, optional site override, and
`CARBIDE_API_` environment values are merged. They produce warnings by default
so configuration can be deployed ahead of the supporting binary. Set
`deny_unknown_fields = true` to reject them during startup. Diagnostics include
the invalid key's full section path and source. Names inside intentionally
dynamic maps, such as pool names and rack-profile IDs, remain user-defined;
fields within each map value must still match the documented schema.

The removed `force_dpu_nic_mode` key is explicitly recognized at the top level
and under `[site_explorer]`, ignored, and reported as a deprecation warning.
Use `site_explorer.dpu_policy` instead.

---

## `NicoConfig` (top-level)

| Field | Type | Default | Group | Description |
| ------- | ------ | --------- | ------- | ------------- |
| `listen` | `SocketAddr` | `[::]:1079` | `server` | Socket address for the gRPC API server. |
| `listen_only` | `bool` | `false` | `server` | Run passively (no background services, RPC/web only). Used in dev mode. |
| `metrics_endpoint` | `Option<SocketAddr>` | — | `integrations` | Socket address for the Prometheus `/metrics` HTTP server. |
| `alt_metric_prefix` | `Option<String>` | — | `integrations` | Alternative metric prefix emitted alongside `nico_` for dashboard migration. |
| `database_url` | `String` | **required** | `server` | Postgres connection string for all persistent state. |
| `max_database_connections` | `u32` | `1000` | `server` | Maximum database connection pool size. |
| `deny_unknown_fields` | `bool` | `false` | `server` | Reject unknown configuration fields instead of logging warnings and continuing. |
| `database_pool_acquire_timeout` | `Duration` | `30s` | `server` | How long a caller may wait for a connection from the pool before the attempt fails (sqlx's own default); trips on a stalled database or a saturated pool alike. Must be greater than zero (startup rejects `0`). |
| `database_pool_idle_timeout` | `Duration` | `10m` | `server` | Idle time after which the pool closes a connection, keeping the pool's own reaping well inside the Postgres server's 60-minute idle-session reaper. Must be greater than zero (startup rejects `0`). |
| `database_pool_max_lifetime` | `Duration` | `30m` | `server` | Maximum age of a pooled connection before it is recycled, so the pool re-balances onto the current primary after a database failover. Must be greater than zero (startup rejects `0`). |
| `api_admission_control` | `ApiAdmissionControlConfig` | *(see below)* | `server` | Fair per-client admission with global execution and pending-request limits for gRPC and admin HTTP business requests. |
| `ib_config` | `Option<IBFabricConfig>` | — | `hardware` | InfiniBand fabric configuration (see [IBFabricConfig](#ibfabricconfig)). |
| `asn` | `u32` | **required** | `networking` | Autonomous System Number, fixed per environment. Used by nico-dpu-agent for `frr.conf` BGP routing. |
| `dhcp_servers` | `Vec<Ipv4Addr>` | `[]` | `networking` | DHCP server addresses announced to DPUs during network provisioning. |
| `ntp_servers` | `Vec<Ipv4Addr>` | `[]` | `networking` | Site-level NTP server IPs used for BMC time configuration and DHCP NTP Server configuration. |
| `route_servers` | `Vec<String>` | `[]` | `networking` | Route server IPs for L2VPN Ethernet Virtual network support. |
| `enable_route_servers` | `bool` | `false` | `networking` | Enables route server injection into DPU FRR configs for L2VPN. |
| `deny_prefixes` | `Vec<IpNetwork>` | `[]` | `networking` | IPv4 and IPv6 CIDR prefixes that tenant instances are blocked from reaching. FNN generates family-specific NVUE ACL policies; all non-FNN virtualizers apply the IPv4 prefixes only. |
| `site_fabric_prefixes` | `Vec<IpNetwork>` | `[]` | `networking` | IP prefixes (v4/v6) assigned for tenant use within this site. |
| `tenant_prefix_overlap_enabled` | `bool` | `false` | `networking` | Site opt-in for [tenant prefix overlap checks](#tenant-prefix-overlap-checks). The existing `VpcPrefix` exclusion continues to prevent overlapping `VpcPrefix` persistence until the cutover tracked by [#3892](https://github.com/dsx-ai-factory/infra-controller/issues/3892). |
| `max_site_prefixes_per_tenant` | `u32` | `8` | `networking` | Maximum tenant-managed SitePrefixes retained for one tenant at this site. Prefixes awaiting removal still count against this limit and keep their CIDR reserved. |
| `anycast_site_prefixes` | `Vec<Ipv4Network>` | `[]` | `networking` | Aggregate IPv4 prefixes containing tenant-announced prefixes (e.g., BYOIP). **Deprecated.** Use [`routing_profiles.allowed_anycast_prefixes`](#fnnroutingprofileconfig) instead. |
| `common_tenant_host_asn` | `Option<u32>` | — | `networking` | ASN that tenants use to peer with the DPU. If unset, any ASN is accepted. |
| `vpc_isolation_behavior` | `VpcIsolationBehaviorType` | `MutualIsolation` | `networking` | VPC isolation policy: `mutual_isolation` or `open`. |
| `host_naming_strategy` | `HostNamingStrategyKind` | `IpAddress` | `machines` | How new machine hostnames are derived: `ip_address` (IP-derived, e.g. `10-1-2-3`; the default and backwards-compatible), `fun` (stable adjective-noun handles like `wholesale-walrus`), `serial_number` (a machine's hardware serial -- the primary interface gets the bare serial, secondary interfaces get `serial-<mac>`, BMC interfaces stay IP-named), or `mac_address` (each interface's own MAC, e.g. `0a-1b-2c-3d-4e-5f`). Only `fun` leaves existing hostnames unchanged -- it keeps any real name, whether IP-, serial-, or MAC-derived, so after a switch fun names appear only on newly named interfaces; the others re-derive, so switching to one progressively renames interfaces as they reconcile. Junk placeholder serials (e.g. `To Be Filled By O.E.M.`) fall back to the IP name, and `serial_number` errors on duplicate serials rather than assigning a substitute name. |
| `dpu_network_monitor_pinger_type` | `Option<String>` | — | `networking` | Pinger implementation type (e.g., `"OobNetBind"`) for DPU link health checks. |
| `tls` | `Option<TlsConfig>` | — | `server` | TLS certificate/key paths (see [TlsConfig](#tlsconfig)). |
| `listen_mode` | `ListenMode` | `Tls` | `server` | Transport mode: `plaintext_http1`, `plaintext_http2`, or `tls`. |
| `auth` | `Option<AuthConfig>` | — | `server` | Authentication/authorization settings (see [AuthConfig](#authconfig)). |
| `pools` | `Option<HashMap<String, ResourcePoolDef>>` | — | `networking` | Resource pools that allocate IPs, VNIs, etc. Required but `Option` for partial-config merging. |
| `networks` | `Option<HashMap<String, NetworkDefinition>>` | — | `networking` | Networks created at startup. Alternative: `CreateNetworkSegment` gRPC. `NetworkDefinition` supports dual-stack seed-time segments with optional `prefix_v6` and `dhcpv6_link_address`; config edits do not retrofit prefixes onto an already-seeded segment because seed definitions are snapshotted on first create. |
| `dpu_ipmi_tool_impl` | `Option<String>` | — | `machines` | IPMI tool implementation for DPU power control (`"prod"` or `"fake"`). |
| `dpu_ipmi_reboot_attempts` | `Option<u32>` | — | `machines` | Retry count when IPMI errors during DPU reboot. |
| `bmc_session_lockout_threshold` | `u32` | `3` | `security` | Consecutive BMC HTTP 401/403 responses before session-token login attempts stop for that BMC. |
| `bmc_max_sessions_per_caller` | `usize` | `4` | `security` | Cap on outstanding Redfish sessions per calling service identity per BMC; a `GetBmcCredentials` mint past the cap revokes that caller's oldest sessions. Values below 1 are treated as 1. |
| `ib_fabrics` | `HashMap<String, IbFabricDefinition>` | `{}` | `hardware` | InfiniBand fabrics managed by the site. Currently only one fabric is supported. |
| `initial_domain_name` | `Option<String>` | — | `machines` | Domain to create if none exist. Most sites use a single domain. |
| `initial_dpu_agent_upgrade_policy` | `Option<AgentUpgradePolicyChoice>` | — | `machines` | Policy for nico-dpu-agent upgrades. Also settable via `nico-admin-cli`. |
| `max_concurrent_machine_updates` | `Option<i32>` | — | `machines` | **Deprecated.** Use `machine_updater` instead. |
| `machine_update_run_interval` | `Option<u64>` | — | `machines` | Interval (seconds) at which the machine update manager checks for updates. |
| `retained_boot_interface_window` | `Option<Duration>` | — | `machines` | How long a retained boot interface pair (`retained_boot_interfaces` table) stays applicable after its `machine_interfaces` row was deleted. Unset retains forever; set a window (e.g. `30d`) so a MAC reappearing on different hardware doesn't inherit an obsolete Redfish interface id. |
| `site_explorer` | `SiteExplorerConfig` | *(see below)* | `hardware` | SiteExplorer hardware discovery settings (see [SiteExplorerConfig](#siteexplorerconfig)). |
| `vpc_peering_policy` | `Option<VpcPeeringPolicy>` | — | `networking` | Policy for VPC peering based on network virtualization type at creation time. |
| `vpc_peering_policy_on_existing` | `Option<VpcPeeringPolicy>` | — | `networking` | Policy for whether existing VPC peerings should be active. |
| `attestation_enabled` | `bool` | `false` | `security` | Enables TPM-based machine attestation (adds `Measuring` state before `Ready`). |
| `bmc_rotation_enabled` | `bool` | `false` | `security` | Site-wide kill-switch for passive BMC credential rotation. When `false` (default), a Ready host never auto-enters `RotatingBmc`; the force-converge escape hatch bypasses it. |
| `uefi_rotation_enabled` | `bool` | `false` | `security` | Site-wide kill-switch for passive UEFI credential rotation (host and DPU). When `false` (default), a Ready host never auto-enters `RotatingHostUefi` nor drives its DPUs into `RotatingDpuUefi`; the per-machine force-converge escape hatch bypasses it. |
| `nic_lockdown_ikm_rotation_enabled` | `bool` | `false` | `security` | Site-wide kill-switch for NIC lockdown IKM rotation. When `false` (default), the SuperNIC lock/unlock flow keeps deriving keys from each card's current tracked IKM version, so a staged `RotateCredential(lockdown_ikm)` bumps the site-wide target without migrating any card. When `true`, the assignment-cycle lock derives from the staged site-wide target, so cards migrate to the new IKM as tenants cycle. Unlock always derives from the version a card is actually locked under regardless of this flag, so flipping it off never bricks an already-migrated card. |
| `bmc_factory_reset_on_instance_termination_enabled` | `bool` | `false` | `security` | Site-wide opt-in for factory-resetting the host BMC during tenant release. When `false` (default), tenant release proceeds directly to `PowerCycle`; when `true`, the release flow factory-resets the BMC, waits for it to return, restores the device's previous per-device credential, then continues with the existing power-cycle / boot-order repair. |
| `tpm_required` | `bool` | `true` | `security` | Require TPM module for machine registration. **Testing only** when `false`. |
| `machine_state_controller` | `MachineStateControllerConfig` | *(see below)* | `machines` | Machine state controller timing (see [MachineStateControllerConfig](#machinestatecontrollerconfig)). |
| `network_segment_state_controller` | `NetworkSegmentStateControllerConfig` | *(see below)* | `networking` | Network segment state controller timing. |
| `vpc_prefix_state_controller` | `VpcPrefixStateControllerConfig` | *(see below)* | `networking` | VPC prefix state controller timing. |
| `extension_service_state_controller` | `ExtensionServiceStateControllerConfig` | *(see below)* | `machines` | DPU extension service state controller timing. |
| `ib_partition_state_controller` | `IbPartitionStateControllerConfig` | *(see below)* | `hardware` | IB partition state controller timing. |
| `dpa_interface_state_controller` | `DpaInterfaceStateControllerConfig` | *(see below)* | `networking` | DPA interface state controller timing. |
| `rack_state_controller` | `RackStateControllerConfig` | *(see below)* | `hardware` | Rack state controller timing, optional ingestion firmware update, and primary-switch mTLS service selection. |
| `power_shelf_state_controller` | `PowerShelfStateControllerConfig` | *(see below)* | `hardware` | Power shelf state controller timing and optional rack firmware reprovisioning. |
| `switch_state_controller` | `SwitchStateControllerConfig` | *(see below)* | `hardware` | Switch state controller timing and per-switch mTLS service selection. |
| `spdm_state_controller` | `SpdmStateControllerConfig` | *(see below)* | `security` | SPDM state controller timing. |
| `host_models` | `HashMap<String, Firmware>` | `{}` | `machines` | Maps host model identifiers to firmware definitions. |
| `firmware_global` | `FirmwareGlobal` | *(see below)* | `machines` | Global firmware update settings (see [FirmwareGlobal](#firmwareglobal)). |
| `machine_updater` | `MachineUpdater` | *(see below)* | `machines` | Machine update policies (see [MachineUpdater](#machineupdater)). |
| `max_find_by_ids` | `u32` | `100` | `server` | Max IDs accepted by `find_*_by_ids` APIs. |
| `network_security_group` | `NetworkSecurityGroupConfig` | *(see below)* | `networking` | NSG settings (see [NetworkSecurityGroupConfig](#networksecuritygroupconfig)). |
| `min_dpu_functioning_links` | `Option<u32>` | unset (effective value `2`) | `machines` | Controls DPU ToR BGP health checks. Refer to [DPU ToR Uplink Health](../../../../docs/dpu-management/dpu_configuration.md#dpu-tor-uplink-health) for values and lifecycle effects. |
| `host_health` | `HostHealthConfig` | *(default)* | `machines` | Host health monitoring thresholds for hardware health and DPU agent compliance. |
| `observability` | `ObservabilityConfig` | *(default)* | `integrations` | Observability settings shared across all state controllers (see [ObservabilityConfig](#observabilityconfig)). |
| `internet_l3_vni` | `u32` | `100001` | `networking` | Network infrastructure-provided L3 VNI for FNN VPC Internet connectivity. Combined with `datacenter_asn` for route-target. |
| `measured_boot_collector` | `MeasuredBootMetricsCollectorConfig` | *(see below)* | `security` | Measured boot metrics exporter (see [MeasuredBootMetricsCollectorConfig](#measuredbootmetricscollectorconfig)). |
| `machine_validation_config` | `MachineValidationConfig` | *(see below)* | `machines` | Machine validation tests (see [MachineValidationConfig](#machinevalidationconfig)). |
| `machine_identity` | `MachineIdentityConfig` | *(see below)* | `security` | SPIFFE JWT-SVID machine identity (see [MachineIdentityConfig](#machineidentityconfig)). |
| `bypass_rbac` | `bool` | `false` | `server` | Disables RBAC enforcement. **Testing/dev only.** |
| `dpu_config` | `DpuConfig` | *(see below)* | `machines` | DPU firmware and provisioning (see [DpuConfig](#dpuconfig)). |
| `fnn` | `Option<FnnConfig>` | — | `networking` | FNN L3 VNI overlay networking (see [FnnConfig](#fnnconfig)). |
| `bom_validation` | `BomValidationConfig` | *(see below)* | `machines` | BOM/SKU validation (see [BomValidationConfig](#bomvalidationconfig)). |
| `bios_profiles` | `BiosProfileVendor` | *(default)* | `machines` | BIOS profiles by vendor/model for Redfish BIOS management. |
| `selected_profile` | `BiosProfileType` | *(default)* | `machines` | Default BIOS profile type applied to machines. |
| `ewethers_config` | `Option<EwEthersConfig>` | — | `networking` | Cluster Interconnect (east-west Ethernet) config (see [EwEthersConfig](#ewethersconfig)). Accepts the legacy `dpa_config` section name; legacy inline `mqtt_endpoint`, `mqtt_broker_port`, `hb_interval`, and `auth` keys are migrated into `svpc` at load time with a deprecation warning. |
| `dsx_exchange_event_bus` | `Option<DsxExchangeEventBusConfig>` | — | `integrations` | MQTT event bus for managed-host state publishing plus BMS metadata subscription and rack/isolation/heartbeat publishing (see [DsxExchangeEventBusConfig](#dsxexchangeeventbusconfig)). |
| `datacenter_asn` | `u32` | `11414` | `networking` | Datacenter ASN used by FNN for DC-specific route targets. |
| `nvlink_config` | `Option<NvLinkConfig>` | — | `hardware` | NvLink partitioning via NMX-C (see [NvLinkConfig](#nvlinkconfig)). |
| `power_manager_options` | `PowerManagerOptions` | *(see below)* | `hardware` | Power management timing (see [PowerManagerOptions](#powermanageroptions)). |
| `sitename` | `Option<String>` | — | `server` | Human-readable site name exposed to tenants via FMDS. |
| `auto_machine_repair_plugin` | `AutoMachineRepairPluginConfig` | *(default)* | `machines` | Auto-repair configuration for failed machines. |
| `vmaas_config` | `Option<VmaasConfig>` | — | `integrations` | VMaaS configuration for VM system integration (see [VmaasConfig](#vmaasconfig)). |
| `mlxconfig_profiles` | `Option<HashMap<String, MlxConfigProfile>>` | — | `machines` | Named Mellanox NIC register configuration profiles for superNIC firmware flashing. TOML key: `mlx-config-profiles`. |
| `rack_management_enabled` | `bool` | `false` | `hardware` | Standalone infrastructure manager mode for GB200/GB300/VR144. See doc comment for full behavioral changes. |
| `rms` | `RmsConfig` | *(see below)* | `hardware` | Rack Manager Service configuration for API connectivity and mTLS (see [RmsConfig](#rmsconfig)). |
| `rack_profiles` | `RackProfileConfig` | *(default)* | `hardware` | Rack profile definitions referenced by expected racks. |
| `spdm` | `SpdmConfig` | *(see below)* | `security` | SPDM hardware attestation (see [SpdmConfig](#spdmconfig)). |
| `bgp_leaf_session_password` | `Option<BgpLeafSessionPassword>` | — | `networking` | Selects the credential source for leaf-facing BGP session passwords returned to agents in managed host network config. Supported value: `site_wide`. |
| `site_global_vpc_vni` | `Option<u32>` | — | `networking` | Forces all VRFs to share a single VNI (Cumulus Linux route-leaking workaround). Limits DPU to one VRF. |
| `dpf` | `DpfConfig` | *(see below)* | `machines` | DPF (DPU Platform Framework) Kubernetes deployment (see [DpfConfig](#dpfconfig)). |
| `x86_pxe_boot_url_override` | `Option<String>` | — | `machines` | Override PXE boot URL for x86 machines. |
| `arm_pxe_boot_url_override` | `Option<String>` | — | `machines` | Override PXE boot URL for ARM machines. |
| `pxe_public_base_url` | `String` | `http://carbide-pxe.forge:8080` | `machines` | Canonical PXE base URL. |
| `set_http_boot_uri_for_vendors` | `Vec<BMCVendor>` | `[]` | `machines` | Vendors for which the state controller pins the UEFI HTTP boot URL on the BMC via Redfish `HttpBootUri`. Empty = all machines rely on nico-dhcp option 67 for the URL. |
| `compute_allocation_enforcement` | `ComputeAllocationEnforcement` | `WarnOnly` | `machines` | Controls enforcement of compute allocations on new instance requests. |
| `supernic_firmware_profiles` | nested `HashMap` | `{}` | `machines` | SuperNIC firmware profiles keyed by `part_number` then `PSID`. |
| `component_manager` | `Option<ComponentManagerConfig>` | — | `hardware` | Component manager for NvLink switches and power shelves. |
| `vpcs` | `Option<HashMap<String, VpcDefinition>>` | — | `networking` | VPCs to create at startup (see [VpcDefinition](#vpcdefinition)). Use the `CreateVpc` gRPC to create them later instead. |
| `allow_bmc_basic_auth_fallback` | `bool` | `false` | `security` | When `true`, `GetBmcCredentials` may return `UsernamePassword` credentials for BMCs whose Redfish ServiceRoot does not expose `SessionService`. When `false`, such BMCs surface a `NoSessionService` error and no basic-auth fallback is performed. |
| `rack_validation_config` | `RackValidationConfig` | *(default)* | `hardware` | Rack-level validation: multi-node partition tests after firmware upgrade and maintenance to verify rack health (see [RackValidationConfig](#rackvalidationconfig)). |
| `oem_manager_profiles` | `BiosProfileVendor` | `{}` | `machines` | Vendor-specific iDRAC/BMC manager attributes applied during machine setup, before BMC lockdown. Keyed by vendor → model → profile → attribute name; targets the manager OEM attributes endpoint (e.g. Dell `DellAttributes`), as opposed to `bios_profiles` which targets BIOS settings. Model names are normalized to lowercase with underscores (e.g. `"PowerEdge R760"` → `"poweredge_r760"`). |
| `external_api_url` | `Option<String>` | — | `server` | Alternate API URL for external hosts that cannot resolve the internal name, e.g. `https://carbide-stack-api.corp.example.com`. Handed to interfaces on the static-assignments subnet; unset means external hosts get the internal `api_url`. |
| `external_pxe_url` | `Option<String>` | — | `machines` | Alternate PXE URL for external hosts. Used for cloud-init and root CA retrieval on the static-assignments segment; same rules as `external_api_url`. |
| `external_static_pxe_url` | `Option<String>` | — | `machines` | Alternate static PXE URL for kernel/blob downloads on the static-assignments segment. Falls back to `external_pxe_url`. |
| `default_tenant_routing_profile_type` | `String` | `EXTERNAL` | `networking` | The default routing profile used when a tenant is created. |
| `initial_objects_file` | `Option<PathBuf>` | — | `server` | Path to the `initial_objects.toml` file for seeding the database. |
| `enable_admin_ui` | `bool` | `true` | `server` | Whether to serve the admin web UI (the HTML pages under `/admin`). Set to `false` to run only the gRPC API; the gRPC service is unaffected either way. |
| `web_ui_sidebar_tools` | `Vec<ToolLink>` | `[]` | `server` | External tool links surfaced in the admin web UI's "Tools" sidebar. Each entry's `name` must be unique; the section is hidden when the list is empty. |
| `web_ui_logs_link_template` | `String` | `""` | `server` | URL template for the "Logs" link on machine and endpoint detail pages. The placeholder `{search}` is replaced with the machine ID or BMC IP. When empty, the link is hidden. |
| `log_history` | `LogHistoryConfig` | *(default)* | `integrations` | In-memory log history for the admin web live log viewer at `/admin/logs` (see [LogHistoryConfig](#loghistoryconfig)). |
| `tracing` | `TracingConfig` | *(default)* | `integrations` | OTLP trace export settings (see [TracingConfig](#tracingconfig)). |
| `secrets` | `Option<SecretsConfig>` | — | `security` | Secrets backend configuration. When present, the credential reader chain and write target are operator-configured (see [SecretsConfig](#secretsconfig)). |
| `credentials` | `CredentialsConfig` | *(default)* | `security` | Operator-managed static credential sources and the UFM read/mutation policy (see [CredentialsConfig](#credentialsconfig)). The config stores source locations, not credential values. |
| `dhcp_lease_expiry_handling` | `bool` | `false` | `networking` | Enables IP cleanup when a DHCP lease expires. |
| `certificates` | `CertificatesConfig` | *(default)* | `security` | Certificate vending backend, selected independently of the credential store; the default shares the credential Vault (see [CertificatesConfig](#certificatesconfig)). |
| `allow_insecure_discovery` | `bool` | `false` | `machines` | Allows machines to submit discovery without enforcing the request comes from the expected IP address. Needed for *Integration tests only*, should otherwise not be used. |
| `scout_boot_interface_correction_enabled` | `bool` | `false` | `machines` | Controls whether NICo may reconcile a boot interface selection recorded as `RedfishChassisId` or `RedfishSerialNumber` after ordering DPU-attached Admin interfaces by the `domain:bus:device.function` PCI addresses in scout's `HardwareInfo`. The setting is read at startup. NICo records available comparisons in structured logs and `carbide_scout_pci_evaluations_total` regardless of this setting. When `false`, it does not change the selection. When `true`, reconciliation requires at least two eligible interfaces, a complete and unique candidate, `ManagedHostState::Ready` or `ManagedHostState::HostInit` with `MachineState::Discovered`, no `Instance` or primary interface prediction, and no conflicting or integrated-NIC primary. If the selected MAC is already desired and primary, NICo changes only the source to `ScoutReportPci`. Otherwise it updates the desired target and primary together and enqueues the state handler. A `Ready` host enters `BootConfiguring`; `HostInit` completes its reboot handshake first. |
| `node_auth` | `NodeAuthConfig` | *(default)* | `security` | How Scout and the DPU-agent authenticate: bearer JWTs, machine mTLS client certificates, or both during a migration (see [NodeAuthConfig](#nodeauthconfig)). |

---

### Component Manager RMS Node Descriptors

When `[component_manager]` uses RMS backends, NICo builds RMS node descriptors
from rack profiles. Each descriptor contains three attributes:

- Role from the component-manager operation: `compute`, `switch`, or
  `power_shelf`.
- Product family from `product_family`, which must be non-empty for RMS-backed
  operations. NICo passes other non-empty product-family identifiers to RMS
  without a local hardware mapping.
- Vendor from `rack_capabilities.<role>.vendor` for each role using an RMS
  backend.

NICo always sends these attributes in descriptor-based RMS requests. For exact
role, vendor, and product-family combinations represented by the current RMS
`NodeType` enum, NICo also sends that enum and legacy firmware-filter entries
for compatibility with older RMS servers. Other combinations leave `NodeType`
unset and require RMS support for `NodeDescriptor`. This best-effort legacy
mapping does not participate in startup validation. In particular, VRNVL72
power shelves use their configured VRNVL72 descriptor because no matching
legacy `NodeType` exists.

NICo validates configured rack profiles at startup when any component-manager
backend is set to `rms`. The component-manager backend fields default to `rms`,
so deployments that only want one RMS role must explicitly set the other backend
fields to non-RMS values. Startup validation checks the product family and only
the vendor fields for enabled RMS roles. For example, if only
`power_shelf_backend = "rms"` after the other backend fields are set to non-RMS
values, then only `rack_capabilities.power_shelf.vendor` is required as a vendor
field.

NICo trims outer whitespace from `product_family` and vendor values and requires
both to be non-empty. It does not validate either value against a fixed list.
RMS determines whether each role/vendor/product-family combination is supported
when a request is made. See
[Supported RMS descriptor combinations](../../../../docs/configuration/component-manager-rms.md#supported-rms-descriptor-combinations),
including VRNVL72.

For product families other than `gb200` and `gb300`, the `GetRackProfile`
`product_family` enum is `UNSPECIFIED`. The configured string remains available
to descriptor-based RMS operations.

Each `rack_capabilities.<role>` section also requires a `count` field. This
field is independent of RMS: it tells the rack state machine how many devices
with that role the rack must have before it can progress. A rack stays in
`Created` until all three roles have at least `count` devices registered; it
stays in `Discovering` until all three roles have at least `count` devices in
`Ready` state. All three roles — `compute`, `switch`, and `power_shelf` —
require a `count` regardless of which backends are set to `rms`. The third
example below shows `count` on `compute` and `switch` even though those roles
use non-RMS backends.

The examples below only show the component-manager and rack-profile fields.
Configure `[rms]` separately when NICo needs to call RMS.
The `nsm` and `psm` backend values require externally managed services; the
NICo deployment charts do not install NSM or PSM.

Example: GB200 rack where all component-manager roles use RMS:

```toml
[component_manager]
compute_tray_backend = "rms"
nv_switch_backend = "rms"
power_shelf_backend = "rms"

[rack_profiles.NVL72]
product_family = "gb200"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

[rack_profiles.NVL72.firmware_object]
url = "https://firmware.example.com/objects/nvl72.json"
fetch_timeout = "30s"

[rack_profiles.NVL72.rack_capabilities.compute]
vendor = "NVIDIA"
count = 18

[rack_profiles.NVL72.rack_capabilities.switch]
vendor = "NVIDIA"
count = 9

[rack_profiles.NVL72.rack_capabilities.power_shelf]
vendor = "LiteOn"
count = 8
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
count = 18

[rack_profiles.NVL72_GB300.rack_capabilities.switch]
vendor = "nvidia"
count = 9

[rack_profiles.NVL72_GB300.rack_capabilities.power_shelf]
vendor = "delta"
count = 6
```

Example: only the component-manager power shelf backend uses RMS. The compute
and switch component-manager backends are explicitly set to non-RMS values, so
component-manager startup validation only requires the power shelf vendor field:

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

[rack_profiles.NVL72_POWER.rack_capabilities.compute]
count = 18

[rack_profiles.NVL72_POWER.rack_capabilities.switch]
count = 9

[rack_profiles.NVL72_POWER.rack_capabilities.power_shelf]
vendor = "Lite-On"
count = 8
```

Each rack that uses an RMS-backed operation must have a `rack_profile_id`
matching a key under `[rack_profiles]`. Startup validation does not scan
existing rack database rows, so missing or unknown per-rack profile IDs are
still checked when an RMS operation runs.

The separate site-explorer machine-ingestion RMS slot/tray lookup also uses the
rack profile to build a compute node descriptor. If that path is enabled for
machines with rack IDs, the profile also needs compute product-family and vendor
data even when `compute_tray_backend` is not `rms`.

NICo accepts non-empty product-family strings. RMS evaluates descriptor support
when an operation runs. The optional `rack_hardware_topology` field remains
available for topology-specific flows.

---

## Sub-Structs

### `ApiAdmissionControlConfig`

Admission control places each authenticated client in its own bounded FIFO and
schedules those clients fairly within the global execution and pending-request
budgets. The default per-client limits apply to external users, SPIFFE machines,
SPIFFE services without an override, and requests without a recognized client
identity. An exact SPIFFE service override can give a trusted internal service a
different share without allowing it to exceed either global bound.

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `true` | Enable fair, bounded API admission. When `false`, admission is bypassed and the other fields in this section are not validated. |
| `max_work_in_flight` | `usize` | `64` | Maximum business requests executing concurrently. When enabled, must be greater than zero and no greater than `tokio::sync::Semaphore::MAX_PERMITS`. |
| `max_pending` | `usize` | `1024` | Maximum business requests waiting for execution. When enabled, must be greater than zero and no greater than `tokio::sync::Semaphore::MAX_PERMITS`. |
| `max_work_in_flight_per_client` | `usize` | `8` | Default maximum business requests from one client that may execute concurrently. Must be greater than zero, no greater than `max_work_in_flight`, and representable as a `u32`. |
| `max_pending_per_client` | `usize` | `64` | Default hard bound on pending business requests from one client. Must be greater than zero and no greater than `max_pending`. |
| `pending_timeout` | `Duration` | `5s` | Default maximum time a client's pending request may wait for execution. Admission can reject earlier when its queue-delay estimate exceeds this value. Must be greater than zero. |
| `client_idle_timeout` | `Duration` | `5m` | How long an empty, inactive client's scheduler state is cached before cleanup. Must be greater than zero. |
| `service_limits` | `BTreeMap<String, ApiAdmissionServiceLimitsConfig>` | `{}` | Per-service overrides keyed by the exact SPIFFE service identifier described below. Unlisted services use the default per-client limits. |

#### `ApiAdmissionServiceLimitsConfig`

Every field is required for each service override; the nested structure has no
field-level defaults.

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `max_work_in_flight` | `usize` | **required** | Maximum requests from this service that may execute concurrently. Must be greater than zero, no greater than the global `max_work_in_flight`, and representable as a `u32`. |
| `max_pending` | `usize` | **required** | Hard bound on this service's pending requests. Must be greater than zero and no greater than the global `max_pending`. |
| `pending_timeout` | `Duration` | **required** | Maximum time this service's pending request may wait for execution. Admission can reject earlier when its queue-delay estimate exceeds this value. Must be greater than zero. |

The map key is the identifier extracted by removing one of
`auth.trust.spiffe_service_base_paths` from the certificate's SPIFFE path. For
example, a SPIFFE path `/forge-system/sa/scout` with base path
`/forge-system/sa/` has service identifier `scout`:

```toml
[api_admission_control.service_limits.scout]
max_work_in_flight = 16
max_pending = 128
pending_timeout = "5s"
```

Matching is exact and case-sensitive. Keys are not trimmed, expanded as
prefixes, or interpreted as globs. Configure `scout`, not the full SPIFFE URI
and not the rendered principal `spiffe-service-id/scout`. An override key must
contain at least one non-whitespace character. Quote a TOML key when the
extracted identifier contains characters such as `/` or `.`.

### `TlsConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `root_cafile_path` | `String` | `""` | Root CA certificate for client validation. |
| `identity_pemfile_path` | `String` | `""` | Server identity certificate PEM. |
| `identity_keyfile_path` | `String` | `""` | Server identity private key. |
| `admin_root_cafile_path` | `String` | `""` | Admin root CA for admin client validation. |

### `NodeAuthConfig`

Node (Scout / DPU-agent) authentication. Bearer tokens are off by default, so
the default is machine mTLS exactly as before. See
`docs/design/machine-identity/node-auth-jwt.md`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Accept `Authorization: Bearer` node JWTs. Nodes self-sign these with their existing mTLS client-certificate key and carry the certificate in the token's `x5c` header; the API verifies it against `[tls] root_cafile_path`. Requires a TLS listener -- the API refuses to accept bearer tokens over plaintext. |
| `mtls_enabled` | `bool` | `true` | Accept machine mTLS client certificates as node identity. Turn off only once the fleet presents bearer tokens; startup fails if this and `enabled` are both false. Scoped to machine certificates -- service and admin-CLI certificates are unaffected. Requires a TLS listener to mean anything: a plaintext `listen_mode` presents no peer certificates, so this silently authenticates nobody. |
| `max_token_ttl_sec` | `u32` | `900` | Longest accepted token lifetime, in seconds. Clients mint 300 s tokens; this caps how far a client may push `exp`. Must be greater than zero and at most 86400. |
| `fmds_use_node_tokens` | `Option<bool>` | *(unset)* | Whether DPF-deployed fmds is rendered in token mode. Unset follows `enabled`, which is what almost every site wants. Set it to `false` while `enabled` is still `true` to move fmds back to client certificates *first* -- the supported way to stage a disable, since the API stops accepting tokens the moment it restarts while fmds keeps presenting them until DPF has rolled every DaemonSet. `true` with `enabled = false` is refused at startup. |

Both mechanisms need `listen_mode = "tls"`. Bearer tokens are refused over
plaintext explicitly, at startup; machine mTLS simply has no certificates to
inspect, because a plaintext listener hands the middleware an empty peer-cert
list. The `enabled = false` + `mtls_enabled = false` lockout check therefore
guarantees a working node-auth path *only on a TLS listener* -- on plaintext,
`mtls_enabled = true` satisfies the check while authenticating nobody. No
shipped configuration selects a plaintext mode.

### `AuthConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `permissive_mode` | `bool` | — | Enable permissive authorization (dev mode). |
| `casbin_policy_file` | `Option<PathBuf>` | — | Path to Casbin CSV policy file. |
| `cli_certs` | `Option<AllowedCertCriteria>` | — | Additional allowed cert criteria for nico-admin-cli. |
| `trust` | `Option<TrustConfig>` | — | SPIFFE trust domain and allowed paths for client certs. |

### `IBFabricConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enables InfiniBand fabric management. |
| `max_partition_per_tenant` | `i32` | `31` | Maximum IB partitions per tenant (1-31). |
| `allow_insecure` | `bool` | `false` | Allow insecure fabric configs that skip tenant isolation. |
| `mtu` | `IBMtu` | *(default)* | MTU for IB fabric traffic. |
| `rate_limit` | `IBRateLimit` | *(default)* | Rate limit for IB traffic. |
| `service_level` | `IBServiceLevel` | *(default)* | QoS service level for IB packets. |
| `fabric_monitor_run_interval` | `Duration` | `60s` | Interval for the IB fabric monitor. |

### `NvLinkConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enables NvLink partitioning. |
| `monitor_run_interval` | `Duration` | `60s` | NvLink monitor polling interval. |
| `nmx_c_tls_ca_cert_path` | `Option<String>` | — | Extra CA bundle for verifying the NMX-C server over HTTPS. |
| `nmx_c_tls_client_cert_path` | `Option<String>` | — | Client certificate for mTLS to NMX-C. |
| `nmx_c_tls_client_key_path` | `Option<String>` | — | Client private key for mTLS to NMX-C. |
| `nmx_c_tls_authority` | `Option<String>` | — | TLS server name used for SNI and certificate verification. |
| `allow_insecure` | `bool` | `false` | Skip TLS verification for NMX-C. |
| `nmx_c_endpoint_port` | `Option<u16>` | — | TCP port for NMX-C endpoints derived from switch NVOS IP. Unset uses the production NMX-C port. |
| `nmx_c_certificate_rotation` | `NmxCCertificateRotationConfig` | *(default)* | Optional expiry-driven rotation for NMX-C server certificates. |
| `partition_monitor_max_concurrent_groups` | `NonZeroUsize` | `16` | Maximum number of NMX-C machine groups (chassis or rack) processed concurrently per monitor iteration. Bounds DB pool usage and gRPC fan-out. Must be ≥ 1. |

### `NmxCCertificateRotationConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enables NMX-C server certificate expiry checks and rotation. |
| `run_interval` | `Duration` | `1h` | Interval between checks of the certificate served by NMX-C. |
| `rotate_before_expiry` | `Duration` | `1w` | Requests rotation when the served certificate expires within this duration. Must leave enough time for the replacement certificate to be issued first. |
| `probe_timeout` | `Duration` | `10s` | Timeout for each NMX-C certificate probe operation. |

`expiry_warning_window` remains accepted as a deprecated alias for
`rotate_before_expiry`; its value now controls rotation rather than warning only.

### `SiteExplorerConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `true` | Enables hardware discovery. |
| `run_interval` | `Duration` | `120s` | Interval between exploration runs. |
| `concurrent_explorations` | `u64` | `100` | Max nodes explored in parallel. |
| `explorations_per_run` | `u64` | `360` | Max nodes explored per run. |
| `create_machines` | `bool` | `true` | When false, SiteExplorer skips creating ManagedHost state machines; the DPU agent (scout) must self-register via DiscoverMachine gRPC endpoint with create_machine=true. Dynamically toggleable. |
| `machines_created_per_run` | `u64` | `100` | Max ManagedHosts created per run. |
| `rotate_switch_nvos_credentials` | `bool` | `false` | Auto-rotate switch NVOS admin credentials. |
| `override_target_ip` | `Option<String>` | — | **Deprecated.** Use `bmc_proxy`. Debug BMC IP override. |
| `override_target_port` | `Option<u16>` | — | **Deprecated.** Use `bmc_proxy`. Debug BMC port override. |
| `bmc_proxy` | `HostPortPair` | — | BMC proxy host:port for integration testing/dev. |
| `allow_changing_bmc_proxy` | `Option<bool>` | *(auto)* | Allow runtime changes to `bmc_proxy`. Auto-detected from initial config. |
| `reset_rate_limit` | `Duration` | `1h` | Minimum time between SiteExplorer-initiated BMC resets. |
| `admin_segment_type_non_dpu` | `bool` | `false` | Non-DPU hosts use `HostInband` admin segment type. |
| `create_power_shelves` | `bool` | `true` | Auto-create Power Shelf state machines for explored shelves with a matching `expected_power_shelves` record. Shelves are discovered at their `expected_power_shelves` static IP even without a DHCP lease. |
| `power_shelves_created_per_run` | `u64` | `1` | Max power shelves created per run. |
| `create_switches` | `bool` | `true` | Auto-create Switch state machines for explored switches with a matching `expected_switches` record. |
| `switches_created_per_run` | `u64` | `9` | Max switches created per run. |
| `explore_mode` | `SiteExplorerExploreMode` | `NvRedfish` | Redfish backend: `libredfish`, `nv-redfish`, or `compare-result`. |
| `dpu_policy` | `Option<HostDpuPolicy>` | — (effective: `manage`) | Site-wide policy for DPU hardware: `manage`, `nic`, or `ignore`. Per-host `nic` and `ignore` override it; per-host `manage` inherits it for backward compatibility. When omitted, the site default is `manage`. The previous `use_as_nic` value and the legacy `dpu_mode` field with `dpu_mode` / `nic_mode` / `no_dpu` values remain accepted during deserialization. |

### `StateControllerConfig`

Shared by all `*StateControllerConfig` structs (machine, network segment, VPC prefix, extension
service, IB partition, DPA interface, rack, power shelf, switch, SPDM).

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `iteration_time` | `Duration` | `30s` | Target duration for one state controller iteration. |
| `max_object_handling_time` | `Duration` | `3m` | Timeout for evaluating/advancing a single object's state. |
| `max_concurrency` | `usize` | `10` | Max objects advanced in parallel. |
| `processor_dispatch_interval` | `Duration` | `2s` | Max wait time when checking for and dispatching new tasks. |
| `processor_log_interval` | `Duration` | `60s` | How often the processor emits log messages. |
| `metric_emission_interval` | `Duration` | `60s` | How often aggregate metrics are recalculated. |
| `metric_hold_time` | `Duration` | `5m` | How long per-object metrics are held before eviction. |

### `RackStateControllerConfig`

TOML section: `[rack_state_controller]`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `controller` | `StateControllerConfig` | *(default)* | Common state controller timing (see [StateControllerConfig](#statecontrollerconfig)). |
| `nmx_cluster_switch_mtls_services` | `Vec<SwitchMtlsService>` | N/A (ignored) | **Deprecated.** Accepted and ignored. Rack maintenance does not configure switch certificates. |

### `SwitchStateControllerConfig`

TOML section: `[switch_state_controller]`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `controller` | `StateControllerConfig` | *(default)* | Common state controller timing (see [StateControllerConfig](#statecontrollerconfig)). |
| `switch_mtls_services` | `Vec<SwitchMtlsService>` | all four values below | mTLS certificate bindings applied by switch state-controller operations and direct `ComponentConfigureSwitchCertificate` RPC calls. A non-empty list replaces the default. Omission and `[]` both use the default. |

`switch_mtls_services` accepts these RMS service values:

| Value | RMS service description |
|-------|-------------------------|
| `nvue_api` | NVUE REST API service |
| `scale_up_fabric_telemetry` | Scale-up fabric telemetry service |
| `scale_up_fabric_manager` | Scale-up fabric manager service |
| `scale_up_fabric_telemetry_interface` | Scale-up fabric telemetry interface service |

`switch_mtls_services` selects server-side certificate bindings. It does not
enable the underlying service. For workflow scope, see
[Switch Certificate Configuration](https://docs.nvidia.com/infra-controller/documentation/architecture/state-machines/switch-certificate-configuration).

### `ObservabilityConfig`

TOML section: `[observability]`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `per_object_metrics_for_classifications` | `Vec<HealthAlertClassification>` | `[]` | Health alert classifications for which the per-object metric `carbide_object_unhealthy_by_classification_count` is emitted, labeled with `object_type` (e.g. `machine`, `switch`, `rack`, `power_shelf`) and `object_id`. Each entry adds up to one extra time series per matching object, so it defaults to empty (disabled) to keep metric cardinality bounded. When empty, the metric is not registered or exposed at all; aggregate health metrics are unaffected regardless. |
| `per_object_state_metrics` | `PerObjectStateMetricsConfig` | disabled | High-cardinality per-object state, SLA, manual-intervention, trait, and association metrics served from a dedicated listener. |

### `PerObjectStateMetricsConfig`

TOML section: `[observability.per_object_state_metrics]`.

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Registers per-object state metrics and starts the dedicated listener. When `object_types` is empty, registration and the listener are both skipped even if this is `true`. |
| `listen_address` | `SocketAddr` | `[::]:9091` | Dual-stack address serving `/metrics`; the Helm chart derives it from `service.perObjectStateMetrics.port`. |
| `object_types` | `Vec<PerObjectStateMetricObjectType>` | all supported types | Types to publish: `machine`, `switch`, `power_shelf`, `rack`, `network_segment`, `vpc_prefix`, `spdm_attestation`, and/or `ib_partition`. An empty list publishes no state series and skips the dedicated listener. |

### `MachineStateControllerConfig`

Extends `StateControllerConfig` with:

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `dpu_wait_time` | `Duration` | `5m` | Time before a DPU is considered definitively down. |
| `power_down_wait` | `Duration` | `2m` | Wait after power-down before powering on. |
| `failure_retry_time` | `Duration` | `90m` | Time before re-triggering reboot if machine hasn't called back. |
| `dpu_up_threshold` | `Duration` | `5m` | Max time without DPU health report before assuming it's down. |
| `scout_reporting_timeout` | `Duration` | `5m` | Duration without scout report before host is unhealthy. |
| `waiting_for_measurements_timeout` | `Duration` | `4h` | How long a host may remain in WaitingForMeasurements before being escalated to Failed. |
| `uefi_boot_wait` | `Duration` | `5m` | Wait time for UEFI boot completion after host reboot. |
| `max_bios_config_retries` | `u32` | `3` | Shared retry budget for automated host boot-configuration convergence across BIOS recovery and boot-order verification. |
| `polling_bios_setup_stuck_threshold` | `Duration` | `15m` | Time in PollingBiosSetup with `is_bios_setup == false` before recovery escalation. |
| `boot_interface_observation_interval` | `Duration` | `10m` | Positive time between successful Redfish observations of an already-verified boot interface. |
| `controller` | `StateControllerConfig` | *(default)* | Common state controller timing (see [StateControllerConfig](#statecontrollerconfig)). |

The Redfish observation is read-only. A successful match refreshes the last
observation timestamp. A mismatch records a new pending generation for the same
desired target: Ready enters the existing boot-configuration flow on its next
controller sweep, while Assigned defers remediation until release. Failed reads
and skipped observations preserve the last successful observation and retry on
a later controller iteration.

The controller skips periodic observation for locked Supermicro hosts because
their reported boot-order view remains stale until lockdown is disabled and the
host is rebooted. Profiles configured with `disable_lockdown = true` use the
normal observation path.

### `NetworkSegmentStateControllerConfig`

Extends `StateControllerConfig` with:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `network_segment_drain_time` | `Duration` | `5m` | Time a network segment must have 0 allocated IPs before release. |
| `controller` | `StateControllerConfig` | *(default)* | Common state controller timing (see [StateControllerConfig](#statecontrollerconfig)). |

### `VpcPrefixStateControllerConfig`

Extends `StateControllerConfig` with:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `vpc_prefix_drain_time` | `Duration` | `5m` | Time a VPC prefix must have 0 referencing network prefixes before release. |
| `controller` | `StateControllerConfig` | *(default)* | Common state controller timing (see [StateControllerConfig](#statecontrollerconfig)). |

### `ExtensionServiceStateControllerConfig`

TOML section: `[extension_service_state_controller]`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `controller` | `StateControllerConfig` | *(default)* | Common state controller timing (see [StateControllerConfig](#statecontrollerconfig)). |

### `FirmwareGlobal`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
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
| `firmware_download_cache_directory` | `PathBuf` | `/mnt/persistence/fw/download-cache` | Writable directory used to cache downloaded firmware artifacts. |
| `max_concurrent_bfb_copies` | `usize` | `10` | Maximum number of concurrent BFB copy operations. |

### `MachineUpdater`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `instance_autoreboot_period` | `Option<TimePeriod>` | — | UTC time window for automatic machine reboots. |
| `max_concurrent_machine_updates_absolute` | `Option<i32>` | — | Hard cap on concurrent machine updates. |
| `max_concurrent_machine_updates_percent` | `Option<i32>` | — | Percentage cap on concurrent updates (lesser of absolute/percent is used). |

### `PowerManagerOptions`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enable power management. |
| `next_try_duration_on_success` | `Duration` | `5m` | Retry interval after successful power operation. |
| `next_try_duration_on_failure` | `Duration` | `2m` | Retry interval after failed power operation. |
| `wait_duration_until_host_reboot` | `Duration` | `15m` | Wait after power-down before powering on host. |

### `VmaasConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `allow_instance_vf` | `bool` | `true` | Global instance-VF admission switch for creation and network updates. `false` rejects every VF. With DPF intercept topology, `true` additionally requires every requested VF ID to be explicitly selected by that topology. Without DPF intercept topology, the historical boolean-only admission behavior is preserved. |
| `hbn_reps` | `Option<String>` | — | Select which representors from the configured VF population HBN is expected to use during DPU provisioning. When omitted, HBN uses its default representor selection. |
| `bridging` | `Option<HostRepresentorBridgingConfig>` | — | Provisioning-time topology for bridges inserted between host representors and HBN or DPF's `br-sfc`. Under DPF, a present `vmaas_config` makes this map the complete configurable PF/VF inventory and requires exactly one PF entry. |

### `HostRepresentorBridgingConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `hbn_bridge` | `String` | `"br-hbn"` | HBN/SFC bridge that host-representor patch ports attach to during BlueField provisioning. |
| `host_representor_intercept_bridging` | `HashMap<String, HostInterceptBridging>` | `{}` | Host-owned PF/VF representor bridge layout keyed by the legacy representor name. Non-skipped entries are sent to pre-DPF BlueField provisioning as `<representor>:<bridge>:<patch_port>`. When DPF is enabled, this map replaces the static PF/VF inventory; an absent map, empty map, or VF-only map is rejected because FMDS requires the selected PF. |

### `HostInterceptBridging`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `bridge` | `String` | **required** | Bridge that sits between the host PF/VF representor and br-hbn or br-sfc. |
| `patch_port` | `String` | **required** | Patch port on this bridge that connects it toward HBN or SFC. |
| `skip_create` | `bool` | `false` | When true, the entry is omitted from provisioning-time bridge creation. |
| `dpf_interface` | `Option<DpfInterfaceIdentity>` | — | Typed DPF PF/VF selection. Optional and ignored by pre-DPF provisioning, but required for every map entry when DPF is enabled. The surrounding legacy map key is never parsed as DPF identity. |

When DPF is enabled, `skip_create=true` is rejected. `bridge` is a Linux netdev name and must contain 1–15 lowercase ASCII letters, digits, or hyphens and start with a letter. `patch_port` is an OVS patch-interface name rather than a Linux netdev: it must be non-empty, start with a lowercase ASCII letter, and contain only lowercase ASCII letters, digits, hyphens, or underscores. NICo does not apply the Linux 15-character limit to patch ports. Both names must be unique across the rendered OVS topology. DPF requires exactly one configured PF and also rejects duplicate identities, generated-name collisions, BF3 raw-representor collisions, configurations spanning more than one selected controller/PF parent, hardware VF counts above 126, and VFs whose `vf_id` is greater than 15 or greater than or equal to `dpu_config.num_of_vfs`. PF selection is independent of that VF count.

Pre-DPF configurations retain their existing behavior: `dpf_interface` may be
omitted, `skip_create=true` remains valid, `hbn_bridge` still defaults to
`br-hbn`, and the sorted provisioning value remains exactly
`<representor>:<bridge>:<patch_port>`. DPF ignores `hbn_reps`, derives
`br-sfc` internally, and uses the typed identity rather than the map key.

With a configured DPF intercept inventory, the topology is exclusively the complete VMaaS-managed PF/VF inventory. NICo retains the fixed `p0` and `p1` physical interfaces and assigns both to HBN. Every configured PF or VF becomes a Patch interface between `br-sfc` and its configured intermediate bridge and is assigned to HBN and DHCP; only the selected PF is also assigned to FMDS. The selected hardware PF is exposed inside HBN as `pf0hpf_if`, and VF identity `vf_id` as `pf0vf{vf_id}_if`, regardless of the configured controller and PF identifiers. DHCP exposes them as `d_pf0hpf_if` and `d_pf0vf{vf_id}_if`; FMDS exposes the selected PF as `f_pf0hpf_if`. For `P` configured PFs and `V` configured VFs, NICo manages `2 + P + V` HBN endpoints, `P + V` DHCP endpoints, and `P` FMDS endpoints, for `2 + 3P + 2V` total SF-backed service endpoints. The one-selected-PF contract requires `P = 1`; together with the VF15 bound, it permits at most 19 generated HBN interfaces. The generated HBN inventory may not exceed 32 interfaces. With `allow_instance_vf=true`, instance creation and network updates admit only the explicitly configured VF IDs; a PF-only topology therefore admits no instance VFs.

Without configured DPF intercept topology, NICo deliberately preserves the established static VF0–VF13 inventory, its VF0–VF7 DHCP subset, and historical instance admission behavior. In this mode, `pf_total_sf_reserved` is the complete `PF_TOTAL_SF`; it defaults to `30`, and explicit operator overrides remain supported. Reconciling the inventory, DHCP, or admission surfaces would change existing ServiceInterfaces and the hashed DPUFlavor, so it is deferred to a separately planned cleanup and DPU re-ingestion migration.

### `DpfInterfaceIdentity`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `controller_id` | `u8` | **required** | DPF controller number (`0..=255`) containing the selected PF or VF. |
| `pf_id` | `u8` | **required** | PF identifier (`0..=255`) on the selected controller. |
| `vf_id` | `Option<u8>` | — | VF identifier. Omission selects the PF. Under DPF intercept topology, presence selects that VMaaS VF and requires both `vf_id <= 15` and `vf_id < dpu_config.num_of_vfs`; pre-DPF provisioning ignores this typed field. |

### `DpuConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `bootstrap_ca_source` | `BootstrapCaSource` | `legacy_download` | How non-DPF DPUs obtain the API trust anchor: `legacy_download`, `embedded`, or `mounted`. Omitting the field preserves the historical PXE download. The field is not sent to host Scout boots. Non-network modes do not fall back to downloading. |
| `dpu_nic_firmware_initial_update_enabled` | `bool` | `false` | Enable DPU NIC firmware updates on initial discovery. |
| `dpu_nic_firmware_reprovision_update_enabled` | `bool` | `true` | Enable DPU NIC firmware updates on reprovisioning. |
| `dpu_models` | `HashMap<String, Firmware>` | *(BF2+BF3 defaults)* | DPU model firmware definitions. |
| `dpu_nic_firmware_update_versions` | `Vec<String>` | *(BF2+BF3 NIC versions)* | DPU NIC firmware version strings. |
| `dpu_enable_secure_boot` | `bool` | `false` | Enable secure boot flow for DPU provisioning via Redfish. |
| `num_of_vfs` | `u32` | `16` | Number of hardware VFs configured per DPU PF during BlueField provisioning. Max `126`. Under DPF, changing this value changes the immutable BF3/generic-BF4 flavor and requires a carbide-api restart and DPU reprovisioning. Reducing it below the static inventory's previous effective VF count also removes desired VF ServiceInterfaces; because NICo does not prune them, operators must stop NICo, remove the omitted NICo ServiceInterfaces, re-ingest the DPUs, and restart. Configured intercept inventories remain valid only while every selected `vf_id` is both lower than this value and no greater than 15. |
| `service_vpc_slot_count` | `u32` | `0` | Number of HBN interfaces reserved for externally coordinated service-VPC attachments on BF3 and generic BF4. NICo generates stable names from `iface_svc_0` through `iface_svc_{N-1}`. The generated interfaces count toward HBN's 32-interface limit and increase its `nvidia.com/bf_sf` request. BF4 Astra ignores this field. |
| `additional_managed_sf` | `u32` | `0` | Additional BF3/generic-BF4 SF capacity without a generated HBN interface. This value and `service_vpc_slot_count` are added to the managed SF count used to size or validate `PF_TOTAL_SF`. BF4 Astra ignores this field. |
| `restart_ovs_on_use_admin_network_change` | `bool` | `false` | Restart OVS on DPU-OS agents when host `use_admin_network` changes. Containerized agents skip the local service restart and still ACK the network config. |

With intercept bridging, both SF settings increase `PF_TOTAL_SF`, change the
`DPUFlavor`, and require controlled DPU reprovisioning. Without intercept
bridging, they consume the unchanged legacy `pf_total_sf_reserved` pool, and
startup rejects an overcommit. NICo does not create bridges, ServiceInterfaces,
service chains, IPAM, or application-service CRs for service-VPC slots; an
external controller must coordinate them. Changes are read at API startup.

To use `embedded`, build a site-specific BFB with an explicit
`BOOTSTRAP_CA_PATH`. The build provides no repository or default CA fallback
for the dedicated embedded payload. Existing legacy artifact inputs remain
unchanged. It stages the source at `/opt/forge/embedded_forge_root.pem`.

This path is separate from `/opt/forge/forge_root.pem`, which the provisioning
environment must populate for `mounted`. NICo does not create that mount. Both
modes fail closed when their own bundle is absent or invalid. Changing this
setting affects the next DPU network boot or reprovisioning. It does not affect
host Scout boots or rewrite installed DPUs in place. The selected CA validates
the NICo API server certificate. This validation remains necessary even when
client-certificate authentication is not used.

### `NetworkSecurityGroupConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `max_network_security_group_size` | `u32` | `200` | Max expanded rules per NSG. |
| `stateful_acls_enabled` | `bool` | `true` | Allow stateful NSG creation and stateless-to-stateful updates, and enable supporting NVUE configuration on DPUs. When disabled, existing stateful NSGs remain editable but behave statelessly. |
| `policy_overrides` | `Vec<NetworkSecurityGroupRule>` | `[]` | NSG rules injected before user-defined rules. |

### `FnnConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `admin_vpc` | `Option<AdminFnnConfig>` | — | FNN configuration for the admin network VPC. |
| `common_internal_route_target` | `Option<RouteTargetConfig>` | — | Double-tag for internal tenant routes (consumed by the network infrastructure). |
| `additional_route_target_imports` | `Vec<RouteTargetConfig>` | `[]` | Extra route targets imported on DPU VRFs. |
| `routing_profiles` | `HashMap<String, FnnRoutingProfileConfig>` | `{}` | Named per-VPC routing profiles (see [FnnRoutingProfileConfig](#fnnroutingprofileconfig)). |
| `use_vpc_vrf_loopback` | `bool` | `false` | Whether IPs are allocated for VPC loopbacks. When false, the VPC loopback pool is unused and no VPC/VRF loopback IP is sent to the DPU. |

### `FnnRoutingProfileConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `route_target_imports` | `Option<Vec<RouteTargetConfig>>` | — (effective `[]`) | Route targets imported into DPU VRFs for VPC routes. |
| `route_targets_on_exports` | `Option<Vec<RouteTargetConfig>>` | — (effective `[]`) | Route targets added to routes exported by the DPU. |
| `internal` | `Option<bool>` | — (effective `false`) | Whether the profile uses internal VNI allocation. This property cannot be overridden on a VPC. |
| `tenant_prefix_overlap_eligible` | `bool` | `false` | Base routing profile opt-in for [tenant prefix overlap checks](#tenant-prefix-overlap-checks). This setting cannot be overridden on a VPC. |
| `leak_default_route_from_underlay` | `Option<bool>` | — (effective `false`) | Leak the default route from the underlay/default VRF into tenant VRFs. |
| `leak_tenant_host_routes_to_underlay` | `Option<bool>` | — (effective `false`) | Leak tenant host routes into the underlay/default VRF. |
| `tenant_leak_communities_accepted` | `Option<bool>` | — (effective `false`) | Honor route-leak communities sent by the tenant host OS. |
| `accepted_leaks_from_underlay` | `Option<Vec<PrefixFilterPolicyEntry>>` | — (effective `[]`) | Specific underlay/default VRF prefixes allowed to leak into tenant VRFs. Routing only; does not affect ACLs. |
| `allowed_anycast_prefixes` | `Option<Vec<PrefixFilterPolicyEntry>>` | — (effective `[]`) | IPv4 or IPv6 prefixes that tenant hosts are allowed to announce to the DPU as anycast routes. |
| `access_tier` | `Option<u32>` | — (effective `0`) | Routing profile access tier. Lower values grant broader access. This property cannot be overridden on a VPC. |

Unset properties retain presence information so a VPC's inline
`routing_profile_overrides` can inherit them. After the named profile and VPC
override are combined, properties still unset use the effective defaults above.

### Tenant prefix overlap checks

`tenant_prefix_overlap_enabled` defaults to `false`. When set to `true`, NICo
checks whether two `VpcPrefix` records may reuse the same CIDR. It does not
permit direct `NetworkPrefix` reuse or change the database constraints.

An overlapping `VpcPrefix` pair is eligible only when all of these conditions
are true:

- The requested and existing CIDRs are identical, the existing `VpcPrefix` is
  not deleted, and the `VpcPrefix` records belong to different VPCs and tenant
  organizations.
- Both VPCs use FNN and have assigned, distinct status VNIs.
- Each `VpcPrefix` is linked to a tenant-managed, `DatacenterOnly` `SitePrefix`
  owned by its VPC tenant and containing the `VpcPrefix` CIDR. The requested
  `SitePrefix` must be `Ready`; the existing `SitePrefix` may be `Ready` or
  `Deleting`.
- Site-wide `vpc_isolation_behavior` is `"mutual_isolation"`.
- `site_global_vpc_vni` and `common_internal_route_target` are unset, and
  `additional_route_target_imports` is empty, so they cannot bridge the VPCs.
- Each resolved FNN profile, after applying its VPC overrides, has
  `tenant_prefix_overlap_eligible = true` and `internal = true`; has no import
  or export route targets; disables default-route leakage, tenant-host-route
  leakage, and tenant leak communities; and has no accepted underlay leaks or
  allowed anycast prefixes.

The gRPC `CreateNetworkSegment` and `AttachNetworkSegmentToVpc` handlers reject
any direct prefix that overlaps a `VpcPrefix`, regardless of the site gate. The
gRPC `CreateVpcPrefix` handler considers prefixes on attached segments. It may
adopt only direct Tenant segment prefixes in the same VPC that are not already
linked to a `VpcPrefix`; every other direct `NetworkPrefix` overlap on an
attached segment is rejected. An unattached `CreateNetworkSegment` request does
not run these checks, but a later attachment does.

These handlers do not validate changes to peering or VPC policy, or Instance
paths that retain routing state. They also do not cover startup or audit every
writer. Those checks are tracked in
[#5114](https://github.com/dsx-ai-factory/infra-controller/issues/5114) and
[#5115](https://github.com/dsx-ai-factory/infra-controller/issues/5115), while startup
and complete writer coverage are tracked in
[#5116](https://github.com/dsx-ai-factory/infra-controller/issues/5116). All three must
land before the database cutover in
[#3892](https://github.com/dsx-ai-factory/infra-controller/issues/3892).

Even when the application accepts an eligible pair, the existing `VpcPrefix`
exclusion rejects overlapping `VpcPrefix` persistence until the cutover tracked
by [#3892](https://github.com/dsx-ai-factory/infra-controller/issues/3892).

### `VpcDefinition`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `organization_id` | `Option<String>` | — | Tenant organization that owns the seeded VPC. |
| `network_virtualization_type` | `VpcVirtualizationType` | **required** | Data plane used by the VPC. |
| `routing_profile_type` | `Option<String>` | — | Named FNN routing profile recorded on the seeded VPC. |
| `routing_profile_overrides` | `Option<VpcRoutingProfileOverrides>` | — | Unsupported for seeded VPCs. Any configured value causes startup to fail; inline overrides are accepted only by VPC creation or update requests. |
| `vni` | `Option<i32>` | — | Desired VNI; when absent, one is allocated. |

### `PrefixFilterPolicyEntry`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `prefix` | `IpNetwork` | **required** | IPv4 or IPv6 CIDR prefix accepted by a prefix-list policy. |

### `EwEthersConfig`

Legacy site files may still name this section `[dpa_config]` (accepted as an
alias) and may inline `mqtt_endpoint`, `mqtt_broker_port`, `hb_interval`, and
`auth`; those keys are migrated into `svpc` at load time with a deprecation
warning. Nest them under `[ewethers_config.svpc]` in new configurations.

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enable Cluster Interconnect Network. |
| `svpc_enabled` | `bool` | `false` | Enable the SVPC path. Not mutually exclusive with `astra_enabled`. |
| `astra_enabled` | `bool` | `false` | Enable the Astra path. Not mutually exclusive with `svpc_enabled`. |
| `subnet_ip` | `Ipv4Addr` | `0.0.0.0` | Base IPv4 address of the DPA subnet. |
| `subnet_mask` | `i32` | `0` | CIDR prefix length for the DPA subnet. |
| `monitor_run_interval` | `Duration` | `60s` | The interval at which the DPA monitor runs. |
| `svpc` | `SvpcConfig` | *(defaults)* | SVPC MQTT connection settings (see [SvpcConfig](#svpcconfig)). |

### `SvpcConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `mqtt_endpoint` | `String` | `"mqtt.forge"` | MQTT broker host for the SVPC path. |
| `mqtt_broker_port` | `u16` | `1884` | MQTT broker port. |
| `hb_interval` | `Duration` | `2m` | Heartbeat interval for DPA health checks. |
| `auth` | `MqttAuthConfig` | *(none)* | MQTT authentication settings. |

### `DsxExchangeEventBusConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enable the DSX Exchange Event Bus for managed-host state publishing, BMS metadata subscription, and BMS rack/isolation/heartbeat publishing. |
| `mqtt_endpoint` | `String` | `"mqtt.nico"` | MQTT broker host. |
| `mqtt_broker_port` | `u16` | `1884` | MQTT broker port. |
| `publish_timeout` | `Duration` | `1s` | Timeout for MQTT publish operations. |
| `queue_capacity` | `usize` | `1024` | Event buffer size for DSX publish work (events dropped when full). |
| `auth` | `MqttAuthConfig` | *(none)* | MQTT authentication settings. |
| `topic_prefix` | `String` | `NICO/v1/machine` | Topic prefix used when publishing `ManagedHostState` transitions; the full topic is `{topic_prefix}/{machineId}/state`. NATS subjects are case-sensitive, so this must match the producer pub allow configured on the broker. |
| `periodic_state_republish` | `PeriodicStateRepublishConfig` | *(enabled)* | Periodically re-publish current managed-host state so consumers that miss change events can reconcile (see [PeriodicStateRepublishConfig](#periodicstaterepublishconfig)). |

### `PeriodicStateRepublishConfig`

In addition to publishing on every state change, NICo can re-publish current
`ManagedHostState` on a timer. Republished messages use the same
`{topic_prefix}/{machineId}/state` topic and JSON payload as change-driven
events, so consumers handle them identically.

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `true` | Enable periodic republishing (on by default whenever the DSX Exchange Event Bus is enabled). Change-driven publishing is unaffected by this setting. |
| `interval` | `Duration` | `5m` | How often a republish sweep runs, clamped to 1 second through 1 hour. |
| `scope` | `RepublishScope` | `all` | Which managed hosts to publish each sweep (see [RepublishScope](#republishscope)). |
| `healthy_republish_every` | `u32` | `1` | When `scope = all`, publish healthy hosts only every Nth sweep; hosts with an active health alert are always published every sweep. `0` is treated as `1`. Ignored when `scope = unhealthy_only`. |
| `max_publishes_per_second` | `u32` | `0` | Upper bound on publishes per second within a sweep, to avoid bursting the broker on large sites. `0` disables pacing. |

### `RepublishScope`

| Value | Description |
|-------|-------------|
| `all` | Republish every managed host each sweep (healthy hosts may be published less often via `healthy_republish_every`). |
| `unhealthy_only` | Republish only managed hosts that currently have a health alert. |

### `DpfConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enable DPF Kubernetes deployment. |
| `deployment_scoped_service_interfaces` | `bool` | `false` | Opt the complete DPF namespace into deployment-scoped `-bf3`, `-bf4`, and `-astra` DPUServiceInterfaces. Each resource selects Nodes in the remote DPU cluster through DPF's propagated `svc.dpu.nvidia.com/owned-by-dpudeployment=<namespace>_<deployment_name>` ownership label; management-cluster DPUNode deployment labels are not used for this selector. Enabling or disabling is a planned migration: stop NICo, remove old-mode NICo ServiceInterfaces in both transition directions, perform DPU re-ingestion, and restart. NICo neither detects nor deletes old-mode resources; skipping cleanup can leave competing interface generations active. Astra requires this setting. |
| `pf_total_sf_reserved` | `u32` | `30` | SF capacity reserved beyond the NICo-managed HBN, DHCP, and FMDS endpoints when an intercept-bridging inventory is configured. NICo sets `PF_TOTAL_SF` to the effective inventory's endpoint count plus this value for BF3 and generic BF4. Without configured intercept bridging, this value is the complete `PF_TOTAL_SF`, preserving the legacy default of `30`; BF4 Astra retains its fixed flavor and ignores this setting. Changing this value changes the BF3/generic-BF4 flavor. Every intercept-inventory change requires controlled ServiceInterface cleanup and DPU re-ingestion, even when the serialized flavor and its hash remain unchanged. Operators must select a value compatible with their platform's SF and BAR capacity. With configured intercept bridging, startup rejects configurations whose managed endpoint count plus reserve exceeds `u32::MAX`. |
| `dpu_service_sync_enabled` | `bool` | `true` | Whether NICo rolls a changed DPUService out on its own, by releasing the DPF maintenance hold on hosts whose DPUs already match their DPUDeployment. Selects *who* opens the gate, never whether one exists: DPF is always configured to park a changed DPUService behind a hold, so no service update reaches a DPU unchecked. Setting `false` does not resume unchecked rollout — the held DPUs wait for an operator to release them deliberately. Hosts still awaiting reprovisioning, and hosts carrying a live tenant instance, keep their hold either way. |
| `dpu_agent_bootstrap_ca` | `DpfDpuAgentBootstrapCa` | `legacy_download` | Bootstrap trust for the containerized DPU agent. Supports `legacy_download` and `mounted`, as described in the following examples. |
| `services` | `Box<DpfMandatoryServicesConfig>` | built-in mandatory-service defaults | Helm chart, image, pull-secret, and `extra_helm_values` settings for the six mandatory DPF services. |
| `extra_services` | `Box<DpfExtraServicesConfig>` | built-in extra-service defaults | Site-wide Helm chart, image, pull-secret, and `extra_helm_values` settings for deployment-specific services. BF4 Astra uses Weave DHCP agent, Weave flow controller, and Xplane; BF3 and generic BF4 do not render them. |
| `docker_image_pull_secret` | `Option<String>` | — | Override for the Kubernetes `imagePullSecrets` entry used to pull mandatory-service images (applied to every mandatory service except `dts` and `doca_hbn`, which take a pull secret only from their per-service config). It is the fallback for an Astra extra service that has no per-service pull secret. |
| `proxy` | `Option<DpfProxyDetails>` | — | Proxy configuration for the DPU. When set, containerd on the DPU routes outbound HTTPS traffic through it. |
| `extra_bfcfg_parameters` | `Vec<String>` | `[]` | `bf.cfg` lines appended to each deployment's [DPUFlavor `bfcfgParameters`](https://networking-docs.nvidia.com/dpf/26.4.1/dpuflavor) — for example, a DPU login password (`ubuntu_PASSWORD='$6$...'`). These site-wide lines are appended first, followed by that deployment's own `extra_bfcfg_parameters`. Entries are passed through verbatim; NICo applies no quoting or interpretation. Entries containing `{{` are rejected at startup. Changing this list creates a new DPUFlavor resource and reprovisions the affected deployment's DPUs; the new parameters take effect during DPU re-install. |
| `deployments` | `DpfDeploymentsConfig` | *(default)* | Per-generation DPUDeployment configurations. BF3 is always present with defaults; BF4 variants are opt-in. A deployment can override individual fields of its supported extra services. |

### `DpfDeploymentConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `bfb_url` | `Option<String>` | BF3 bf-bundle URL | BlueField firmware bundle used for BF3 provisioning. Mutually exclusive with `bluefield_software`; BF4 requires `bluefield_software` instead. |
| `bluefield_software` | `Option<BlueFieldSoftwareConfig>` | — | BF4 OS ISO and PSID-to-PLDM firmware source. BF4 requires this field with exactly one `pldm_fw_bundle` entry. |
| `flavor_name` | `String` | `carbide-dpu-flavor` | Base name for the generated BF3/generic-BF4 `DPUFlavor` or Astra `DPUFlavorTemplate`. |
| `deployment_name` | `String` | `nico-deployment-v2` | Name of the generated `DPUDeployment`. |
| `node_label_key` | `String` | `carbide.nvidia.com/controlled.node.v2` | Label key used to select DPU nodes for this deployment. |
| `services` | `Option<Box<DpfMandatoryServicesConfig>>` | inherit `[dpf.services]` | Optional complete per-deployment mandatory-service override. Omitted service entries use built-in defaults rather than top-level values. |
| `extra_services` | `BTreeMap<DpfExtraService, DpfServiceConfigOverride>` | `{}` | Deployment-local overlays for supported extra services. |

Every active DPF deployment must use distinct `deployment_name`, `flavor_name`, and `node_label_key` values. A deployment `node_label_key` must not be `feature.node.kubernetes.io/dpu-enabled`, which marks every DPF-managed node, or `carbide.nvidia.com/host-bmc-ip`, whose per-node contextual value is the host BMC address. These checks use the local configuration and do not query or modify cluster resources.

Each entry under `[dpf.services]` accepts a chart-native `extra_helm_values` table. NICo deep-merges it over generated `DPUServiceTemplate` values. Nested scalars and arrays replace generated values. DPF applies NICo's deployment-specific `DPUServiceConfiguration` values after the template values. Top-level and per-deployment service fields both overlay the service's built-in defaults.

The topology-derived `[dpf.services.doca_hbn.extra_helm_values.resources]` value `nvidia.com/bf_sf` is reserved: NICo restores the effective HBN interface count after applying operator overrides so the SF request, interface assignment, and startup configuration cannot diverge.

```toml
[dpf.services.dpu_agent.extra_helm_values.fmds]
sign_proxy_url = "http://dsx-imds.dpf-operator-system.svc.cluster.local:8080"
```

Omitting `[dpf.dpu_agent_bootstrap_ca]` preserves the historical download URL.
Use the following configuration to retain download mode while overriding the
complete endpoint URL:

```toml
[dpf.dpu_agent_bootstrap_ca]
source = "legacy_download"
# Optional full endpoint URL. Omit to use the agent default.
url = "http://carbide-pxe.forge/api/v0/tls/root_ca"
```

Use the following configuration to project an existing Secret into the DPU
agent init container:

```toml
[dpf.dpu_agent_bootstrap_ca]
source = "mounted"
object_kind = "secret"
name = "nico-bootstrap-ca-v1"
key = "ca.crt"
```

Use the following configuration for a ConfigMap that already exists in every
target DPU cluster:

```toml
[dpf.dpu_agent_bootstrap_ca]
source = "mounted"
object_kind = "config_map"
name = "nico-bootstrap-ca-v1"
key = "ca.crt"
```

The URL override changes routing, not the initial trust model. An HTTPS URL is
authenticated only when its server certificate chains to a root already
trusted by the shared dpu-agent image.

Mounted mode never falls back to the legacy download. The shared published
dpu-agent image does not embed a site-specific trust anchor. A mounted
ConfigMap must already exist in the DPU cluster. A suitably labeled Secret can
be propagated there by DPF.

### `RmsConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `api_url` | `Option<String>` | — | RMS API URL for rack-level firmware upgrades and power sequencing. |
| `root_ca_path` | `Option<String>` | — | Path to the root CA certificate for TLS verification. |
| `client_cert` | `Option<String>` | — | Path to the client certificate PEM for mTLS. |
| `client_key` | `Option<String>` | — | Path to the client private key PEM for mTLS. |
| `enforce_tls` | `bool` | `true` | Enforce TLS when connecting to RMS. |
| `scale_up_fabric_manager_api_version` | `ScaleUpFabricManagerApiVersion` | `v2` | **Deprecated.** Accepted and ignored. Accepts `v1` or `v2`; any other value fails config load. ScaleUpFabricManager configuration submits an asynchronous job and polls it to completion. |

### `SpdmConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable SPDM hardware attestation. |
| `nras_config` | `Option<nras::Config>` | — | NRAS configuration for secure boot verification. |

### `MachineIdentityConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Master switch for machine identity APIs (opt-in; set `true` with `current_encryption_key_id` and credentials). |
| `algorithm` | `String` | `"ES256"` | Signing algorithm for per-org keys. |
| `token_ttl_min_sec` | `u32` | `60` | Minimum token TTL in seconds. |
| `token_ttl_max_sec` | `u32` | `86400` | Maximum token TTL in seconds. |
| `token_endpoint_http_proxy` | `Option<String>` | — | HTTP proxy for token endpoint calls (SSRF mitigation). |
| `current_encryption_key_id` | `Option<String>` | — | Key-id for encrypting new tenant identity ciphertext (selects from the `machine_identity.encryption_keys` secrets). |
| `trust_domain_allowlist` | `Vec<String>` | `[]` | Trust domains allowed for tenant JWT `iss` (normalized host). Empty allows any. Patterns: exact hostname, `*.suffix` (one label under suffix), `**.suffix` (suffix or any subdomain). |
| `token_endpoint_domain_allowlist` | `Vec<String>` | `[]` | Allowed DNS names for the `token_endpoint` URL host (`http://` / `https://` only). Empty allows any; same pattern syntax as `trust_domain_allowlist`. |
| `signing_key_overlap_max_sec` | `u32` | `604800` | Upper bound for `signing_key_overlap_sec` on `SetTenantIdentityConfiguration` when `rotate_key` is true (seconds). |

### `MeasuredBootMetricsCollectorConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enable measured boot metrics export. |
| `run_interval` | `Duration` | `60s` | Polling interval for boot measurement data. |

### `MachineValidationConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enable machine validation tests. |
| `test_selection_mode` | `MachineValidationTestSelectionMode` | `Default` | `Default`, `EnableAll`, or `DisableAll`. |
| `run_interval` | `Duration` | `60s` | Validation check interval. |
| `stale_run_timeout` | `Duration` | `24h` | Grace period before an active validation run is considered stale. Values below `90s` are raised to `90s` to avoid marking healthy heartbeat-based runs stale. |
| `tests` | `Vec<MachineValidationTestConfig>` | `[]` | Per-test enable/disable overrides. |
| `approved_plugin_registries` | `Vec<String>` | `[]` | Registries allowed for Machine Validation plugin images. Empty denies plugin registration; legacy tests are unaffected. |
| `allow_privileged_plugins` | `bool` | `false` | Allows registration of plugins that request the privileged container profile. |
| `allow_full_host_plugins` | `bool` | `false` | Allows registration of privileged plugins that request a writable host-root mount. Each revision still needs separate approval before it can be enabled. |

### `BomValidationConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Enable BOM/SKU validation. |
| `ignore_unassigned_machines` | `bool` | `false` | Let machines without a SKU bypass validation. |
| `allow_allocation_on_validation_failure` | `bool` | `false` | Keep machines with assigned SKUs allocatable on validation failure; does not bypass unassigned machines. |
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
| ------- | ------ | --------- | ------------- |
| `token_url` | `String` | **required** | OAuth2 token endpoint URL. |
| `scopes` | `Vec<String>` | `[]` | OAuth2 scopes to request. |
| `http_timeout` | `Duration` | `30s` | Token endpoint HTTP timeout. |
| `username` | `String` | `"oauth2token"` | Username in MQTT CONNECT packet. |

### `TracingConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `enabled` | `bool` | `false` | Whether to enable OTLP tracing. |
| `allow_runtime_changes` | `bool` | `true` | Whether tracing may be enabled/disabled at runtime (`nico-admin-cli set tracing-enabled`). |
| `otlp_endpoint` | `Option<String>` | — | The endpoint traces are sent to. The `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` env var takes precedence when set. |

### `LogHistoryConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_megabytes` | `usize` | `128` | Maximum amount of recent log history retained in memory, in MiB. Oldest lines are evicted once the budget is exceeded. |
| `page_size` | `usize` | `500` | Number of lines sent in the initial view and in each scrollback page of the live log viewer. |

### `CredentialsConfig`

The optional `[credentials]` section configures non-secret locations from which
NICo reads operator-managed credentials. Non-UFM credentials continue to read
the local environment and file sources before the configured persistent
backends. `ufm_source` controls the read precedence and mutation policy for UFM
credentials.

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `ufm_source` | `UfmCredentialSource` | `local_first` | UFM credential policy. `local_first` reads environment/file entries before falling back to the persistent backend and writes to the backend. `backend` ignores local UFM entries. `local` makes environment/file entries authoritative and rejects persistent-backend UFM mutations. |
| `file` | `Option<CredentialFileSourceConfig>` | — | Watched JSON or YAML static-credential file (see [CredentialFileSourceConfig](#credentialfilesourceconfig)). When present, it replaces the legacy file source selected by `CARBIDE_CREDENTIALS_FILE_*`; the environment source remains first when enabled. |

When `ufm_source = "local"` and InfiniBand management is enabled, startup
requires a local `ufm_auth_by_fabric` entry for every configured fabric. The
mode is all-or-nothing: NICo does not fall back to Vault or Postgres for a
missing fabric. When `ufm_source` is omitted, `local_first` preserves the
pre-existing local-override behavior.

#### Environment credential source

The environment source is disabled by default. Set
`CARBIDE_CREDENTIALS_ENV_ENABLED=true` on the `nico-api` process to enable it;
the only accepted boolean values are `true` and `false`. The optional
`CARBIDE_CREDENTIALS_ENV_PREFIX` overrides the default
`CARBIDE_STATIC_CREDENTIAL_` prefix. Trailing underscores are removed from the
configured prefix, then `__` separates each nested field.

For example, these variables provide both fields required for the `default`
UFM fabric using the default prefix:

```bash
export CARBIDE_CREDENTIALS_ENV_ENABLED=true
export CARBIDE_STATIC_CREDENTIAL__UFM_AUTH_BY_FABRIC__DEFAULT__USERNAME=ignored-for-token-or-certificate-auth
export CARBIDE_STATIC_CREDENTIAL__UFM_AUTH_BY_FABRIC__DEFAULT__PASSWORD=bearer-token-or-empty
```

Environment credentials are snapshotted at process startup. Changing them
requires restarting `nico-api`; use the watched file source for runtime
credential rotation. With `ufm_source = "local_first"`, an environment entry
overrides the corresponding file and persistent-backend entries. With
`ufm_source = "local"`, every configured fabric must be present in the enabled
environment/file sources.

### `CredentialFileSourceConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `path` | `PathBuf` | **required** | Absolute or working-directory-relative path to a JSON or YAML credential file. The file must exist and parse at startup. |
| `poll_interval` | `Duration` | `60s` | Nonzero interval used in addition to filesystem events to detect projected-Secret replacements. Startup rejects zero. |

The watcher keeps the last valid snapshot when a reload fails. A UFM-only YAML
file has the following shape; both `username` and `password` are required. The
password is the bearer token, while an empty password selects the default
SPIFFE client certificate:

```yaml
ufm_auth_by_fabric:
  default:
    username: ignored-for-token-or-certificate-auth
    password: bearer-token-or-empty
```

### `SecretsConfig`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `kms` | `KmsConfig` | **required** | KMS backend configuration (see [KmsConfig](#kmsconfig)). |
| `routing` | `HashMap<String, String>` | **required** | Maps path prefixes to the `kek_id` that encrypts new writes under them, longest prefix winning. A `/` catch-all entry is required. Reads never consult routing — every stored row records the KEK that wrote it. |
| `backends` | `Vec<CredentialBackend>` | `[vault]` | The persistent-backend read order, highest priority first (first match wins). Enabled local overrides are tried first for non-UFM credentials. UFM reads use these backends directly in `backend` mode and as fallback in `local_first` mode. |
| `writer` | `CredentialBackend` | `vault` | Where new credential writes go. Set to `postgres` to send new writes to the journal; independent of `backends`. UFM mutations are rejected when `credentials.ufm_source = "local"`. |
| `import_from` | `Option<ImportSource>` | — | A source backend to import secrets from at startup. Only `vault` is supported. When `credentials.ufm_source = "local"`, the import does not traverse or read `ufm/`; an import containing only excluded UFM entries still records completion. Unset means a fresh site with nothing to import. |
| `import_approach` | `ImportApproach` | `missing_only` | How to treat secrets that already exist in Postgres during import. |

### `KmsConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `active` | `String` | **required** | The provider that wraps DEKs for new writes. |
| `providers` | `HashMap<String, ProviderConfig>` | **required** | Named provider configurations (see [ProviderConfig](#providerconfig)). |

### `ProviderConfig`

Each entry in `providers` is tagged by `type`. Unknown fields are rejected.

| Field | Type | Default | Description |
| ----- | ---- | ------- | ----------- |
| `type` | `"integrated"` or `"transit"` | **required** | `integrated` holds key material in the NICo process; `transit` wraps and unwraps DEKs in Vault/OpenBao Transit so KEK material never leaves the KMS. |
| `keys` (`integrated`) | `HashMap<String, KeySource>` | **required, non-empty** | Maps each `kek_id` to where its base64-encoded 256-bit key loads from: `{ env = "NAME" }`, `{ file = "/path" }`, or `{ value = "<base64>" }`. Each key must decode to exactly 32 bytes; a missing variable or unreadable file fails the boot, and a key file readable by group or others logs a warning. Inline `value` is development/test-only because the config is logged at startup and shown on the admin web page. |
| `keys` (`transit`) | `Vec<String>` | **required** | Transit key names this provider answers for. |
| `transit_mount` (`transit`) | `Option<String>` | `"transit"` | Secrets-engine mount path. Transit requires a static token in `VAULT_TOKEN`; the Kubernetes service-account login flow is not supported for Transit. |

### `CertificatesConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `backend` | `CertBackendKind` | `shared_vault` | Which backend issues certificates: `shared_vault` reuses the credential store's Vault client (one client, one token lease), `dedicated_vault` uses a separately-configured Vault. |
| `dedicated_vault` | `Option<DedicatedVaultSettings>` | — | Connection settings for a dedicated certificate Vault (see [DedicatedVaultSettings](#dedicatedvaultsettings)). Required when `backend = "dedicated_vault"`, ignored otherwise. |

### `DedicatedVaultSettings`

| Field | Type | Default | Description |
| ------- | ------ | --------- | ------------- |
| `address` | `String` | **required, non-empty** | Vault address, e.g. `https://vault-certs.example:8200`. |
| `pki_mount_location` | `String` | **required, non-empty** | PKI secrets-engine mount path on the target Vault. |
| `pki_role_name` | `String` | **required, non-empty** | PKI role used to sign leaf certificates. |
| `token` | `Option<String>` | — | Token for root-token auth; required only when the pod has no Kubernetes service-account token. |
| `vault_cacert` | `Option<String>` | — | CA bundle that signs the target Vault's TLS cert. Defaults to the site root / `VAULT_CACERT`. |

### `RackValidationConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `false` | Enables rack validation testing. |
| `run_interval` | `Duration` | `60s` | Interval between rack validation controller runs. |
