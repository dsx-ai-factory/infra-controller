# Adding Support for New Hardware

This guide explains how to add or extend hardware support in the NICo stack when new BMC/server hardware arrives that does not work out of the box. The general process is: ingest the hardware, observe where it fails, and patch the appropriate layer based on which of the three scenarios applies.

Changes for new hardware must not break existing platforms. Guard quirks behind vendor, model, or firmware checks instead of changing a shared path for every BMC.

For background on how NICo uses Redfish end to end, see [Redfish Workflow](../architecture/redfish_workflow.md). For currently supported hardware, see the [Hardware Compatibility List](../hcl.md).

## Before You Start

Hardware support has a higher review bar than a software-only change because maintainers and CI might not have access to the device. Before writing code:

1. Open an issue through the [GitHub issue chooser](https://github.com/dsx-ai-factory/infra-controller/issues/new/choose) and agree on scope with the maintainers.

   Use a Feature/Enhancement request for a new vendor or platform, and a Bug Report for a regression on supported hardware. Include the NICo version, OEM, model or SKU, DPU generation, NICs, BMC firmware, reproduction steps, expected and observed behavior, affected Redfish endpoints, and any time limit on hardware access.

1. Check the [Hardware Compatibility List](../hcl.md) to avoid duplicating an already-supported platform or to determine whether this is a firmware regression on an existing one.

1. Confirm that the hardware exposes the operations NICo needs.

   For a managed host, this normally includes Redfish power control, boot-order management, UEFI Secure Boot configuration, firmware update, and serial-over-LAN. Record any missing or OEM-only operation in the issue.

1. Capture evidence from the live BMC before designing a workaround.

   This includes service root, systems, managers, chassis, BIOS, boot options, network adapters, and firmware inventory. Remove credentials, addresses, serial numbers, and other site-specific data before sharing captures.

The contributor is the hardware owner for the change and is responsible for validating it on the physical device. A build or mock-only test cannot establish that a vendor's real Redfish implementation works.

## Contribution Workflow and Pull Request Evidence

This checklist is the guide to the rest of this page:

1. Complete the issue scoping and evidence collection in [Before You Start](#before-you-start).

1. Follow [Coordinating Redfish Library Changes](#coordinating-redfish-library-changes) for any upstream nv-redfish or libredfish changes and releases.

1. Implement the changes for your situation in [The Three Scenarios](#the-three-scenarios), along with the matching [Changes in NICo](#changes-in-nico) and any [optional BMC mock coverage](#optional-bmc-mock-coverage).

1. Complete the live-BMC validation described in [Testing Site Explorer with the BMC Explorer Tool](#testing-site-explorer-with-the-bmc-explorer-tool) and [Testing with `nico-admin-cli redfish`](#testing-with-nico-admin-cli-redfish).

1. Validate every affected lifecycle transition through a running NICo deployment (if possible). A complete new host integration normally covers discovery and ingestion, inventory and health monitoring, BIOS and Secure Boot setup, boot order, power control, serial-over-LAN, lockdown, credential rotation, instance creation and termination, reprovisioning, and affected firmware update paths. See the [Quick Start Guide](../getting-started/quick-start.md), [Ingesting Hosts](../provisioning/ingesting-hosts.md), [Managed Host State Diagrams](../architecture/state_machines/managedhost.md), [DPU Lifecycle Management](../dpu-management/dpu-lifecycle-management.md), and [Firmware Updates](../operations/firmware-updates.md).

1. Include the Hardware Compatibility List update in the same NICo change as the Site Explorer and BMC Explorer integration.

1. Open **one** NICo pull request using the repository [contribution process](https://github.com/dsx-ai-factory/infra-controller/blob/main/CONTRIBUTING.md) and [pull request template](https://github.com/dsx-ai-factory/infra-controller/blob/main/.github/PULL_REQUEST_TEMPLATE.md). Link the agreed issue and upstream nv-redfish or libredfish pull requests and releases, and ensure every commit is both DCO-signed-off and cryptographically signed.

Include the following evidence in the NICo pull request:

- Hardware OEM, model, and SKU
- BMC, BIOS, DPU, and NIC firmware versions that apply
- The commands and lifecycle paths tested, with results
- Any operation that could not be tested and why
- Any difference between the live Redfish surface and optional mock coverage

## Overview

NICo discovers and manages bare-metal hosts through their BMC (Baseboard Management Controller) via the DMTF Redfish standard. Two Rust Redfish client libraries handle this:

| Library | Role | Where Used |
|---|---|---|
| **[nv-redfish](https://github.com/NVIDIA/nv-redfish)** | Schema-driven, fast: site exploration reports, firmware inventory, sensor collection, health monitoring. | Site Explorer exploration (`crates/site-explorer/`), Hardware Health (`crates/health/src/`) |
| **[libredfish](https://github.com/NVIDIA/libredfish)** | Stateful BMC interactions: boot config, BIOS setup, power control, account/credential management, lockdown | Site Explorer state controller operations (`crates/site-explorer/`) |

Site Explorer uses `nv-redfish` by default to generate an `EndpointExplorationReport`. _Exploration through_ `libredfish` _is deprecated_; it remains temporarily available for comparison and transition testing through the `explore_mode` configuration setting (`SiteExplorerExploreMode`):

| Mode | Behavior |
|---|---|
| `nv-redfish` | Generate the report with nv-redfish (default) |
| `libredfish` | Generate the report with the deprecated libredfish exploration path |
| `compare-result` | Run both paths and report differences for transition testing |

Implement new exploration support in the `nv-redfish` path. Do not add new hardware support only to the deprecated libredfish exploration path. Stateful operations such as boot order, BIOS setup, lockdown, and credential rotation still use libredfish, so both libraries may need changes for a complete platform integration.

Beyond the Redfish libraries, **NICo itself** has vendor-aware logic that also needs updating - see [Changes in NICo](#changes-in-nico).

### Coordinating Redfish Library Changes

[nv-redfish](https://github.com/NVIDIA/nv-redfish) and [libredfish](https://github.com/NVIDIA/libredfish) are separate upstream repositories. Submit the required library changes upstream first, then link those pull requests to your NICo hardware-support issue.

After your upstream changes are merged, and an nv-redfish release or libredfish tag is available, open **one** NICo pull request. In that single pull request, update NICo's dependency in the workspace `Cargo.toml` and `Cargo.lock`, and make your corresponding Site Explorer, BMC Explorer, vendor-model, test, and documentation changes.

<Warning title="No local paths">
Local path overrides are only for development before the releases exist. Restore the released dependency declarations and regenerate `Cargo.lock` before submitting the NICo pull request; do not commit local checkout paths.
</Warning>

## The Three Scenarios

### Scenario 1: New BMC Vendor

The hardware uses a BMC firmware stack that does not map to any existing `RedfishVendor` variant.

**What to do:**

1. **Add a `RedfishVendor` variant** in `libredfish/src/model/service_root.rs`.

1. **Extend vendor detection** in `ServiceRoot::vendor()` (same file). The vendor string comes from `GET /redfish/v1` - the `Vendor` field, or failing that, the first key in the `Oem` object. If the vendor string alone is not enough to distinguish the BMC (e.g., the vendor is "Lenovo" but some models use an AMI-based BMC), use secondary signals like `self.has_ami_bmc()` or `self.product`.

1. **Create a vendor module** (or reuse an existing one). Each vendor has a file `libredfish/src/<vendor>.rs` containing a `Bmc` struct that implements the `Redfish` trait. If the new vendor's BMC is very close to an existing one (e.g., LenovoAMI reuses `ami::Bmc`), you can route to the existing implementation.

1. **Wire up** `set_vendor` in `libredfish/src/standard.rs` to dispatch the new variant to the appropriate `Bmc` implementation.

1. **Implement the `Redfish` trait** for the new `Bmc`. Start by delegating to `RedfishStandard` and override methods as needed. The methods below are grouped by how they are used in the state machine; almost all need vendor-specific overrides.

   **BIOS / machine setup** - called during initial ingestion and instance creation to configure UEFI settings:
   - `machine_setup()` - applies BIOS attributes (names differ per vendor and model)
   - `machine_setup_status()` - polls whether all `machine_setup` changes have taken effect
   - `is_bios_setup()` - lightweight check used during instance creation (`PollingBiosSetup`) to confirm BIOS is ready before proceeding to boot order configuration

   **Lockdown** - called to secure the BMC before tenant use and unlocked during instance termination or reconfiguration:
   - `lockdown()` - enable/disable BMC security lockdown
   - `lockdown_status()` - polled by the state controller to confirm lockdown state; wrong results cause machines to get stuck
   - `lockdown_bmc()` - lower-level BMC-specific lockdown (e.g., iDRAC lockdown on Dell, distinct from BIOS lockdown)

   **Boot order** - called during ingestion to set DPU-first boot and during DPU reprovisioning:
   - `set_boot_order_dpu_first()` - reorder boot options so the DPU boots first (platform-specific boot option discovery)
   - `boot_once()` - one-time boot from a specific target (e.g., `UefiHttp` for DPU HTTP boot path)
   - `boot_first()` - persistently change boot order to a given target

   **Serial console** - SSH console access setup:
   - `setup_serial_console()` - configure BMC serial-over-LAN
   - `serial_console_status()` - polled to confirm setup; incorrect results stall provisioning

   **Credential management** - called during initial ingestion to rotate factory defaults:
   - `change_password()` - rotate BMC user password
   - `change_uefi_password()` / `clear_uefi_password()` - UEFI password management (only tested on Dell, Lenovo, NVIDIA)
   - `set_machine_password_policy()` - apply password-never-expires policy (vendor-specific)

   **Important:** Pay careful attention to all **status/polling methods** (`is_bios_setup()`, `lockdown_status()`, `machine_setup_status()`, `serial_console_status()`, etc.). The state controller polls these during provisioning, instance creation, instance termination, and reprovisioning to decide when to advance state. If they return incorrect results, machines will get stuck in polling states, fail to terminate properly, or skip required configuration steps.

1. **Add OEM model types** if needed in `libredfish/src/model/oem/<vendor>.rs`.

1. **Add unit tests** for vendor detection and other deterministic behavior. Consider adding a BMC mock for lasting regression coverage (see [Optional BMC mock coverage](#optional-bmc-mock-coverage)).

1. **Update nv-redfish** - it is the default and supported path for Site Explorer discovery. See [nv-redfish quirks](#adding-nv-redfish-quirks-for-exploration-and-health-monitoring).

1. **Update NICo** - add the vendor to `BMCVendor`, `HwType`, and handle any state controller quirks. See [Changes in NICo](#changes-in-nico).

### Scenario 2: New Server Model with Quirks

The hardware uses an already-supported BMC vendor but the specific model has quirks: different BIOS attribute names, unusual boot option paths, model-specific OEM extensions, etc.

**What to do:**

1. **Identify the model string.** `GET /redfish/v1/Systems/{id}` returns a `Model` field. The function `model_coerce()` in `libredfish/src/lib.rs` normalizes this by replacing spaces with underscores.

1. **Use BIOS / OEM manager profiles** for config-driven differences. NICo supports per-vendor, per-model BIOS settings via the `BiosProfileVendor` type in `lib.rs`, letting you define model-specific attributes in config (TOML) without code changes.

1. **Add model-specific branches** in the vendor module when profiles are not enough. Use the model/product string from `ComputerSystem` to gate behavior.

1. **Handle missing or renamed attributes.** Check the actual BIOS attributes via `GET /redfish/v1/Systems/{id}/Bios` on the target hardware. If an attribute is missing, add a guard that logs and skips rather than failing.

### Scenario 3: New Firmware for an Existing Model

A firmware update for an already-supported model introduces regressions: removed endpoints, changed response schemas, renamed attributes, etc.

**What to do:**

1. **Compare old and new firmware Redfish responses.** Use `curl --insecure --user '<user>:<password>' https://<bmc-ip>/redfish/v1/<resource>` to `GET` the relevant endpoints on both versions and diff the responses.

1. **Add defensive handling** where endpoints may no longer exist - catch `404` errors and fall through.

1. **Fix deserialization issues**: null values in arrays (custom deserializers), new enum values, missing required fields (`Option<T>`).

1. **Adjust OEM-specific paths** if the firmware reorganizes its Redfish tree.

1. **Guard behavioral changes behind firmware version checks** if needed, using `ServiceRoot.redfish_version` or firmware inventory versions.

## Changes in NICo

Beyond the Redfish libraries, NICo itself has vendor-aware logic that needs updating for new hardware.

### `BMCVendor` enum (`crates/bmc-vendor/src/lib.rs`)

NICo has its own `BMCVendor` enum, distinct from libredfish's `RedfishVendor`. It is used throughout NICo for vendor-specific branching in the state controller, credential management, and exploration. When adding a new vendor:

1. **Add the variant** to `BMCVendor`.

1. **Update the** `bmc_vendor()` **mapping** in `crates/redfish/src/libredfish/conv.rs` so libredfish's vendor detection flows into NICo's enum.

1. **Extend parsing** in `From<&str>` and `from_udev_dmi()` as applicable.

### `HwType` enum (`crates/bmc-explorer/src/hw/mod.rs`)

The `bmc-explorer` crate (used by the nv-redfish exploration path) classifies hardware into `HwType` variants. Each variant maps to a `BMCVendor` via `bmc_vendor()`. For a new hardware type, add a variant to `HwType` and implement the required methods. If the hardware type has unique exploration behavior, add a corresponding module under `crates/bmc-explorer/src/hw/`.

### State controller vendor branches

The state controller (`crates/api/src/state_controller/machine/handler.rs`) has vendor-specific logic gated on `BMCVendor` for operations that cannot be handled generically in libredfish. Examples:

- **Factory credential rotation**: On first exploration, NICo changes the factory default BMC password. This is vendor-aware - ensure the new vendor's credential rotation path works correctly.
- **UEFI password setting**: Only tested on Dell, Lenovo, and NVIDIA - other vendors log a warning and skip.
- **Power cycling**: Lenovo SR650 V4s use IPMI chassis reset instead of Redfish `ForceRestart` to avoid killing DPU power. Lenovo BMCs need an explicit `bmc_reset()` after firmware upgrades.
- **Lockdown**: Dell requires BMC lockdown to be disabled separately before UEFI password changes.

Review `handler.rs` for `bmc_vendor().is_*()` calls and add branches for the new vendor where its behavior differs.

## Testing Site Explorer with the BMC Explorer Tool

Use the standalone BMC explorer tool to exercise the same `BmcEndpointExplorer` report-generation code as Site Explorer against a live BMC. The tool creates the full `EndpointExplorationReport` that NICo would produce for the BMC.

**The report is written as JSON to standard output** (stdout), and diagnostic logs go to standard error (stderr). Validate that the report contains the expected systems, managers, chassis, network adapters, firmware inventory, boot interface, and vendor/model classification. The explorer tool, called `bmc-explorer-cli`, does not require a running NICo deployment or a database.

Build the package from the workspace root:

```bash
cargo build -p bmc-explorer-cli
```

Then, run the CLI against the BMC using the supported nv-redfish exploration path:

```bash
target/debug/bmc-explorer-cli \
  --username <bmc-user> \
  --password <bmc-password> \
  --mode nv-redfish \
  --boot-mac <host-boot-interface-mac> \
  --bmc-port 443 \
  <bmc-ip>
```

`--boot-mac` is the MAC address of the interface from which the host is expected to boot. For the common managed-DPU configuration, use the host-facing `pf0` MAC of the primary DPU; for an integrated-NIC configuration, use the NIC selected as the primary boot interface. Use the same MAC for exploration, machine setup, setup status, and boot-order tests. Omit it only when the platform does not require a selected boot interface. See [Boot Interfaces and DPU Policies](../provisioning/boot-interfaces-and-dpu-modes.md) for how NICo selects and persists this value.

`--bmc-port` is the (optional) port on which the BMC listens. HTTPS port 443 is the default; use `--bmc-port <port>` for a BMC listening elsewhere.

The CLI itself defaults to `compare-result`, but new Site Explorer support must be tested explicitly with `--mode nv-redfish`, matching the deployed default. `--mode compare-result` is useful only when investigating a difference from the deprecated libredfish exploration path. It runs both implementations and logs report differences.

For repeated timing tests, add `--benchmark <iterations>`. The benchmark runs the same exploration multiple times and suppresses the JSON report:

```bash
target/debug/bmc-explorer-cli \
  --username <bmc-user> \
  --password <bmc-password> \
  --mode nv-redfish \
  --benchmark 10 \
  <bmc-ip>
```

If the change is in a local nv-redfish checkout, temporarily replace the workspace dependency with the local crate before building:

```toml
# Cargo.toml (workspace root)
[workspace.dependencies]
nv-redfish = { path = "../nv-redfish/redfish" }
```

Adjust the path for the location of the checkout.

## Testing with `nico-admin-cli redfish`

The fastest way to validate libredfish changes against a real BMC is to compile `nico-admin-cli` with a **local checkout of libredfish** and use the `redfish` subcommand to test specific operations directly, rather than waiting for Site Explorer or the state machine to exercise the code path.

### Setup: Use a local libredfish checkout

Place your libredfish checkout inside the NICo workspace (or anywhere accessible), then override the dependency in the workspace `Cargo.toml`:

```toml
# Cargo.toml (workspace root)
[workspace.dependencies]
# Comment out the git version:
# libredfish = { git = "https://github.com/NVIDIA/libredfish.git", tag = "<current-tag>" }
# Point to your local checkout instead:
libredfish = { path = "libredfish" }
```

Then build the CLI from the workspace root:

```bash
cargo build -p nico-admin-cli
```

### Running commands against a real BMC

The `redfish` subcommand talks directly to a BMC - no NICo deployment needed:

```bash
# Check if vendor detection and basic connectivity work
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  get-power-state

# Read BIOS attributes to see what the BMC exposes
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  bios-attrs

# Test machine setup (the core provisioning step)
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  machine-setup \
  --boot-interface-mac <host-boot-interface-mac>

# Check if machine setup succeeded
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  machine-setup-status \
  --boot-interface-mac <host-boot-interface-mac>

# Test boot order (set DPU first)
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  set-boot-order-dpu-first \
  --boot-interface-mac <host-boot-interface-mac>

# Test lockdown
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  lockdown-enable
target/debug/nico-admin-cli redfish \
  --address <bmc-ip> \
  --username <user> \
  --password <pass> \
  lockdown-status

# Browse any Redfish endpoint directly when diagnosing raw responses
curl --insecure --user '<user>:<password>' https://<bmc-ip>/redfish/v1
```

If all of these commands work correctly, there is a good chance the hardware will work end-to-end through the state machine.

## Code Structure Reference

```text
libredfish/
├── src/
│   ├── lib.rs                    # Redfish trait, BiosProfile types, model_coerce()
│   ├── standard.rs               # RedfishStandard: defaults + set_vendor() dispatch
│   ├── network.rs                # create_client(): ServiceRoot → vendor → set_vendor
│   ├── ami.rs, dell.rs, hpe.rs,  # Vendor-specific Redfish trait implementations
│   │   lenovo.rs, supermicro.rs, ...
│   └── model/
│       ├── service_root.rs       # RedfishVendor enum, vendor detection
│       ├── oem/                  # Vendor-specific OEM data models
│       └── testdata/             # JSON fixtures for unit tests
├── tests/
│   ├── integration_test.rs       # Per-vendor integration tests
│   ├── mockups/<vendor>/         # Redfish JSON mockup trees
│   └── redfishMockupServer.py    # Python server for mockups

nico/
├── crates/bmc-vendor/src/lib.rs        # BMCVendor enum + string parsing
├── crates/redfish/src/libredfish/conv.rs  # bmc_vendor(): RedfishVendor → BMCVendor
├── crates/bmc-explorer/src/hw/mod.rs   # HwType enum (nv-redfish exploration)
├── crates/bmc-explorer-cli/            # Standalone live-BMC exploration tool
├── crates/api/src/state_controller/    # Vendor-specific state machine logic
└── crates/admin-cli/src/redfish/       # nico-admin-cli redfish subcommand
```

## Adding nv-redfish Quirks for Exploration and Health Monitoring

nv-redfish provides site exploration reports and is also used for health monitoring (`nico-hw-health`). If the new hardware causes failures in either path, the fix goes into nv-redfish.

1. **Add a `Platform` variant** in `nv-redfish/redfish/src/bmc_quirks.rs` if the quirk is platform-specific.

1. **Map the variant** in `BmcQuirks::new()` using the vendor string, redfish version, and product from the service root.

1. **Add quirk methods** for each workaround. Common quirks:
   - `bug_missing_root_nav_properties()` - BMC omits `Systems`/`Chassis`/`Managers` from service root
   - `expand_is_not_working_properly()` - `$expand` query parameter broken
   - `wrong_resource_status_state()` - non-standard `Status.State` enum values
   - `fw_inventory_wrong_release_date()` - invalid date formats

1. **Add OEM feature support** if needed. OEM extensions are gated behind Cargo features (`oem-ami`, `oem-dell`, `oem-hpe`, etc.) in `nv-redfish/redfish/Cargo.toml`.

## Optional BMC Mock Coverage

A BMC mock is optional but strongly recommended for a new vendor or a platform with a distinct Redfish surface because it lets CI preserve discovery, inventory, and provisioning behavior after access to the physical device is gone.

To add one:

1. Add a platform module under `crates/bmc-mock/src/hw/`, following a close existing platform such as `dell_poweredge_r750.rs` or `supermicro_gb300_nvl.rs`.

1. Register the module in `crates/bmc-mock/src/hw/mod.rs` and add a typed `HardwareType` variant in `crates/bmc-mock/src/lib.rs`.

1. Wire the variant through `crates/bmc-mock/src/machine_info.rs`: DPU count and type, vendor and product identity, Redfish version, manager, system, chassis, discovery, firmware inventory, and OEM behavior should match the live BMC responses relevant to the test.

1. Add focused tests for the behavior the hardware contribution changes, such as vendor detection, inventory, NIC-mode detection, or provisioning.
