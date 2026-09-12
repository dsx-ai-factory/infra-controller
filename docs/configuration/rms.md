# RMS Configuration (Day 1) <Badge intent="info">v2.0</Badge>

Reference for the `nico-api` site-configuration settings that connect NICo to the
**[Rack Management Service (RMS)](https://docs.nvidia.com/rms/documentation/home/)** and
select it as the backend for rack hardware operations.

Settings are grouped by the component that consumes them. For how the integration fits
together, refer to [RMS Backend](../architecture/backends/rms.md).

## Overview

Four configuration sections take part in the integration, and each is owned by a different
NICo component:

| Section | Component | Purpose |
| --- | --- | --- |
| `[rms]` | RMS client | Where RMS is and how to authenticate to it |
| `[component_manager]` | Component Manager | Which backend handles each hardware role |
| `[rack_profiles]` | Rack state machine and Component Manager | Node descriptors and the firmware manifest source |
| `[switch_state_controller]` | Switch state controller | Which switch services receive mTLS certificates |

NICo reads these from the site config file mounted into the `nico-api` pod. In Helm-based
deployments the file is rendered from chart values, so the settings on this page are the
TOML that those values produce.

Canonical field reference for all NICo settings:
[`crates/api-core/src/cfg/README.md`](https://github.com/NVIDIA/infra-controller/blob/main/crates/api-core/src/cfg/README.md).

---

## `[rms]` — RMS client connection

Controls whether NICo builds an RMS client, and the mutual TLS material it presents.

### `api_url`

The RMS gRPC endpoint.

| Property | Value |
| --- | --- |
| Type | String |
| Required | Yes, to enable any RMS-backed workflow |
| Default | None |

When this field is unset or empty, NICo does not construct an RMS client and every RMS-backed
operation reports a clean error. NICo Core still starts and runs healthy, so enabling the
setting before RMS is deployed is safe.

The NICo chart renders `https://rms-api-server.rack-manager.svc.cluster.local:8801`, which
matches the `rack-manager` chart's Service in its default namespace.

### `enforce_tls`

Requires TLS on connections to RMS.

| Property | Value |
| --- | --- |
| Type | Boolean |
| Required | No |
| Default | `true` |

Keep this enabled in production. Disable it only against an RMS instance deliberately started
in its insecure development mode.

### `root_ca_path`, `client_cert`, `client_key`

Paths inside the `nico-api` container to mutual TLS material for RMS.

| Property | Value |
| --- | --- |
| Type | String |
| Required | No |
| Default | None |

Leave all three unset in a standard deployment. The RMS client library then discovers
`nico-api`'s own SPIFFE certificate mount at `/var/run/secrets/spiffe.io/`, which shares a
certificate authority with the RMS API server certificate. That produces mutual TLS in both
directions with no extra mounts.

<Warning>
Supplying `client_cert` without `client_key` yields no client certificate **and** suppresses
the SPIFFE fallback. Set both fields or neither.
</Warning>

### `scale_up_fabric_manager_api_version`

Selects the RMS API used by the ScaleUpFabric Manager workflow.

| Property | Value |
| --- | --- |
| Type | String, one of `v1` or `v2` |
| Required | No |
| Default | `v2` |

`v1` is accepted for configuration compatibility and has no effect. Any other value fails
configuration load.

---

## `[component_manager]` — backend selection

Selects which backend handles each hardware role. **All three backend fields default to
`rms`**, so a deployment that enables RMS for one role must explicitly set the others to
non-RMS values.

| Field | Accepted values | Default |
| --- | --- | --- |
| `compute_tray_backend` | `rms`, `core`, `mock` | `rms` |
| `nv_switch_backend` | `rms`, `nsm`, `mock` | `rms` |
| `power_shelf_backend` | `rms`, `psm`, `mock` | `rms` |

The `nsm` and `psm` values require externally managed services. The NICo deployment charts do
not install NSM or PSM.

### State controller routing

| Field | Type | Default |
| --- | --- | --- |
| `compute_tray_use_state_controller` | Boolean | `false` |
| `nv_switch_use_state_controller` | Boolean | `false` |
| `power_shelf_use_state_controller` | Boolean | `false` |

When `true`, power control and firmware update calls for that role route through the state
controller instead of dispatching directly to the device. Status reads and firmware-catalog
reads still pass through to the direct backend. The NICo chart sets all three to `true`.

---

## `[rack_profiles]` — node descriptors and firmware source

For RMS backends, NICo builds a node descriptor from the rack profile containing three
attributes:

- **Role** — derived from the operation as `compute`, `switch`, or `power_shelf`
- **Product family** — taken from `product_family`
- **Vendor** — taken from `rack_capabilities.<role>.vendor`, for each role using an RMS backend

NICo trims outer whitespace from product-family and vendor values and requires both to be
non-empty. Case and internal punctuation are preserved after trimming. NICo does not check
these values against a supported-hardware list; RMS validates each combination when it
receives a request and returns `INVALID_ARGUMENT` when no rule matches.

RMS normalizes both values before matching: comparison is case-insensitive and ignores spaces,
hyphens, and underscores, so `Lite-On`, `LiteOn`, and `lite_on` are equivalent. Normalized
values are compared in full rather than by prefix, so `NVIDIACorp` does not match `NVIDIA`.

<Note>
RMS resolves a fixed set of role, vendor, and product-family combinations, and that set
changes as platforms are added. Check the
[Hardware Compatibility List](https://docs.nvidia.com/rms/documentation/reference/hardware-compatibility-list)
for the combinations the deployed RMS version accepts, rather than assuming a vendor is
supported because NICo accepted the string at startup.
</Note>

### Field reference

| Field | Accepted values | Required |
| --- | --- | --- |
| `product_family` | Non-empty string; RMS validates support at request time | When an RMS-backed operation uses the profile |
| `rack_hardware_topology` | `gb200_nvl36r1_c2g4_topology`, `gb200_nvl72r1_c2g4_topology`, `gb300_nvl36r1_c2g4_topology`, `gb300_nvl72r1_c2g4_topology`, `vr_nvl8r1_c2g4_rtf_topology`, `vr_nvl72r1_c2g4_topology` | No |
| `rack_capabilities.<role>.vendor` | Non-empty string; RMS validates support at request time | For each role whose backend is `rms` |
| `rack_capabilities.<role>.count` | Non-negative integer | Always, for all three roles |

`count` is independent of RMS. It tells the rack state machine how many devices of that role a
rack must have before it can progress. A rack stays in `Created` until all three roles have at
least `count` devices registered, and stays in `Discovering` until all three roles have at
least `count` devices in `Ready` state.

Each rack that uses an RMS-backed operation must have a `rack_profile_id` matching a key under
`[rack_profiles]`.

### `firmware_object` — rack firmware manifest source

Declares where the rack's source-of-truth firmware manifest is fetched from during rack
ingestion and maintenance.

| Field | Type | Required | Default |
| --- | --- | --- | --- |
| `url` | URL | Yes, when the block is present | None |
| `fetch_timeout` | Duration string | No | `30s` |

When a profile omits the `firmware_object` block, rack ingestion skips the automatic firmware
update unless an explicit maintenance request supplies a firmware object. NICo fetches the
document, confirms it parses as a JSON object, and sends the body inline to RMS. The document
body is capped at 16 MiB.

```toml
[rack_profiles.NVL72.firmware_object]
url = "https://artifacts.example.internal/sot/vr-nvl72-0.6-build14.json"
fetch_timeout = "60s"
```

---

## `[switch_state_controller]` — switch mTLS services

### `switch_mtls_services`

Selects which switch services receive the certificate NICo installs through RMS.

| Property | Value |
| --- | --- |
| Type | Array of strings |
| Required | No |
| Default | All four services |

Accepted values, written in snake case:

| Value | Service |
| --- | --- |
| `nvue_api` | NVOS REST API (NVUE) |
| `scale_up_fabric_telemetry` | ScaleUpFabric telemetry collector (NMX-T) |
| `scale_up_fabric_manager` | ScaleUpFabric manager daemon (NMX-C) |
| `scale_up_fabric_telemetry_interface` | ScaleUpFabric telemetry interface (gNMI) |

Omitting the field **and** supplying an empty list both select all four services.

<Note>
`[rack_state_controller] nmx_cluster_switch_mtls_services` is deprecated. The setting is still
parsed so existing site configurations load, but rack maintenance does not configure switch
certificates and never reads it. Use `switch_mtls_services` instead.
</Note>

---

## Startup validation

NICo validates rack profiles at startup **when any Component Manager backend is set to
`rms`**. Validation checks the product family and the vendor fields for the roles that use an
RMS backend.

| Condition | Outcome |
| --- | --- |
| No role uses the `rms` backend | Validation is skipped |
| `[rack_profiles]` is empty while any role uses `rms` | Rejected at startup |
| Missing `product_family` on a profile, with any RMS role | Rejected at startup |
| Missing vendor for a role whose backend is `rms` | Rejected at startup |
| Per-rack `rack_profile_id` missing or unknown | Surfaces at runtime, not startup |

Startup validation does not scan rack database rows, so per-rack profile assignment errors
appear when an RMS operation runs. Unknown keys in `[rms]` and `[component_manager]` fail
configuration load, so a misspelled field is caught at startup rather than ignored.

---

## Examples

### GB200 rack using RMS for all three roles

```toml
[component_manager]
compute_tray_backend = "rms"
nv_switch_backend = "rms"
power_shelf_backend = "rms"

[rms]
api_url = "https://rms-api-server.rack-manager.svc.cluster.local:8801"
enforce_tls = true

[rack_profiles.NVL72]
product_family = "gb200"
rack_hardware_topology = "gb200_nvl72r1_c2g4_topology"

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

### GB300 rack with Lenovo compute trays and Delta power shelves

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

### Power shelves use RMS; compute and switch do not

The compute and switch backends are set to non-RMS values, so startup validation requires the
power shelf vendor field and no others:

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

---

## Machine ingestion requirement

The site-explorer machine-ingestion path performs an RMS slot and tray lookup, and uses the
rack profile to build a compute node descriptor. This path runs when **both** conditions hold:

1. An RMS client is configured, meaning `[rms] api_url` is set.
2. The machine has a `rack_id`.

Under those conditions the profile must include compute `product_family` and `vendor` data.
Setting `compute_tray_backend` to a non-RMS value does not remove this requirement.

---

## RMS server-side configuration

Settings on this page configure **NICo's side** of the integration. RMS has its own runtime
configuration, delivered as a TOML file the container reads from `/etc/rms/config.toml` at
startup, covering its listening ports, TLS material, switch certificate roots, firmware
directory, job retention, and database connection.

The RMS documentation is the source of truth for those settings:

- [Configuring RMS](https://docs.nvidia.com/rms/documentation/configuration/configuring-rms) — the complete TOML reference
- [Configuration via Helm](https://docs.nvidia.com/rms/documentation/configuration/configuration-via-helm) — how chart values render that file
- [Kubernetes Deployment with Helm](https://docs.nvidia.com/rms/documentation/deployment/kubernetes-deployment-with-helm) — deploying the `rack-manager` chart

Two RMS settings have to agree with the NICo values on this page:

| RMS setting | Must match |
| --- | --- |
| `port` | The port in `[rms] api_url` |
| `tls.ca` | A certificate authority that trusts NICo's client certificate |

The RMS API server certificate also has to carry a subject alternative name matching the DNS
name in `[rms] api_url`.

## See also

- [RMS Backend](../architecture/backends/rms.md) — how the integration works
- [Hardware Compatibility List](https://docs.nvidia.com/rms/documentation/reference/hardware-compatibility-list) — the hardware RMS supports
