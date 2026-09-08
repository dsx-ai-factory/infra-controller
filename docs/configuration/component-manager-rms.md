# Component Manager RMS Backends (Day 1) <Badge intent="info">v2.0</Badge>

Operator guide for configuring **[Rack Management Service (RMS)](https://docs.nvidia.com/rms/documentation/home/)** backends in the
`[component_manager]` section of `nico-api` site config, and the **rack profile**
data those backends require for node descriptors.

`[component_manager]` manages compute trays, NVLink switches, and power shelves.
When a role's backend is set to `rms`, NICo builds an RMS `NodeDescriptor` from
the rack profile. If a configured rack profile is missing required fields,
`nico-api` **fails configuration validation at startup**. Per-rack
`rack_profile_id` assignments are not checked at startup. Those errors surface
at runtime when an RMS operation runs (refer to
[Startup validation](#startup-validation)).

Canonical field reference: [`crates/api-core/src/cfg/README.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/cfg/README.md).
Configure the `[rms]` block (mTLS connectivity to the external RMS) separately;
the examples on this page cover the component-manager and rack-profile fields.
The `nsm` and `psm` backend values require externally managed services; the
NICo deployment charts do not install NSM or PSM.

`setup.sh` deploys RMS in-cluster by default (phase 5c, opt out with `--skip-rms`): it installs the
[nv-rms](https://github.com/dsx-ai-factory/nv-rms) rack-manager chart with an
mTLS certificate issued from `vault-nico-issuer`, matching the `[rms]` defaults
NICo Core's chart already renders (`rms-api-server.rack-manager.svc.cluster.local:8801`).
An externally managed RMS remains supported - point `[rms] api_url` at it and
pass `--skip-rms`.

For predecessor Flow deployments that still bundle PSM/NSM, the setup.sh path
is intentionally unreachable: the upgrade guard exits before any phase runs,
because a prerequisites sync would strand the bundled managers' credentials.
To move Core backends to RMS *before* that Flow upgrade, use an externally
managed RMS, or install the rack-manager chart manually from the
`helm-prereqs/nv-rms` submodule with a database the guard cannot touch
(`databaseMode: standalone`, or an external PostgreSQL). Once the Flow-only
upgrade completes, re-run `setup.sh` and phase 5c adopts RMS management.

---

## What the rack profile provides

For RMS component-manager backends, NICo sends a descriptor containing three
attributes:

- **Role**: Derived from the operation as `compute`, `switch`, or `power_shelf`.
- **Product family**: Taken from `product_family`.
- **Vendor**: `rack_capabilities.<role>.vendor`, for each role using an RMS
  backend.

NICo trims outer whitespace from product-family and vendor values and requires
both to be non-empty. Case and internal punctuation are preserved after
trimming. NICo does not map these values to a supported-hardware list. RMS
validates each role/vendor/product-family combination when a request is made.

Each `rack_capabilities.<role>` section also requires a `count` field. This
field is independent of RMS: it tells the rack state machine how many devices
with that role the rack must have before it can progress. A rack stays in
`Created` until all three roles have at least `count` devices registered; it
stays in `Discovering` until all three roles have at least `count` devices in
`Ready` state. All three roles — `compute`, `switch`, and `power_shelf` —
require a `count` regardless of which backends are set to `rms`.

For product families other than `gb200` and `gb300`, the `GetRackProfile`
`product_family` enum is `UNSPECIFIED`. The configured string remains available
to descriptor-based RMS operations.

NICo always sends descriptor-based RMS requests. For exact role, vendor, and
product-family combinations represented by the current RMS `NodeType` enum,
NICo also sends that enum and legacy firmware-filter entries for compatibility
with older RMS servers. Other combinations leave `NodeType` unset and require
RMS support for `NodeDescriptor`. This best-effort legacy mapping does not
participate in startup validation. VRNVL72 power shelves are descriptor-only
because no matching legacy `NodeType` exists.

## Supported RMS descriptor combinations

RMS accepts these role, vendor, and product-family combinations:

| `product_family` | Role | Supported vendor |
| ---------------- | ---- | ---------------- |
| `gb200` | `compute` | `nvidia` |
| `gb200` | `switch` | `nvidia` |
| `gb200` | `power_shelf` | `liteon`, `delta` |
| `gb300` | `compute` | `nvidia`, `lenovo` |
| `gb300` | `switch` | `nvidia` |
| `gb300` | `power_shelf` | `liteon`, `delta` |
| `vrnvl72` | `compute` | `nvidia` |
| `vrnvl72` | `switch` | `nvidia` |
| `vrnvl72` | `power_shelf` | `liteon`, `delta` |

RMS compares normalized values: matching is case-insensitive and ignores
spaces, hyphens, and underscores. For example, `Lite-On` and `LiteOn` are
equivalent, as are `vr_nvl72` and `vrnvl72`. After normalization, RMS compares
full values rather than prefixes, so `NVIDIACorp` does not match `NVIDIA`.

VRNVL72 power shelves use the GB200 LiteOn or Delta internal implementation
after descriptor resolution. RMS returns `INVALID_ARGUMENT` when no descriptor
rule matches. NICo accepts other non-empty values at startup; RMS validates
support when it receives a request. Consult the hardware compatibility list for
the deployed RMS version when NICo and RMS versions differ.

## Startup validation

NICo validates configured rack profiles at startup **when any component-manager
backend is set to `rms`**. The backend fields default to `rms`, so a deployment
that enables a single RMS role must **explicitly set the other backend fields to
non-RMS values**. Startup validation checks the product family and the vendor
fields for enabled RMS roles.

For example, if `power_shelf_backend = "rms"` and the other backend fields are
set to non-RMS values, then `rack_capabilities.power_shelf.vendor` is the sole
required vendor field.

`[rack_profiles]` must contain at least one profile when any component-manager
backend is set to `rms`; an empty table is rejected at startup.

Each rack that uses an RMS-backed operation must have a `rack_profile_id` matching
a key under `[rack_profiles]`. Startup validation does not scan existing rack
database rows, so missing or unknown per-rack profile IDs are still checked when
an RMS operation runs.

---

## Examples

### GB200 rack with RMS for compute, switch, and power shelf

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
count = 18

[rack_profiles.NVL72.rack_capabilities.switch]
vendor = "NVIDIA"
count = 9

[rack_profiles.NVL72.rack_capabilities.power_shelf]
vendor = "LiteOn"
count = 8
```

### GB300 rack, Lenovo compute trays and Delta power shelves

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

### Power shelf backend uses RMS; compute and switch do not

The compute and switch component-manager backends are explicitly set to non-RMS
values, so component-manager startup validation requires the power shelf vendor
field and no others:

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

## Accepted values

| Field | Accepted values |
| ----- | --------------- |
| `product_family`, when an RMS-backed operation uses the profile | Non-empty string; RMS validates support at request time |
| `rack_hardware_topology` | `gb200_nvl36r1_c2g4_topology`, `gb200_nvl72r1_c2g4_topology`, `gb300_nvl36r1_c2g4_topology`, `gb300_nvl72r1_c2g4_topology`, `vr_nvl8r1_c2g4_rtf_topology`, `vr_nvl72r1_c2g4_topology` |
| Compute profile vendor, when `compute_tray_backend = "rms"` | Non-empty string; RMS validates support at request time |
| Switch profile vendor, when `nv_switch_backend = "rms"` | Non-empty string; RMS validates support at request time |
| Power shelf profile vendor, when `power_shelf_backend = "rms"` | Non-empty string; RMS validates support at request time |
| `rack_capabilities.<role>.count` (all three roles, always required) | Non-negative integer. Minimum devices of that role that must be registered before the rack leaves `Created`, and in `Ready` state before it leaves `Discovering`. Independent of RMS backend selection. |

## Machine ingestion note

The separate site-explorer machine-ingestion path performs an RMS slot/tray
lookup and uses the rack profile to build a compute node descriptor. This path
runs when **both** conditions hold:

1. An RMS client is configured (the `[rms]` block is present).
2. The machine has a `rack_id`.

Under those conditions the profile must include compute `product_family` and
`vendor` data. Setting `compute_tray_backend` to a non-RMS value does not
remove this requirement.
