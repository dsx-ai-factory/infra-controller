# Flow Component Managers

This page configures Flow’s component-manager implementations and API providers. Configure Core’s downstream hardware backends separately in [Component Manager RMS Backends](component-manager-rms.md).

## Overview

The Component Manager configuration controls:

1. Which implementation to use for each component type (compute, NVL switch, power shelf)
2. Manager behavior settings for selected implementations
3. Which API providers to enable and their client settings

Timing parameters for power control and firmware update operations are generally
configured **per-rule** via action parameters in operation rules. Manager-wide
behavior settings, such as compute power-call staggering, live under
`manager_configs`.

## Configuration Files

| File | Purpose |
|------|---------|
| `componentmanager.test.yaml` | Testing/development configuration using mock implementations |
| *(embedded)* | Service default embedded in the binary via `builtin.LoadConfig("")` |

The production config is compiled into the binary. No YAML file is needed for production
deployments. A YAML file is only required when overriding defaults (e.g., for testing).
When a YAML file path is supplied, the file is authoritative and is not merged
with the embedded defaults.

## Configuration Structure

### Component Managers

```yaml
component_managers:
  compute: <implementation>
  nvswitch: <implementation>
  powershelf: <implementation>
```

Maps each component type to its implementation. Service-loaded YAML files must
include at least one `component_managers` entry. Partial maps are supported:
missing component types are not filled from embedded defaults and remain
unconfigured until explicitly added.

Available implementations:

| Component Type | Available Implementations | Description |
|----------------|---------------------------|-------------|
| `compute` | `nicolegacy`, `nico`, `mock` | Manages compute nodes. `nico` (default) routes through Core's Component Manager dispatch (`ComponentPowerControl`, `UpdateComponentFirmware`, ...) like nvswitch and powershelf already do. `nicolegacy` calls NICo Core's machine-centric RPCs (`AdminPowerControl`, `SetFirmwareUpdateTimeWindow`, ...). See [Selecting the compute implementation](#selecting-the-compute-implementation) for the override knob. |
| `nvswitch` | `nico`, `mock` | Manages NVLink switches |
| `powershelf` | `nico`, `mock` | Manages power shelves |

### Providers

```yaml
providers:
  nico:
    timeout: "<duration>"
```

Configures API client providers. Provider configs are completed from
`component_managers` using provider defaults. If `providers` is present, entries
in that section override defaults for those providers; any required provider not
listed there is still added with its default config. `providers: {}` is
equivalent to omitting the section for provider-backed component managers.

| Provider | Used By | Description |
|----------|---------|-------------|
| `nico` | compute, nvswitch, powershelf | NICo API for component management |

### Manager Configs

```yaml
manager_configs:
  compute:
    nicolegacy:
      compute_power_delay: "<duration>"
```

Configures behavior for a selected component manager implementation. The keys
are the descriptor identity: component type, then implementation name. Entries
must match the selected `component_managers` implementation.

| Manager | Option | Type | Default | Description |
|---------|--------|------|---------|-------------|
| `compute/nicolegacy` | `compute_power_delay` | duration string | `2s` | Delay between sequential power control calls for compute trays. Prevents overwhelming the power delivery system. Set to `0s` to disable. |

#### Provider Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `timeout` | duration string | `1m` | gRPC call timeout |

Duration strings use Go format: `30s`, `1m`, `2m30s`, etc.

## Examples

### Production Configuration (embedded default)

```yaml
# Equivalent to builtin.LoadConfig("")
component_managers:
  compute: nico
  nvswitch: nico
  powershelf: nico

providers:
  nico:
    timeout: "1m"
```

### Test Configuration

```yaml
# Uses mock implementations - no external dependencies
component_managers:
  compute: mock
  nvswitch: mock
  powershelf: mock

# No providers section needed for mock implementations
```

### Mixed Configuration (e.g., partial testing)

```yaml
# Real power shelf management via NICo, mock compute/nvswitch
component_managers:
  compute: mock
  nvswitch: mock
  powershelf: nico

providers:
  nico:
    timeout: "30s"
```

## Provider Completion

Providers are automatically enabled based on the component manager implementations:

- If any component uses `nico` → NICo provider is enabled with defaults

This allows minimal configuration:

```yaml
component_managers:
  compute: nicolegacy
  nvswitch: nico
  powershelf: nico
# nico provider auto-enabled based on implementations above
```

Provider entries can override settings:

```yaml
component_managers:
  compute: nicolegacy
  nvswitch: nico
  powershelf: nico

providers:
  nico:
    timeout: "1m30s"
```

## Usage

Flow selects the base configuration in this precedence order:

1. **Command line flag**: `flow serve --component-config <path>`
2. **Environment variable**: `COMPONENT_MANAGER_CONFIG=<path>`
3. **Default**: embedded service config

### Selecting the compute implementation

Compute has two NICo-backed implementations:

| Implementation | RPC path | Notes |
|----------------|----------|-------|
| `nico` (default) | `ComponentPowerControl`, `GetComponentInventory`, `UpdateComponentFirmware`, `GetComponentFirmwareStatus` | Component Manager dispatch path, identical to `nvswitch/nico` and `powershelf/nico`. Honours `info.SubTargets` (BMC, BIOS, ...) and forwards `target_version` verbatim to Core (the SoT firmware-object identifier). Does **not** consume the firmware time window — Core dispatches immediately. Requires Core to be configured with `compute_tray_use_state_controller=true` (see `crates/component-manager/src/config.rs`). |
| `nicolegacy` | `AdminPowerControl`, `UpdatePowerOption`, `SetMachineAutoUpdate`, `SetFirmwareUpdateTimeWindow` | Legacy machine-centric path. Honours `manager_configs.compute.nicolegacy.compute_power_delay` and the legacy `start_time` / `end_time` firmware window. Opt-in via `COMPONENT_MANAGER_COMPUTE=nicolegacy`. |

To flip a deployment back to the legacy machine-centric path without shipping
a separate component manager YAML, set the `COMPONENT_MANAGER_COMPUTE`
environment variable on the Flow service:

```bash
# Use the Component Manager-based path (same as the embedded default)
COMPONENT_MANAGER_COMPUTE=nico

# Use the legacy machine-centric path
COMPONENT_MANAGER_COMPUTE=nicolegacy
```

The override is consumed by `flow serve` after the base config is
loaded and replaces only the `compute` entry in `component_managers`. An unset or whitespace-only override leaves the base selection unchanged.
An invalid value surfaces as a normal startup failure during catalog
validation. The `nicolegacy` implementation is an opt-in compatibility path for deployments using machine-centric Core RPCs.

## Timing Parameters

Power control and firmware update timing (delays, poll intervals, timeouts) are
configured **per-rule** via action parameters in operation rules, not here.

See [Operation Rules](../operations/flow/operation-rules.md) for action parameters and examples.
