# Configurability

This page is the landing point for every knob that affects how NICo runs at a
site. It does not replace the in-repo reference documentation — it points to
where the canonical definitions live, names which layer each knob belongs to,
and calls out the most common decisions operators make at deploy time.

If you are doing a first-time deploy, start with the
[`helm-prereqs` README](../../../helm-prereqs/README.md#pre-setup-checklist)
checklist and come back here for individual knobs.

## Contents

- [Mental model — four layers of configuration](#mental-model--four-layers-of-configuration)
- [Layer 1 — Helm values](#layer-1--helm-values)
- [Layer 2 — `nico-api` siteConfig TOML](#layer-2--nico-api-siteconfig-toml)
- [Layer 3 — Environment variables and secrets](#layer-3--environment-variables-and-secrets)
- [Layer 4 — External-system configuration](#layer-4--external-system-configuration)
- [DHCP hook parameters](#dhcp-hook-parameters--easy-to-misconfigure)
- [Machine Update Manager](#machine-update-manager)
- [IB Fabric Monitor](#ib-fabric-monitor)
- [NvLink Monitor](#nvlink-monitor)
- [NICo REST stack configuration](#nico-rest-stack-configuration)
- [Operations and lifecycle](#operations-and-lifecycle)
- [Optional components — at a glance](#optional-components--at-a-glance)
- [Naming and migration notes](#naming-and-migration-notes)
- [Glossary](#glossary)
- [Where to go next](#where-to-go-next)

## Mental model — four layers of configuration

NICo's runtime behavior is shaped by four overlapping config sources. When you
change something, you are touching one of these:

| Layer | Where it lives | Owns |
|-------|---------------|------|
| **1. Umbrella + subchart Helm values** | `helm/values.yaml` and `helm/charts/<chart>/values.yaml`, overridden by `helm-prereqs/values/nico-core.yaml` | Which subcharts are enabled, image references, replica counts, per-service LoadBalancer VIPs, ServiceMonitor toggles, certificate parameters. |
| **2. `nico-api` siteConfig (TOML)** | `nico-api.siteConfig.nicoApiSiteConfig` block inside `helm-prereqs/values/nico-core.yaml`. Mounted into the `nico-api` pod as `nico-api-config.toml`. | All site-policy decisions: identity, pools, networks, tenant isolation, IB, NvLink, firmware updates, host health thresholds, state-controller timing, optional features (DSX, DPA, FNN, SPDM, attestation, ...). |
| **3. Environment variables + secrets** | Container env in chart templates, Kubernetes Secrets, ConfigMaps. Injected by `setup.sh` (`NICO_IMAGE_REGISTRY`, `NICO_CORE_IMAGE_TAG`, `REGISTRY_PULL_SECRET`, ...) or ESO-synced from Vault. | Image registry/tag, registry pull credentials, database credentials, Vault tokens, SPIFFE trust domain overrides. |
| **4. External-system config** | Vault PKI roles, Postgres-operator `postgresql` CRDs, MetalLB `IPAddressPool` / `BGPPeer` / `BGPAdvertisement` CRDs, MQTT brokers, Loki endpoints. | Anything outside NICo's own charts that NICo depends on. Wired by `helm-prereqs`, owned by the operator. |

The rule of thumb: if you can change it with `helm upgrade`, it is layer 1
or layer 3. If it requires editing the TOML block, it is layer 2. If it
requires touching a CRD or a sibling system, it is layer 4.

---

## Layer 1 — Helm values

### Umbrella chart

`helm/values.yaml` exposes:

- `global.image.repository` / `global.image.tag` / `global.image.pullPolicy` — applied to every subchart that uses the global image.
- `global.imagePullSecrets` — list of Secret names mounted into pods that pull from authenticated registries.
- `global.certificate.duration`, `.renewBefore`, `.privateKey.algorithm`, `.privateKey.size`, `.issuerRef.{kind,name,group}` — cert-manager Certificate spec applied to every SPIFFE cert the chart issues. Default `ClusterIssuer` is `vault-nico-issuer`.
- `global.spiffe.trustDomain` — SPIFFE trust domain stamped into every cert's SAN. Default `nico.local`.
- One `<subchart>.enabled` flag per subchart.

See [`helm/README.md`](../../../helm/README.md#configuration) for the full list.

### Enabled-by-default vs opt-in subcharts

| Subchart | Default | Reason for default |
|----------|---------|--------------------|
| `nico-api` | on | Core API; required. |
| `nico-bmc-proxy` | on | Authenticating Redfish proxy. |
| `nico-dhcp` | on | DHCP for PXE boot — DPUs need it to come up. |
| `nico-dns` | on | Authoritative DNS for managed machines and VPCs. |
| `nico-dsx-exchange-consumer` | off | Optional MQTT event consumer; requires a broker. |
| `nico-flow` | off | Workflow orchestrator; deployed separately when needed. |
| `nico-hardware-health` | on | Hardware health collector. |
| `nico-ntp` | on | chrony NTP servers; DPU pre-ingestion needs synced clocks. |
| `nico-pxe` | on | HTTP PXE boot server. |
| `nico-ssh-console-rs` | on | SSH console proxy to BMCs. |
| `unbound` | off | Recursive DNS for the DPU `.forge` compatibility zone; only needed when external DNS does not serve those records. |

### Per-service tuning knobs (common pattern)

Each subchart's `values.yaml` exposes the same shape, even when the defaults
vary:

```yaml
<subchart>:
  enabled: true                # umbrella-level toggle
  replicas: 1                  # StatefulSets and some Deployments
  image: {}                    # override if not using global.image
  imagePullSecrets: []
  resources: {}                # CPU/memory limits & requests
  podSecurityContext: {}
  securityContext: {}
  nodeSelector: {}
  tolerations: []
  affinity: {}
  externalService:
    enabled: false             # opt-in LoadBalancer Service
    type: LoadBalancer
    externalTrafficPolicy: Local
    annotations: {}            # single-VIP charts (e.g. nico-api)
    perPodAnnotations: []      # StatefulSet charts (nico-dns, nico-ntp)
  certificate:
    enabled: true              # cert-manager Certificate for the service
    extraDnsNames: []          # additional SANs (e.g. carbide-api.forge)
  serviceMonitor:
    enabled: false             # Prometheus ServiceMonitor; requires the operator
```

### Per-pod LoadBalancer VIPs (StatefulSets)

`nico-dns` and `nico-ntp` run as StatefulSets and expose **one LoadBalancer
Service per replica** so each pod gets a stable MetalLB VIP. The shape is:

```yaml
nico-ntp:
  externalService:
    enabled: true
    perPodAnnotations:
      - metallb.universe.tf/loadBalancerIPs: "10.0.0.10"   # pod-0
      - metallb.universe.tf/loadBalancerIPs: "10.0.0.11"   # pod-1
      - metallb.universe.tf/loadBalancerIPs: "10.0.0.12"   # pod-2
```

The list is indexed in pod order; entry `[i]` goes on the LB Service backing
pod `<chart>-<i>`. DPUs sync against these IPs via the kea DHCP hook (see
*Layer 2 — DHCP hook parameters* below).

### Image configuration

Most subcharts inherit `global.image`. Two do not:

- `nico-ssh-console-rs.lokiLogCollector.image.{repository,tag}` — optional OpenTelemetry sidecar that ships SSH console logs to Loki. Sidecar is off by default (`lokiLogCollector.enabled: false`); reference image is `otel/opentelemetry-collector-contrib:0.81.0`.
- `unbound.image.{repository,tag}` and `unbound.exporterImage.{repository,tag}` — must be set explicitly when `unbound.enabled: true`.

---

## Layer 2 — `nico-api` siteConfig TOML

The TOML block under `nico-api.siteConfig.nicoApiSiteConfig` is the single
largest configuration surface in NICo. Every site-policy decision lives here.

**Canonical reference:** [`crates/api-core/src/cfg/README.md`](../../../crates/api-core/src/cfg/README.md)
documents every field, type, default, and intent. Reach for that file when
you need the exact name or behavior of a knob. The summary below names the
sections and which page in this guide owns the deep dive.

### Site identity

`sitename`, `initial_domain_name`, `asn`, `datacenter_asn`,
`vpc_isolation_behavior`, `vpc_peering_policy`,
`max_concurrent_machine_updates`. Set once at install time.

### IP and VNI pools

`[pools.lo-ip]`, `[pools.vlan-id]`, `[pools.vni]`, `[pools.vpc-vni]`.
Each entry has `type` (`ipv4` or `integer`) and `ranges = [{ start, end }]`.
The API allocates from these pools when creating instances, VPCs, etc.

### Networks

`[networks.<name>]` — one block per L3 segment. Fields: `type` (`admin` |
`underlay`), `prefix`, `gateway`, `mtu`, `reserve_first`. The `admin` network
is mandatory and must have a non-empty `prefix` and `gateway` — `nico-api`
crashes at startup if either is missing.

### Tenant traffic policy

`site_fabric_prefixes` (CIDRs allowed for tenant-to-tenant traffic) and
`deny_prefixes` (CIDRs tenant instances must not reach — typically OOB,
management, control-plane). `deny_prefixes` generates iptables DROP rules
and NVUE ACL policies on DPUs.

### DHCP, route servers, and BGP

`dhcp_servers`, `route_servers`, `enable_route_servers`,
`bgp_leaf_session_password`, `common_tenant_host_asn`.

### Optional capability toggles

Each block below is `Option<...>` in the Rust config and is **off** unless
explicitly enabled in the TOML.

| TOML section | Capability | Notes |
|--------------|------------|-------|
| `[ib_config]` | InfiniBand fabric monitor + partition manager | See *IB Fabric Monitor* below. |
| `[nvlink_config]` | NvLink partitioning via NMX-C | See *NvLink Monitor* below. |
| `[dpa_config]` | Cluster Interconnect (east-west Ethernet) | Requires MQTT broker. |
| `[dsx_exchange_event_bus]` | MQTT event bus for managed-host state and BMS metadata | Requires MQTT broker. Pairs with the `nico-dsx-exchange-consumer` subchart. |
| `[fnn]` | L3 VPC overlay networking (VXLAN) | Requires `routing_profiles` and route targets. |
| `[spdm]` | SPDM hardware attestation (NRAS-based secure boot) | |
| `[machine_identity]` | SPIFFE JWT-SVID issuance for machine (host) identity | Per-org JWT signing. See [Day 0 Machine Identity](../../../docs/getting-started/installation-options/day0-machine-identity.md) and [Machine Identity (Day 1)](../../../docs/configuration/machine_identity.md). |
| `[measured_boot_collector]` | TPM-based attestation metrics | |
| `[machine_validation_config]` | Pre-ingestion validation tests | |
| `[component_manager]` | Compute tray, NvLink switch, and power shelf management | RMS backends require rack profile data for node type resolution. |
| `[vmaas_config]` | VM system integration / VM-aware traffic intercept | Requires `public_prefixes`. |
| `[rms]` | Rack Manager Service (mTLS connectivity to external RMS) | |
| `[dpf]` | DPU Platform Framework — Kubernetes DPU workload deployment | Requires the DPF operator deployed in-cluster. |
| `rack_management_enabled` | Standalone infrastructure manager mode (GB200/GB300/VR144) | Top-level boolean, not a sub-section. |

For RMS component-manager backends, NICo resolves the RMS node type from the
rack profile. The rack profile provides two facts:

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
| --- | --- |
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

| Field | Accepted values |
| --- | --- |
| `product_family`, when an RMS-backed operation uses the profile | Exact match: `gb200`, `gb300` |
| `rack_hardware_topology` | `gb200_nvl36r1_c2g4_topology`, `gb200_nvl72r1_c2g4_topology`, `gb300_nvl36r1_c2g4_topology`, `gb300_nvl72r1_c2g4_topology` |
| Compute profile vendor, when `compute_tray_backend = "rms"` | `nvidia`, `lenovo` after normalization |
| Switch profile vendor, when `nv_switch_backend = "rms"` | `nvidia` after normalization |
| Power shelf profile vendor, when `power_shelf_backend = "rms"` | `liteon`, `delta` after normalization |

The separate site-explorer machine-ingestion RMS slot/tray lookup also uses the
rack profile for RMS node type resolution. If that path is enabled for machines
with rack IDs, the profile also needs compute product-family and vendor data even
when `compute_tray_backend` is not `rms`.

### State-controller timing

Each major resource has its own state-controller block with `*_run_interval`
and `failure_retry_time` knobs:

`[machine_state_controller]`, `[network_segment_state_controller]`,
`[ib_partition_state_controller]`, `[dpa_interface_state_controller]`,
`[rack_state_controller]`, `[power_shelf_state_controller]`,
`[switch_state_controller]`, `[spdm_state_controller]`.

Defaults are reasonable; touch these only when you have a specific timing
constraint.

### Host health thresholds

`[host_health]` — `hardware_health_reports = "MonitorOnly"` or `"Enforce"`,
plus thresholds for DPU agent compliance. Operators flip this from
`MonitorOnly` to `Enforce` once their fleet's baseline is clean.

### Site Explorer

`[site_explorer].run_interval` controls how often background hardware
discovery scans run. `[site_explorer].create_machines` toggles whether
discovered hardware is auto-registered as machines (useful to disable in
manual-onboarding environments).

### TLS / transport — `[tls]` and `listen_mode`

```toml
listen           = "[::]:1079"     # gRPC listen socket
metrics_endpoint = "[::]:1080"     # Prometheus /metrics
listen_mode      = "tls"           # plaintext_http1 | plaintext_http2 | tls

[tls]
identity_pemfile_path  = "/var/run/secrets/spiffe.io/tls.crt"
identity_keyfile_path  = "/var/run/secrets/spiffe.io/tls.key"
root_cafile_path       = "/var/run/secrets/spiffe.io/ca.crt"
admin_root_cafile_path = "/etc/nico/site/admin_root_cert_pem"   # operator/admin CA
```

The chart wires SPIFFE-issued certificates into the standard
`/var/run/secrets/spiffe.io/` paths via cert-manager + Vault PKI, so the
defaults work out of the box. Override only when integrating with an
external PKI.

### Authentication / SSO — `[auth]`

`[auth.trust]` defines the SPIFFE trust domain and service-base-path
patterns that the API accepts:

```toml
[auth.trust]
spiffe_trust_domain       = "nico.local"
spiffe_service_base_paths = ["/nico-system/sa/", "/default/sa/"]
spiffe_machine_base_path  = "/nico-system/machine/"
additional_issuer_cns     = []
```

`[auth.web]` configures the operator/UI auth surface. For Keycloak/OIDC,
the most common shape is:

```toml
[auth.web]
auth_type             = "oauth2"
oauth2_auth_endpoint  = "https://keycloak.example.com/realms/nico/protocol/openid-connect/auth"
oauth2_token_endpoint = "https://keycloak.example.com/realms/nico/protocol/openid-connect/token"
oauth2_client_id      = "nico-api"
allowed_access_groups = ["nico-operators", "nico-admins"]
```

The chart supports overriding any `[auth.web]` field via
`nico-api.extraEnv` (see [`helm/README.md` → OAuth2 / SSO Setup](../../../helm/README.md#oauth2--sso-setup)).

`[auth.acls]` defines per-principal HTTP method+path allow/deny rules
(used by `nico-bmc-proxy` and other authenticating proxies). The example
ACL set in
[`helm/charts/nico-bmc-proxy/files/carbide-bmc-proxy.toml`](../../../helm/charts/nico-bmc-proxy/files/carbide-bmc-proxy.toml)
is the reference.

### DPU configuration — `[dpu_config]`

DPU-side firmware, BFB image references, IPMI behavior, and per-vendor
overrides. Key fields:

- `default_dpu_agent_version` — DPU agent version installed during provisioning.
- `bfb_image_path` — BFB (Bluefield boot image) location served to DPUs.
- `dpu_ipmi_tool_impl` (top-level) — `"prod"` for real IPMI; `"fake"` for dev clusters with simulated DPUs.
- `dpu_ipmi_reboot_attempts` (top-level) — retry budget for IPMI reboot ops.

See [`crates/api-core/src/cfg/README.md` → DpuConfig](../../../crates/api-core/src/cfg/README.md#dpuconfig)
for the full field list.

### BIOS profiles — `bios_profiles`, `selected_profile`

`bios_profiles` is a nested map keyed by vendor then model that defines
Redfish BIOS settings the state controller pushes to BMCs. `selected_profile`
picks the default profile type. Operators add a profile per supported
host model when they need consistent BIOS-level config (Secure Boot, SR-IOV,
NUMA settings, etc.) across the fleet.

### NIC firmware — `mlxconfig_profiles` and `supernic_firmware_profiles`

- `mlxconfig_profiles` (TOML key `mlx-config-profiles`) — Named profiles of Mellanox NIC register values. Applied during superNIC firmware flashing.
- `supernic_firmware_profiles` — Nested map keyed by `part_number` then `PSID`, defining firmware images and constraints per superNIC variant.

Touch these only when introducing a new NIC SKU or pinning specific NIC
firmware behavior fleet-wide.

### Network Security Groups — `[network_security_group]`

NSG enforcement applied to instance traffic. Fields cover default
direction policy, maximum rule count, and policy-cache TTLs. See
[`crates/api-core/src/cfg/README.md` → NetworkSecurityGroupConfig](../../../crates/api-core/src/cfg/README.md#networksecuritygroupconfig).

### Power management — `[power_manager_options]`

Timing knobs for the power state machine: how often the power manager
polls BMCs for power state, retry intervals on failed power-on/off,
concurrency caps on power-cycle operations. Touch when scaling to large
racks where the default poll interval saturates BMC management interfaces.

### Auto-repair — `[auto_machine_repair_plugin]`

Configures the auto-repair plugin that handles failed machines (which
faults qualify, retry budgets, exclusion lists, repair workflow targets).
Defaults disable auto-repair; enable per fault class as fleet maturity
grows.

### BOM / SKU validation — `[bom_validation]`

When enabled, the state controller validates ingested machines against
their expected BOM (component list / serial numbers) before declaring
them `Ready`. Useful in environments where mislabeled or partially
configured hardware reaches the cluster.

### Other top-level knobs worth knowing about

These don't fit any sub-section but show up in production tuning:

| Field | Default | When to touch |
|-------|---------|---------------|
| `max_database_connections` | `1000` | Drop when running multiple `nico-api` replicas to avoid saturating Postgres `max_connections`. |
| `max_find_by_ids` | `100` | Increase if scripts paginate batch lookups; raise the API-side limit to match the client. |
| `compute_allocation_enforcement` | `WarnOnly` | Switch to `Enforce` once tenant compute pools are sized correctly — flips over-allocation from a warning to a refusal. |
| `bmc_session_lockout_threshold` | `3` | Number of consecutive 401/403s from a BMC before NICo stops session-token logins for that BMC. Raise on environments with flaky BMC firmware. |
| `min_dpu_functioning_links` | unset | Minimum healthy DPU links for a machine to report `Healthy`. Unset = all links required. |
| `set_http_boot_uri_for_vendors` | `[]` | Vendors for which the state controller pins UEFI HTTP Boot URL on the BMC via Redfish. Empty = rely on DHCP option 67. |
| `x86_pxe_boot_url_override` / `arm_pxe_boot_url_override` | unset | Override the default `nico-pxe` boot URL by architecture. Useful when chaining through an external HTTP boot artifact server. |
| `nvue_enabled` | `true` | When `false`, DPU agents write configs directly instead of going through NVUE. |
| `anycast_site_prefixes` | `[]` | **Deprecated** — use `[fnn.routing_profiles.<name>].allowed_anycast_prefixes` instead. |
| `internet_l3_vni` | `100001` | L3 VNI announced for FNN VPC internet connectivity. Combined with `datacenter_asn` for the route-target. |
| `datacenter_asn` | `11414` | Datacenter ASN used by FNN for DC-specific route targets. |
| `common_tenant_host_asn` | unset | If set, tenants must use this ASN for peering with the DPU. If unset, any ASN is accepted. |
| `site_global_vpc_vni` | unset | Cumulus Linux route-leaking workaround — forces every VRF to share one VNI. Limits each DPU to one VRF. |
| `bgp_leaf_session_password` | unset | When set to `site_wide`, returns one credential to all DPU agents for leaf-facing BGP sessions. Otherwise per-leaf credentials are used. |

### FNN routing profiles and prefix filters

When `[fnn]` is enabled, the per-tenant VPC overlay needs `routing_profiles`
that define which prefixes each tenant is allowed to announce, and
`prefix_filter_policy` entries that select between accept / deny rules per
direction. The nested structure is:

```toml
[fnn]
default_route_target  = "65000:1"
allowed_anycast_prefixes = ["198.51.100.0/24"]

[fnn.routing_profiles.standard]
direction              = "Both"
prefix_filter_policy   = "AllowAll"
max_announced_prefixes = 100

[[fnn.routing_profiles.standard.prefix_filters]]
direction = "Outbound"
action    = "Permit"
prefix    = "10.0.0.0/8"
le        = 24
```

See [`crates/api-core/src/cfg/README.md` → FnnConfig / FnnRoutingProfileConfig / PrefixFilterPolicyEntry](../../../crates/api-core/src/cfg/README.md#fnnconfig).

### VMaaS traffic interception — `[vmaas_config]` sub-blocks

When `[vmaas_config]` is enabled, two nested blocks tune the bridging
between the VM tenant network and the bare-metal underlay:

- `[vmaas_config.traffic_intercept_bridging]` — uplink-side intercept config (TrafficInterceptBridging).
- `[vmaas_config.host_intercept_bridging]` — host-side intercept config (HostInterceptBridging).

The host-side block typically sets VLAN IDs and MAC ranges per intercept
pool; the uplink-side block defines public prefixes and how they are
advertised. Both are documented field-by-field in
[`crates/api-core/src/cfg/README.md` → VmaasConfig](../../../crates/api-core/src/cfg/README.md#vmaasconfig).

### InfiniBand fabric definitions — `[ib_fabrics.<name>]`

`[ib_config]` toggles InfiniBand support fleet-wide; `[ib_fabrics.<name>]`
defines a specific UFM-managed fabric. Currently exactly one fabric is
supported. Required fields: UFM endpoint, credentials (username + password,
or token), MGMT IB subnet, GUID prefix. See
[`crates/api-core/src/cfg/README.md` → IbFabricDefinition](../../../crates/api-core/src/cfg/README.md#ibfabricdefinition).

### Operator dev / debug knobs

These are **off by default** and should stay off in production:

| Field | Default | Effect when enabled |
|-------|---------|--------------------|
| `bypass_rbac` | `false` | Disables RBAC enforcement on every API call. Local dev only. |
| `tpm_required` | `true` | When `false`, machines may register without a TPM. Testing only. |
| `listen_only` | `false` | Runs `nico-api` passively (RPC/web only, no background controllers). Used by debug shells and CI smoke tests. |
| `attestation_enabled` | `false` | Master switch for TPM-based attestation. Adds a `Measuring` state before `Ready`. |

---

## Layer 3 — Environment variables and secrets

`setup.sh` reads environment variables to inject image references and the
registry pull credential at install time. Everything else is supplied via
values files.

| Variable | Purpose | Format |
|----------|---------|--------|
| `NICO_IMAGE_REGISTRY` | Image registry hosting NICo Core images | `host[/path]`, e.g. `my-registry.example.com/infra-controller` |
| `NICO_CORE_IMAGE_TAG` | NICo Core image tag | `vX.Y.Z` or git-derived tag |
| `NICO_REST_IMAGE_TAG` | NICo REST image tag | `vX.Y.Z` |
| `REGISTRY_PULL_SECRET` | Raw registry API key | **Raw key string** (e.g. `nvapi-...`). Not a file path. Not a JSON dockerconfig. |
| `REGISTRY_PULL_USERNAME` | Registry username | Defaults to `$oauthtoken` (correct for `nvcr.io`) |
| `KUBECONFIG` | Cluster kubeconfig | Filesystem path |
| `NICO_SITE_UUID` | Stable UUID for this site | UUIDv4. If unset, `setup.sh` tries to reuse the UUID from a prior install (site-agent ConfigMap). If that fails, it adopts an existing REST site with the same name, or mints a UUID and seeds the site record itself. |
| `PREFLIGHT_CHECK_IMAGE` | Image for per-node preflight checks | Defaults to `busybox:1.36`. Override for air-gapped clusters. |

Inside the cluster, `nico-api` discovers Vault, Postgres, and SPIFFE settings
through Kubernetes Secrets and ConfigMaps wired by `helm-prereqs`:

| Secret / ConfigMap | Type | Provisioned by | Consumed by |
|--------------------|------|----------------|-------------|
| `nico-system.nico.nico-pg-cluster.credentials` | Secret | postgres-operator | `nico-api` (`DATASTORE_*` env) |
| `nico-system-nico-database-config` | ConfigMap | `helm-prereqs` | `nico-api` (`DATASTORE_HOST/PORT/NAME` env) |
| `nico-vault-token` | Secret | Vault unseal job | `nico-api` (`VAULT_TOKEN` env) |
| `nico-vault-approle-tokens` | Secret | Vault unseal job | Long-running token refresher (`VAULT_ROLE_ID`, `VAULT_SECRET_ID`) |
| `vault-cluster-info` | ConfigMap | `helm-prereqs` | `nico-api` (`VAULT_SERVICE`, `VAULT_MOUNT`) |
| `vault-cluster-keys` / `vaultunsealkeys` / `vaultroottoken` | Secret | Vault init job | Vault auto-unseal on pod restart |
| `nico-roots` | Secret | ESO-synced from Vault PKI | Every workload that needs the SPIFFE root |
| `imagepullsecret` / `nvcr-nico-dev` | dockerconfigjson | `setup.sh` (from `REGISTRY_PULL_SECRET`) | Every pod that pulls from authenticated registries |
| `ssh-host-key` | Secret | `helm-prereqs` pre-install Job | `nico-ssh-console-rs` (mounted as `ssh_host_ed25519_key`) |
| `azure-sso-nico-web-client-secret` | Secret | **operator-supplied** (manual, optional) | `nico-api` when `[auth.web].auth_type = "oauth2"` (`CARBIDE_WEB_OAUTH2_CLIENT_SECRET` env) |
| `site-root` | cert-manager Certificate | `helm-prereqs` (self-signed bootstrap) | Vault PKI bootstrap chain |

`setup.sh` provisions each chart-managed entry on first install. The OAuth2
client secret is the only one operators must create manually — see
[`helm/PREREQUISITES.md` → `azure-sso-nico-web-client-secret`](../../../helm/PREREQUISITES.md#azure-sso-nico-web-client-secret-optional----only-if-using-oauth2)
for the `kubectl create secret` recipe.

In steady state, all the auto-provisioned entries are reconciled by
external-secrets-operator and the Vault PKI config job.

---

## Layer 4 — External-system configuration

### Vault PKI

NICo issues every workload certificate through cert-manager backed by
Vault PKI. Configurable surface:

- `helm-prereqs/values.yaml` → `vault.nicoCliClientRole.{enabled,name,organization}` — adds an optional Vault role for short-lived `nico-cli` client certificates.
- `helm/values.yaml` → `global.certificate.issuerRef.name` — change to point at a non-default ClusterIssuer.
- `global.certificate.{duration,renewBefore}` — certificate lifetime and renewal window.
- `global.spiffe.trustDomain` — override when migrating from a legacy `forge.local` trust domain.

The Vault deployment itself uses [`helm-prereqs/operators/values/vault.yaml`](../../../helm-prereqs/operators/values/vault.yaml).

### PostgreSQL

Deployed by the Zalando postgres-operator as a HA cluster named
`nico-pg-cluster`. Configurable surface:

- `helm-prereqs/values.yaml` → `postgresql.instances` (replicas), `postgresql.volumeSize` (PVC size per replica).
- Postgres-operator chart values: [`helm-prereqs/operators/values/postgres-operator.yaml`](../../../helm-prereqs/operators/values/postgres-operator.yaml).
- Connection string is composed by `nico-api` from the
  `nico-system.nico.nico-pg-cluster.credentials` Secret plus the
  `nico-system-nico-database-config` ConfigMap.

For external Postgres deployments, point `nico-api`'s `database_url` at the
external instance and skip the postgres-operator install.

### MetalLB

LoadBalancer VIPs for every externally-exposed service. Configured via
[`helm-prereqs/values/metallb-config.yaml`](../../../helm-prereqs/values/metallb-config.yaml):

- `IPAddressPool` blocks define the available VIP CIDRs (typical pattern: one internal pool for in-cluster services, one external pool for `nico-api`).
- `BGPPeer` blocks define per-node BGP sessions to the TOR switches.
- `BGPAdvertisement` or `L2Advertisement` selects how VIPs are announced.

The README in `helm-prereqs` has a [`Pre-setup checklist`](../../../helm-prereqs/README.md#pre-setup-checklist)
that walks the IP plan step-by-step.

### MQTT (DSX exchange and DPA)

NICo does not deploy an MQTT broker. When you enable `[dsx_exchange_event_bus]`
or `[dpa_config]` in the siteConfig TOML, point them at an existing broker:

```toml
[dsx_exchange_event_bus]
mqtt_endpoint    = "mqtt.example.com"
mqtt_broker_port = 1884
# Optional auth — pick one of:
[dsx_exchange_event_bus.auth.basic]              # MqttAuthConfig (username/password)
username = "nico"
password = "<from-secret>"
# or:
[dsx_exchange_event_bus.auth.oauth2]             # MqttOAuth2Config
token_endpoint = "https://idp.example.com/oauth2/token"
client_id      = "nico-api"
client_secret  = "<from-secret>"
scope          = "mqtt:publish mqtt:subscribe"
```

`[dpa_config]` uses the same `mqtt_endpoint` / `mqtt_broker_port` /
`[*.auth.basic]` / `[*.auth.oauth2]` shape. See
[`crates/api-core/src/cfg/README.md` → MqttAuthConfig / MqttOAuth2Config](../../../crates/api-core/src/cfg/README.md#mqttauthconfig).

### Network ports and firewall

NICo services need both in-cluster and out-of-cluster reachability. The
table below covers the dataplane (DPU/host-facing) surface; in-cluster
gRPC/REST is handled by ClusterIP services and does not need firewall
rules.

| Service | Port | Protocol | Direction | Notes |
|---------|------|----------|-----------|-------|
| `nico-api` | 1079 | TCP (gRPC + REST) | DPU agent → API, operator → API | TLS / SPIFFE mTLS. Exposed via MetalLB external pool. |
| `nico-api` | 1080 | TCP | Prometheus scrape | `/metrics`. |
| `nico-dhcp` | 67 | UDP (DHCP) | Bare-metal hosts ↔ `nico-dhcp` | L2 broadcast — `nico-dhcp` must share a broadcast domain with the provisioning network. |
| `nico-dhcp` | 1089 | TCP | Prometheus scrape | `/metrics`. |
| `nico-dns` | 53 | TCP + UDP | Bare-metal hosts → DNS | Per-pod VIPs (one Service per replica). |
| `nico-ntp` | 123 | UDP | Bare-metal hosts → NTP | Per-pod VIPs (one Service per replica). |
| `nico-pxe` | 8080 | TCP (HTTP) | UEFI HTTP boot → boot artifacts | Hosts boot via UEFI HTTP Boot URL pinned in BMC or served via DHCP option 67. |
| `nico-ssh-console-rs` | 22 | TCP (SSH) | Operators → BMC consoles | Per-BMC console access. |
| `nico-ssh-console-rs` | 9009 | TCP | Prometheus scrape | `/metrics`. |
| `unbound` | 53 | TCP + UDP | DPUs → recursive DNS (when enabled) | Serves the `.forge` compatibility zone for DPU agents. |
| `nico-bmc-proxy` | 1079 | TCP (gRPC) | API → BMC proxy | Authenticating Redfish proxy. |

`nico-pxe` also needs egress to wherever PXE artifacts are stored
(typically the same cluster's nico-api or an external HTTP boot artifact
server). See [`helm/PREREQUISITES.md` → Network Requirements](../../../helm/PREREQUISITES.md#6-network-requirements)
for the layer-2 / broadcast-domain constraints on `nico-dhcp` /
`nico-pxe`.

### Loki (optional SSH-console log shipping)

The `nico-ssh-console-rs` chart ships an optional OpenTelemetry sidecar that
forwards SSH console logs to Loki. It is off by default. Enable with:

```yaml
nico-ssh-console-rs:
  lokiLogCollector:
    enabled: true
    image:
      repository: otel/opentelemetry-collector-contrib
      tag: "0.81.0"
```

The collector config is at
[`helm/charts/nico-ssh-console-rs/files/otelcol-config.yaml`](../../../helm/charts/nico-ssh-console-rs/files/otelcol-config.yaml).
Point it at your Loki endpoint (default expects
`http://loki.loki.svc.cluster.local:3100`).

### External Secrets Operator (ESO)

Optional but recommended. When ESO is installed (`helm-prereqs` handles this
by default), the chart's `ClusterSecretStore` + `ClusterExternalSecret`
auto-sync the SPIFFE root CA from Vault into every namespace that needs it
(`nico-roots` Secret). Without ESO, the same Secret must be created and
maintained manually.

ESO chart values live at
[`helm-prereqs/operators/values/external-secrets.yaml`](../../../helm-prereqs/operators/values/external-secrets.yaml).

---

## DHCP hook parameters — easy to misconfigure

`nico-dhcp` runs Kea DHCP with a hook library that writes IPs into the DHCP
responses sent to DPUs and bare-metal hosts. The defaults are placeholders;
**leaving them at the defaults silently breaks DPU bring-up** (clock
divergence, name resolution failures, PXE timeout). Set them in
`helm-prereqs/values/nico-core.yaml`:

```yaml
nico-dhcp:
  config:
    kea:
      hookParameters:
        nameservers: "<nico-dns VIP>"           # default: 127.0.0.1
        ntpServer: "<nico-ntp-0>,<nico-ntp-1>,<nico-ntp-2>"  # default: 127.0.0.1
        provisioningServer: "<nico-pxe VIP>"    # default: 127.0.0.1
```

These IPs must equal the LoadBalancer VIPs you assigned to `nico-dns`,
`nico-ntp` (one per replica), and `nico-pxe` in the same file.

---

## Machine Update Manager

Firmware update behavior is split between a global block and per-host
overrides.

### Global settings — `[firmware_global]`

| Field | Default | Description |
|-------|---------|-------------|
| `autoupdate` | `false` | Master switch for background firmware updates. |
| `host_enable_autoupdate` | `[]` | Host model names to force-enable, regardless of `autoupdate`. |
| `host_disable_autoupdate` | `[]` | Host model names to force-disable. |
| `run_interval` | `30s` | How often the firmware manager evaluates pending updates. |
| `max_uploads` | `4` | Concurrent firmware uploads in flight. |
| `concurrency_limit` | `16` | Concurrent firmware flashing operations across the fleet. |
| `firmware_directory` | auto-detect (`/opt/nico/firmware` then `/opt/carbide/firmware`) | Where firmware binaries live on the API pod. Override to pin one location explicitly. |
| `host_firmware_upgrade_retry_interval` | `60m` | Backoff between retries on failed host upgrades. |
| `instance_updates_manual_tagging` | `true` | Require explicit instance tagging before instance-attached firmware upgrades fire. |
| `no_reset_retries` | `false` | Disable retry logic after BMC resets during firmware ops. |
| `hgx_bmc_gpu_reboot_delay` | `30s` | Delay after GPU reboot before HGX BMC is contacted again. |
| `requires_manual_upgrade` | `false` | Force every firmware upgrade to require explicit administrator approval. |
| `max_concurrent_bfb_copies` | `10` | Concurrent DPU BFB copies. |

### Update policy — `[machine_updater]`

`[machine_updater]` controls retries, concurrency caps, and exclusion lists
applied to the update queue. See
[`crates/api-core/src/cfg/README.md` → MachineUpdater](../../../crates/api-core/src/cfg/README.md#machineupdater)
for the full field list.

### Per-model firmware — `[host_models.<name>]`

Maps a host model identifier to a Firmware definition (BMC, UEFI, NIC
images plus version constraints). The state controller picks the right
images when a machine in the model joins. See
[`crates/api-core/src/cfg/README.md` → host_models](../../../crates/api-core/src/cfg/README.md#hostmodelsfirmware).

---

## IB Fabric Monitor

InfiniBand fabric monitoring is a top-level capability gated on
`[ib_config]`. When absent, no InfiniBand integration runs.

```toml
[ib_config]
enabled = true
max_partition_per_tenant = 16
allow_insecure = false              # disable in production
mtu = 4096
rate_limit = 100
service_level = 0
fabric_monitor_run_interval = "60s" # how often UFM is polled for fabric topology + partition status
```

The fabric monitor talks to UFM (Unified Fabric Manager) over its REST API
to read partitions, map them to tenants, and enforce the per-tenant cap.
Connection details (UFM host, credentials) live in `[ib_fabrics.<name>]`.

See [`crates/api-core/src/cfg/README.md` → IBFabricConfig](../../../crates/api-core/src/cfg/README.md#ibfabricconfig).

---

## NvLink Monitor

NvLink partitioning via NMX-C is gated on `[nvlink_config]`:

```toml
[nvlink_config]
enabled = true
monitor_run_interval = "60s"
partition_monitor_unreachable_alert_after = 3 # consecutive passes a rack's NMX-C must be unusable before its NmxcUnreachable rack alert is raised; cleared by the first reachable pass
nmxc_endpoint = "https://nmxc.example.com"
# Plus auth + retry settings — see the canonical reference.
```

When enabled, NICo reads NvLink partition state from NMX-C and reconciles
it with the rack-level state machine. See
[`crates/api-core/src/cfg/README.md` → NvLinkConfig](../../../crates/api-core/src/cfg/README.md#nvlinkconfig).

---

## NICo REST stack configuration

The NICo REST stack (separate helm release named `nico-rest`, in the
`nico-rest` namespace) sits on top of NICo Core and provides the public
REST API, workflow orchestration, optional Keycloak IdP, and the
per-site agent. Its source lives in the
[`rest-api/`](https://github.com/NVIDIA/infra-controller/tree/main/rest-api) tree;
this guide covers only the *site-side* configuration knobs.

### nico-rest helm release — `helm-prereqs/values/nico-rest.yaml`

| Key | Default | Purpose |
|-----|---------|---------|
| `nico-rest-api.config.keycloak.enabled` | `true` | Use the bundled dev Keycloak instance. Set `false` for BYO Keycloak / external OIDC. |
| `nico-rest-api.config.keycloak.baseURL` | `http://keycloak.nico-rest:8082` | Internal Keycloak URL used by `nico-rest-api`. |
| `nico-rest-api.config.keycloak.externalBaseURL` | `http://keycloak.nico-rest:8082` | Issuer URL embedded in tokens — must match what clients reach. |
| `nico-rest-cert-manager.*` | — | Per-component TLS cert SANs and durations. |
| `nico-rest-workflow.*` | — | Temporal client config, retention windows, task queue tuning. |
| `nico-rest-site-manager.*` | — | Site-manager service settings (talks to `nico-api` over gRPC/SPIFFE). |

### Site-agent — `helm-prereqs/values/nico-site-agent.yaml`

The site-agent registers the site with `nico-rest-site-manager` and
bridges Temporal workflows down to NICo Core. Operators almost always
override:

| Key | Default | Purpose |
|-----|---------|---------|
| `envConfig.DB_ADDR` | `postgres.postgres.svc.cluster.local` | Postgres host (the REST-side cluster, separate from NICo Core's). |
| `envConfig.DB_DATABASE` | `elektratest` | Database name. |
| `envConfig.DEV_MODE` | `"true"` | **Production must set to `"false"`.** |
| `envConfig.NICO_SEC_OPT` | `"2"` | Security mode: `0` insecure, `1` TLS, `2` mTLS. Production requires `2`. |
| `CLUSTER_ID` | — (set by `setup.sh`) | Site UUID (`NICO_SITE_UUID`). |
| `TEMPORAL_SUBSCRIBE_NAMESPACE` | — (set by `setup.sh`) | Temporal namespace; must match `CLUSTER_ID`. |

### REST-side PostgreSQL

NICo REST runs its own simple StatefulSet Postgres in the `nico-rest`
namespace (not the HA Patroni cluster used by NICo Core). It hosts the
Temporal, Keycloak (when enabled), and site-manager databases. The
StatefulSet is templated by `setup.sh` and configurable via `nico-rest.yaml`.

### Temporal

Temporal is deployed by `setup.sh` Phase 7f using the upstream Temporal
helm chart with mTLS enabled. The mTLS issuer (`nico-rest-ca-issuer`) is
installed in Phase 7b. Operators usually don't touch Temporal config
directly; see the temporal subchart values in
[`rest-api/temporal-helm/temporal/values.yaml`](https://github.com/NVIDIA/infra-controller/tree/main/rest-api/temporal-helm/temporal)
if you need to tune retention or task queue counts.

### Keycloak (dev IdP)

When `keycloak.enabled: true`, `setup.sh` deploys a development Keycloak
in the `nico-rest` namespace with a pre-seeded `nico-dev` realm. This is
suitable for development and integration testing only. For production,
disable the bundled Keycloak, deploy your own IdP (Azure AD, Okta, etc.),
and point `nico-rest-api.config.keycloak.{baseURL,externalBaseURL}` at it.

Token-acquisition helper: `helm-prereqs/keycloak/get-token.sh`. See
[`helm-prereqs/keycloak/README.md`](../../../helm-prereqs/keycloak/README.md)
for the realm/clients/roles deep-dive.

---

## Operations and lifecycle

### `setup.sh` — install

Orchestrates the full install in phases. Skip flags:

| Flag | Effect |
|------|--------|
| `-y` | Non-interactive — accept all prompts. |
| `--skip-core` | Skip the NICo Core install (prereqs + REST only). |
| `--skip-rest` | Skip the entire NICo REST stack (Core only). |
| `--skip-flow` | Skip the NICo Flow phase inside REST. |
| `--core-values <file>` | Use a site-specific values file instead of `helm-prereqs/values/nico-core.yaml`. |
| `--metallb-config <path>` | Use a site-specific MetalLB manifest file or kustomize directory. |
| `--site-overlay <dir>` | Apply a site kustomize overlay after NICo Core deploys (for per-site resources not managed by the chart). |
| `--debug` | Enable bash tracing — may print secrets, avoid in shared logs. |

Required environment is described in *Layer 3* above.

### `preflight.sh` — pre-install validation

Runs automatically before `setup.sh` makes any cluster change. Verifies
node readiness, kubectl connectivity, registry pull credentials, and
required values fields. Override the preflight image for air-gapped
clusters via `PREFLIGHT_CHECK_IMAGE`.

### `health-check.sh` — post-install validation

Read-only audit of the installed stack: component readiness, Vault and
PostgreSQL health, required secrets and certificates, ESO sync status,
LoadBalancer VIP assignment, in-cluster connectivity, and `.forge` DNS
record reachability.

```bash
helm-prereqs/health-check.sh
```

Override namespace detection with `NICO_NS`, `VAULT_NS`, `POSTGRES_NS`,
`CERT_MANAGER_NS`, `ESO_NS`, `METALLB_NS`.

### Upgrading

Day-2 upgrades use plain `helm upgrade` against the merged values:

```bash
helm diff upgrade nico ./helm \
  -n nico-system \
  -f helm-prereqs/values/nico-core.yaml \
  --set global.image.repository=<registry>/nvmetal-carbide \
  --set global.image.tag=<new-tag>

# Review the diff, then:
helm upgrade nico ./helm \
  -n nico-system \
  -f helm-prereqs/values/nico-core.yaml \
  --set global.image.repository=<registry>/nvmetal-carbide \
  --set global.image.tag=<new-tag>
```

`setup.sh` is install-time only — do not re-run it for routine upgrades.
Re-running is safe (idempotent helmfile sync + helm upgrade), but it
also re-applies operator-chart defaults that may not match your
production tuning.

For the REST stack the equivalent is `helm upgrade nico-rest …` against
`rest-api/helm/charts/nico-rest`.

See [`helm/README.md` → Upgrading](../../../helm/README.md#upgrading) for
the diff-then-apply pattern.

### Migration from forged-kustomize

Sites that previously deployed via the forged-kustomize layout
(`carbide-*` resources in `forge-system`) migrate by:

1. Pointing the new helm chart at the same MetalLB VIPs the kustomize
   layout used — DPUs in the field continue to resolve their hardcoded
   `.forge` hostnames to the same IPs.
2. Keeping `global.spiffe.trustDomain: forge.local` (the original
   trust domain) so existing DPU agent certs remain valid.
3. Enabling `unbound.enabled: true` with the existing `.forge` zone
   records so DHCP-distributed DNS keeps working.
4. Once every DPU has been re-imaged with a NICo-trust-domain cert,
   flip `global.spiffe.trustDomain` to `nico.local` and remove the
   compatibility SANs.

See [`helm/README.md` → Migrating from Kustomize](../../../helm/README.md#migrating-from-kustomize)
and [`helm-prereqs/README.md` → DPU compatibility DNS](../../../helm-prereqs/README.md#dpu-compatibility-dns-forge-zone--required-for-dpu-bring-up).

### Values templates and examples

The chart ships two reference values files:

- [`helm/examples/values-minimal.yaml`](../../../helm/examples/values-minimal.yaml) — smallest possible site, useful as a starting point for new deployments or CI smoke tests.
- [`helm/examples/values-full.yaml`](../../../helm/examples/values-full.yaml) — every knob the chart exposes, with comments. Useful as a reference when tracking down "is this configurable?" questions.

The example used by `setup.sh` (and what real sites should copy from) is
[`helm-prereqs/values/nico-core.yaml`](../../../helm-prereqs/values/nico-core.yaml),
which already wires in the prereq-chart conventions.

### `clean.sh` — teardown

```bash
helm-prereqs/clean.sh
```

Removes everything `setup.sh` installs, in reverse dependency order:
NICo REST → NICo Core → helmfile releases → cluster-scoped CRDs and
RBAC → namespaces → reclaimed PersistentVolumes → local-path-provisioner.
**Destructive — drops the Postgres data volumes.** Use only when wiping
the cluster for a clean reinstall.

### Air-gapped deployments

- Set `PREFLIGHT_CHECK_IMAGE` to a locally-mirrored busybox tag (default `busybox:1.36` pulls from Docker Hub).
- Mirror every chart's image into the air-gapped registry; override each chart's `image.repository` to point at the mirror.
- Mirror the helmfile-managed operator charts (cert-manager, vault, postgres-operator, external-secrets, MetalLB, postgres-operator) — set `imagePullSecrets` in `helm-prereqs/operators/values/*.yaml`.
- The `dockurr/chrony` image used by `nico-ntp` is on Docker Hub by default; mirror to your registry and override `nico-ntp.image.repository` for air-gapped sites.

### Values precedence (override patterns)

Helm merges values in this order (later wins):

1. `helm/values.yaml` — chart defaults.
2. Each subchart's own `helm/charts/<chart>/values.yaml` — only when set at the umbrella level via `<chart>.<field>:`.
3. `-f helm-prereqs/values/nico-core.yaml` — site-level overrides passed by `setup.sh`.
4. `--set` flags injected by `setup.sh` (image registry, image tag, image pull secret).
5. `--set` flags passed directly by an operator on a manual `helm upgrade`.

To inspect the merged values for a deployed release:

```bash
helm get values -a nico -n nico-system
helm template nico ./helm -f helm-prereqs/values/nico-core.yaml --set global.image.tag=<tag>
```

### High availability

- **`nico-api`**: stateless Deployment; scale replicas via `nico-api.replicas` (default 1). The state controllers use leader election when more than one replica runs.
- **`nico-dns` / `nico-ntp`**: StatefulSets with per-pod LoadBalancer VIPs (default 3 replicas). Pod anti-affinity is enabled by default — replicas spread across nodes.
- **PostgreSQL (NICo Core)**: Zalando HA Patroni cluster; `helm-prereqs/values.yaml` → `postgresql.instances` (default 3).
- **Vault**: 3-node HA Raft (set in `helm-prereqs/operators/values/vault.yaml`).
- **PostgreSQL (NICo REST)**: single StatefulSet by default — not HA. For production, point `nico-site-agent.envConfig.DB_ADDR` at an external HA Postgres.

### Logging and observability

- **NICo Core log level**: set `RUST_LOG` via `nico-api.extraEnv` (`info`, `debug`, `trace`; default `info`). Same pattern for every Rust workload.
- **Structured logging**: `nico-api` emits JSON to stdout — collect with any cluster logging stack.
- **Prometheus metrics**: every chart exposes `/metrics`; ServiceMonitor CRDs are off by default and gated behind `<chart>.serviceMonitor.enabled`.
- **Metric prefix migration**: existing dashboards reference `carbide_*`. Set `alt_metric_prefix = "nico"` in siteConfig to emit `nico_*` aliases alongside without breaking the legacy names.
- **SSH session logs**: opt-in via `nico-ssh-console-rs.lokiLogCollector.enabled` — see *Layer 4* above.

### Backup and restore

- **NICo Core Postgres**: backed by Zalando postgres-operator's wal-g integration. Configure backup destinations in `helm-prereqs/operators/values/postgres-operator.yaml`. Operator docs are upstream.
- **Vault**: 3-node HA Raft with disk persistence. Snapshots via `vault operator raft snapshot save`. For disaster recovery, the unseal keys are stored as the `vaultunsealkeys` Kubernetes Secret and the root token as `vaultroottoken` — preserve these out-of-band.
- **NICo REST Postgres**: single-replica StatefulSet — back up the underlying PVC or migrate to an external HA Postgres.
- **Vault PKI roots**: cert-manager re-issues child certs automatically, but the Vault PKI mount's root CA must be backed up if you need to restore signing capability.

### Helm hooks and pre-install Jobs

The chart relies on several Jobs that run automatically during install
and upgrade — knowing they exist helps when debugging stuck rollouts:

| Job / Hook | Phase | When it runs | What it does |
|------------|-------|--------------|--------------|
| `gen-site-ca` | helm-prereqs pre-install | Before `nico-prereqs` install | Generates the self-signed site-root certificate that bootstraps Vault TLS. |
| `vault-pki-config` | helm-prereqs post-install | After Vault is unsealed | Configures the Vault PKI secrets engine, creates the `nico-issuer` role, sets up the AppRole auth used by `nico-api`. |
| `ssh-host-key` | helm-prereqs pre-install | Before `nico-ssh-console-rs` install | Generates an Ed25519 SSH host key and writes it to the `ssh-host-key` Secret. |
| `flow-vault-tokens` | helm-prereqs post-install | After `nico-api` install | Issues per-namespace Vault tokens consumed by the flow service when enabled. |
| `nico-api-migrate` | NICo Core pre-upgrade | Before every `nico-api` upgrade | Runs `nico-api migrate` against the Postgres datastore. Failures abort the upgrade. |
| `nico-rest cert-manager` ClusterIssuer apply | Phase 7b | Before nico-rest pods come up | Installs the `nico-rest-ca-issuer` ClusterIssuer for REST-side TLS. |

If `helm upgrade` hangs, check Job logs first — these run as
pre-/post-install hooks and a failure here blocks the chart from
finishing. The migration Job in particular surfaces image-pull failures
quickly because it runs before anything else.

### Schema migrations

`nico-api migrate` runs as a `nico-api-migrate` Job before every `helm
upgrade`. It applies any pending schema migrations to the Postgres
datastore. Migration files are versioned in
`crates/api-core/migrations/`. The Job:

- Is idempotent — re-running with no pending migrations exits cleanly.
- Blocks the upgrade until completion. If it fails, **the upgrade is rolled back automatically**.
- Uses the same image as `nico-api`, so image registry / pull secret problems surface here first.

Always read `nico-api-migrate-<hash>` Job logs when an upgrade fails
during the pre-upgrade hook phase.

### Service accounts and RBAC

Every subchart creates a dedicated ServiceAccount in the release
namespace; the SA name matches the chart name (`nico-api`, `nico-dhcp`,
`nico-dns`, ...). Cluster-scoped resources (the `vault-pki-config-reader`
ClusterRole, the External-Secrets `ClusterSecretStore` reader) are
owned by `nico-prereqs`.

Workload identity inside the cluster runs over SPIFFE, not Kubernetes SA
tokens — pods bind to a SPIFFE SVID issued by Vault PKI via cert-manager.
The Kubernetes SA is used only for pod scheduling and access to
`kube-api` resources the chart needs (e.g. reading Secrets / ConfigMaps).

### Pod Disruption Budgets

The chart **does not** ship PDBs by default. For multi-replica StatefulSets
(`nico-dns`, `nico-ntp`, the Postgres cluster) operators that run on
clusters with frequent node-drain operations should add their own:

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: nico-dns
  namespace: nico-system
spec:
  minAvailable: 2
  selector:
    matchLabels:
      app.kubernetes.io/name: nico-dns
```

### Resource limits and capacity sizing

The chart's per-chart `resources: {}` defaults are intentionally empty so
operators can set them per environment. Rule-of-thumb starting points
for a 100-host site:

| Component | CPU request / limit | Memory request / limit | Notes |
|-----------|---------------------|------------------------|-------|
| `nico-api` | `1 / 4` | `2Gi / 8Gi` | State controllers + gRPC; scale up linearly with fleet size. |
| `nico-bmc-proxy` | `200m / 1` | `256Mi / 1Gi` | |
| `nico-dhcp` | `200m / 500m` | `256Mi / 512Mi` | Kea is single-threaded; CPU is not the bottleneck. |
| `nico-dns` | `200m / 500m` (per replica) | `256Mi / 512Mi` | |
| `nico-ntp` | `100m / 200m` (per replica) | `64Mi / 128Mi` | chrony is light. |
| `nico-pxe` | `200m / 500m` | `512Mi / 1Gi` | Boot artifact serving. |
| `nico-ssh-console-rs` | `200m / 500m` | `256Mi / 512Mi` | |
| `nico-hardware-health` | `500m / 2` | `1Gi / 4Gi` | Scales with sensor poll rate. |
| Postgres (per replica) | `1 / 4` | `4Gi / 16Gi` | Zalando default tuning. |
| Vault (per replica) | `500m / 2` | `512Mi / 2Gi` | |

PVC sizing:

- `helm-prereqs/values.yaml` → `postgresql.volumeSize` (default `10Gi`) — sized for ~500 machines + a year of state-machine event history.
- Vault raft storage uses the chart default (10Gi) — bump for high-cardinality PKI deployments.

### Telemetry destinations beyond Prometheus

The chart exposes a standard Prometheus `/metrics` endpoint on every
service. To ship metrics elsewhere:

- **Prometheus + Mimir / Cortex / Thanos**: configure remote_write in your Prometheus instance; no chart-side change.
- **OpenTelemetry Collector**: deploy an OTel collector configured with the Prometheus receiver scraping NICo services and any exporter (OTLP, Mimir, Datadog, etc.).
- **Tempo / Jaeger** (traces): NICo emits structured logs but does not currently emit distributed traces; tracing is a roadmap item.
- **Loki** (logs): the SSH-console sidecar pattern (`nico-ssh-console-rs.lokiLogCollector.enabled`) is the only built-in log-shipper. For other services, use a cluster-wide Promtail / Vector / OTel-collector DaemonSet against pod stdout.

### Profiler endpoint

`nico-api` listens on port `1081` for pprof-style runtime profiling. The
endpoint is disabled by default in production builds; enable via
`nico-api.extraEnv` (`RUST_PROFILER=1`) and protect with a NetworkPolicy
before exposing.

### nico-cli configuration

The `nico-cli` (operator CLI) reads its config from
`~/.config/nico/cli-config.json` (or `--config <path>`). It needs the
nico-api URL, the SPIFFE root CA, and a JWT obtained from your IdP.

```json
{
  "nico_api": "https://api-<site>.example.com:1079",
  "root_ca_path": "/path/to/nico-roots.pem",
  "token_path": "/path/to/jwt"
}
```

For Keycloak-bundled deployments, `helm-prereqs/keycloak/get-token.sh`
obtains a fresh JWT.

---

## Optional components — at a glance

A consolidated quick-reference table for every component that can be turned
on or off.

| Component | Layer | Knob | Default | When to enable |
|-----------|-------|------|---------|----------------|
| `nico-ntp` | Helm | `nico-ntp.enabled` | on | Leave on unless upstream NTP is reachable from the provisioning network. |
| `nico-dsx-exchange-consumer` | Helm | `nico-dsx-exchange-consumer.enabled` | off | Enable when the site has an MQTT broker and you want BMS metadata + managed-host events. |
| `nico-flow` | Helm | `nico-flow.enabled` | off | Workflow orchestrator; enable when running Temporal-backed workflows. |
| `unbound` | Helm | `unbound.enabled` | off | Enable when DPUs need the `.forge` compatibility zone and no external DNS serves it. |
| SSH-console Loki sidecar | Helm | `nico-ssh-console-rs.lokiLogCollector.enabled` | off | Enable when shipping SSH session logs to Loki. |
| ServiceMonitor (per chart) | Helm | `<chart>.serviceMonitor.enabled` | off | Enable when the Prometheus Operator is installed. |
| Hardware-health telemetry | Helm | `nico-hardware-health.telemetryServiceMonitor.enabled` | off | Enable for per-machine sensor metrics (temperature, power, fans). |
| IB Fabric Monitor | siteConfig | `[ib_config].enabled` | off | Sites running InfiniBand fabrics managed by UFM. |
| NvLink Monitor | siteConfig | `[nvlink_config].enabled` | off | GB200/GB300 sites using NMX-C for NvLink partitioning. |
| DSX Exchange Event Bus | siteConfig | `[dsx_exchange_event_bus]` present | off | Pairs with the `nico-dsx-exchange-consumer` chart. Requires MQTT broker. |
| DPA (Cluster Interconnect) | siteConfig | `[dpa_config].enabled` | off | East-west Ethernet cluster networking; requires MQTT broker. |
| FNN (L3 VPC overlay) | siteConfig | `[fnn]` present | off | Tenant VPC networking via VXLAN; needs `routing_profiles` and route targets. |
| Attestation | siteConfig | `attestation_enabled = true` | off | TPM-based machine attestation; adds a `Measuring` state before `Ready`. |
| Measured Boot Metrics | siteConfig | `[measured_boot_collector].enabled` | off | Exporter for TPM-based attestation metrics. |
| Machine Identity (SPIFFE JWT-SVID) | siteConfig | `[machine_identity].enabled` | off | Per-org JWT signing for machine identity tokens. See [Day 0](../../../docs/getting-started/installation-options/day0-machine-identity.md) and [Day 1](../../../docs/configuration/machine_identity.md) docs. |
| Machine Validation | siteConfig | `[machine_validation_config].enabled` | off | Pre-ingestion validation tests. |
| SPDM | siteConfig | `[spdm].enabled` | off | Hardware attestation via NRAS. |
| Rack Management | siteConfig | `rack_management_enabled = true` | off | Standalone infrastructure manager mode (GB200/GB300/VR144). |
| Site Explorer machine auto-creation | siteConfig | `[site_explorer].create_machines` | on | Disable for manual-onboarding environments. |
| Firmware autoupdate | siteConfig | `[firmware_global].autoupdate` | off | Enable once the fleet's firmware baseline is stable. |
| Component Manager (compute trays / NvLink switches / power shelves) | siteConfig | `[component_manager]` present | off | GB200/GB300 sites with managed compute, power, and switch fabric. RMS backends require rack profile data for node type resolution. |
| Auto-repair plugin | siteConfig | `[auto_machine_repair_plugin]` | off | Enable per fault class as fleet maturity grows. |
| BOM / SKU validation | siteConfig | `[bom_validation]` present | off | Validate ingested hardware against expected BOM before `Ready`. |
| Network Security Groups | siteConfig | `[network_security_group]` | default | Touch only for non-default direction policy or scale-out limits. |
| RBAC bypass (dev only) | siteConfig | `bypass_rbac = true` | off | Disables RBAC; never set in production. |
| Passive mode (debug only) | siteConfig | `listen_only = true` | off | RPC/web only, no background controllers. CI/dev shells only. |
| TPM bypass (testing only) | siteConfig | `tpm_required = false` | required | Allows machine registration without TPM. Testing only. |
| DPF (Kubernetes DPU workloads) | siteConfig | `[dpf].enabled` | off | Requires the DPF operator. |
| Loki sidecar (REST stack) | Helm (REST) | `nico-rest-*` log shipping | off | Optional; pairs with the same OTel collector pattern used by Core. |
| Bundled dev Keycloak | Helm (REST) | `nico-rest-api.config.keycloak.enabled` | on | Disable for production — use external IdP. |

---

## Naming and migration notes

Some legacy hostnames and field names remain in the chart for backward
compatibility with deployed DPU agents:

- DPU agents in the field resolve hardcoded `.forge` hostnames (`carbide-api.forge`, `carbide-pxe.forge`, `carbide-ntp.forge`, `carbide-static-pxe.forge`, `unbound.forge`). The chart serves these as cert SANs and (optionally) via `unbound`. See [`helm-prereqs/README.md` → DPU compatibility DNS](../../../helm-prereqs/README.md#dpu-compatibility-dns-forge-zone--required-for-dpu-bring-up).
- Sites migrating from a `forge.local` SPIFFE trust domain can keep the old domain by setting `global.spiffe.trustDomain: forge.local` in values.
- Prometheus metric names use the `carbide_*` prefix for historical reasons. `alt_metric_prefix` in the siteConfig adds a `nico_*` alias alongside the legacy names without removing them, for dashboard migration.

These are intentional compatibility surfaces, not bugs.

---

## Glossary

| Term | Meaning |
|------|---------|
| **BFB** | Bluefield Boot — DPU boot image (ARM-side). Stored on `nico-pxe` and chain-loaded onto Bluefield DPUs. |
| **BMC** | Baseboard Management Controller — out-of-band server management chip. Talked to over Redfish via `nico-bmc-proxy`. |
| **BMS** | Bare-Metal Server. The thing being provisioned. |
| **BOM** | Bill of Materials. Component / serial-number list validated by `[bom_validation]`. |
| **DPA** | DPU Platform Agent / Cluster Interconnect. East-west Ethernet fabric between DPUs. Configured via `[dpa_config]`. |
| **DPF** | DPU Platform Framework. Kubernetes operator for running tenant workloads on DPUs. Configured via `[dpf]`. |
| **DPU** | Data Processing Unit. Bluefield-2 / Bluefield-3 cards in managed hosts. Each runs `nico-dpu-agent`. |
| **DSX** | Datacenter Sensor eXchange. MQTT-based event bus for BMS metadata and managed-host state. Configured via `[dsx_exchange_event_bus]`. |
| **ESO** | External Secrets Operator. Syncs secrets from Vault into Kubernetes Secrets. |
| **FMDS** | Fleet Management Data Service. Tenant-facing API surfaced by `nico-api` (site identity is exposed here via `sitename`). |
| **FNN** | Fabric Network Numbering / L3 VPC overlay. Tenant VPC networking via VXLAN. Configured via `[fnn]`. |
| **FRR** | Free Range Routing. Linux routing daemon used inside DPUs for BGP. ASN configured via the top-level `asn` field. |
| **HGX** | NVIDIA HGX baseboard (H100/H200/GB200 GPU systems). Has its own BMC reachable via `nico-bmc-proxy`. |
| **IPMI** | Intelligent Platform Management Interface. Legacy OOB management protocol; `dpu_ipmi_tool_impl` selects prod vs fake implementations. |
| **kea** | The Kea DHCP server from ISC. Runs inside the `nico-dhcp` pod with a custom NICo hook library. |
| **MetalLB** | LoadBalancer provider for bare-metal Kubernetes. Allocates the VIPs every external NICo service uses. |
| **mTLS** | Mutual TLS. The standard transport security mode for NICo (`listen_mode = "tls"` + SPIFFE certs both ways). |
| **NMX-C** | NVLink Management eXchange — Compute. NvLink partitioning controller. Talked to via `[nvlink_config]`. |
| **NRAS** | NVIDIA Remote Attestation Service. External SPDM verification endpoint. |
| **NSG** | Network Security Group. Per-tenant firewall rules. Configured via `[network_security_group]`. |
| **PXE** | Preboot eXecution Environment. Network boot flow served by `nico-pxe` (HTTP-based, UEFI HTTP Boot). |
| **Redfish** | Standard REST API for server management. Proxied by `nico-bmc-proxy`. |
| **RMS** | Rack Manager Service. External rack-level controller for GB200/GB300/VR144 sites. Configured via `[rms]`. |
| **SPDM** | Security Protocol and Data Model. Hardware attestation protocol. Configured via `[spdm]`. |
| **SPIFFE** | Secure Production Identity Framework For Everyone. The workload identity standard used by every NICo service. Trust domain is set via `global.spiffe.trustDomain`. |
| **SVID** | SPIFFE Verifiable Identity Document. Either X.509 (default) or JWT (used for tenant DPU identity via `[machine_identity]`). |
| **TPM** | Trusted Platform Module. Required for `attestation_enabled`; can be bypassed for testing via `tpm_required = false`. |
| **UEFI HTTP Boot** | UEFI firmware feature that fetches the bootloader over HTTP. Used by `nico-pxe`. URL pinned via DHCP option 67 or `HttpBootUri` Redfish setting. |
| **UFM** | Unified Fabric Manager (NVIDIA). InfiniBand fabric controller. Talked to via `[ib_config]` + `[ib_fabrics.<name>]`. |
| **VMaaS** | Virtual Machine as a Service. VM integration mode for sites that run VMs alongside bare-metal. Configured via `[vmaas_config]`. |
| **VNI** | VXLAN Network Identifier. Allocated from `[pools.vni]` / `[pools.vpc-vni]`. |
| **VPC** | Virtual Private Cloud. Tenant-level network isolation domain. Peering controlled by `vpc_peering_policy`. |

## Where to go next

| Goal | Reference |
|------|-----------|
| First-time site bring-up | [`helm-prereqs/README.md`](../../../helm-prereqs/README.md) |
| Every siteConfig TOML field, type, default | [`crates/api-core/src/cfg/README.md`](../../../crates/api-core/src/cfg/README.md) |
| Helm chart structure and subcharts | [`helm/README.md`](../../../helm/README.md) |
| Infrastructure prerequisites (Vault, Postgres, ESO, cert-manager) | [`helm/PREREQUISITES.md`](../../../helm/PREREQUISITES.md) |
| Tenant management and RBAC | [`docs/configuration/tenant_management.md`](../../../docs/configuration/tenant_management.md) |
| Network isolation policy | [`docs/configuration/network-isolation.md`](../../../docs/configuration/network-isolation.md) |
| Org permissions / multitenancy | [`docs/configuration/org-permissions.md`](../../../docs/configuration/org-permissions.md) |
| DPU platform configuration | [`docs/dpu-management/dpu_configuration.md`](../../../docs/dpu-management/dpu_configuration.md) |
