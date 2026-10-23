# libmlx

Consolidated Mellanox NIC configuration, firmware, and device management library for NVIDIA NICo.

## Overview

`libmlx` provides a complete Rust toolkit for managing Mellanox NICs (BlueField-3 SuperNIC, ConnectX-7, etc.) through the `mlxconfig`, `flint`, `mlxfwreset`, and `mlxfwmanager` CLI tools. It handles device discovery, configuration management, firmware lifecycle, and device lockdown — all with compile-time registry validation and type-safe APIs.

The library is organized into these modules:

| Module | Purpose |
|---|---|
| [`variables`](#variables) | Core types, value conversion, and validation for hardware configuration |
| [`registry`](#registry) | Compile-time YAML → Rust code generation for configuration registries |
| [`runner`](#runner) | High-level wrapper around the `mlxconfig` CLI (query, set, sync, compare) |
| [`profile`](#profile) | Profile-based configuration management with YAML/JSON serialization |
| [`firmware`](#firmware) | Firmware flash, verify, and reset via `flint` and `mlxfwreset` |
| [`lockdown`](#lockdown) | Device lockdown status and `flint` command execution |
| [`device`](#device) | Device discovery, info, filtering, and reporting |
| [`embedded`](#embedded) | Example CLI tool for registry management and device operations |

## Binary Targets

The package provides the following binaries:

**Binary Targets**

| Binary | Description |
|---|---|
| `mlxconfig-device` | Device discovery and information CLI |
| `mlxconfig-embedded` | Registry management, device compatibility, and configuration generation CLI |
| `mlx-firmware-supervisor` | Linux per-PCI-slot firmware daemon and per-command subprocess supervisor; both roles use the same binary |

---

## Variables

Core type definitions and validation logic for Mellanox hardware configuration. This is the foundation layer that all other modules build on.

### Key Types

- **`MlxConfigVariable`** — A single hardware configuration parameter with name, description, read-only flag, and type spec
- **`MlxVariableSpec`** — Strongly-typed enum defining all supported variable types (boolean, integer, string, binary, bytes, array, enum, preset, and their array variants)
- **`MlxConfigValue`** — Typed value pairing a variable with its actual data, with automatic validation
- **`MlxVariableRegistry`** — Collection of related configuration variables with optional device filters
- **`MlxValueType`** — The underlying value enum (Boolean, Integer, String, Enum, arrays, etc.)

### Supported Variable Types

- **Simple**: `Boolean`, `Integer`, `String`, `Binary`, `Bytes`, `Array`, `Opaque`
- **Enum**: `Enum { options }` — choice from predefined values
- **Preset**: `Preset { max_preset }` — numbered presets (0 to max)
- **Arrays**: `BooleanArray`, `IntegerArray`, `BinaryArray`, `EnumArray` — fixed-size with sparse support

### Value Creation

The `with()` method automatically converts input types based on the variable's specification:

```rust
use libmlx::variables::variable::MlxConfigVariable;

let turbo_var = registry.get_variable("enable_turbo").unwrap();
let freq_var = registry.get_variable("cpu_frequency").unwrap();
let power_var = registry.get_variable("power_mode").unwrap();

let turbo_value = turbo_var.with(true)?;           // Boolean
let freq_value = freq_var.with(2400)?;             // Integer
let power_value = power_var.with("high")?;         // Enum (validated against options)
```

### Sparse Array Support

All array types support sparse arrays where individual indices can be `None`, enabling partial configuration updates:

```rust
// Dense array — all positions set
let dense = gpio_var.with(vec!["input", "output", "input", "output"])?;

// Sparse array — only some positions set
let sparse = gpio_var.with(vec![
    Some("input".to_string()),
    None,                          // Position 1 unset
    Some("output".to_string()),
    None,                          // Position 3 unset
])?;

// String notation: "-" or "" means None
let from_strings = gpio_var.with(vec!["input", "-", "output", "-"])?;

println!("{}", sparse); // "[input, -, output, -]"
```

### String Parsing for `mlxconfig` JSON Integration

The system natively handles string input from `mlxconfig` JSON responses:

| Variable Type | String Examples |
|---|---|
| Boolean | `"true"`, `"1"`, `"yes"`, `"on"`, `"enabled"` (case insensitive) |
| Integer | `"42"`, `"-123"`, `"0"` |
| Enum | `"high"` (validated against options) |
| Preset | `"5"` (validated against max_preset) |
| Binary/Bytes/Opaque | `"0x1a2b3c"`, `"1A2B3C"` |
| Arrays | `["true", "-", "false"]` — dash means unset |

### Builder Patterns

```rust
use libmlx::variables::variable::MlxConfigVariable;
use libmlx::variables::spec::MlxVariableSpec;

let variable = MlxConfigVariable::builder()
    .name("turbo_enabled")
    .description("Enable turbo boost mode")
    .read_only(false)
    .spec(MlxVariableSpec::builder().boolean().build())
    .build();

let enum_var = MlxConfigVariable::builder()
    .name("power_mode")
    .description("Power management setting")
    .read_only(false)
    .spec(
        MlxVariableSpec::builder()
            .enum_type()
            .with_options(vec!["low", "medium", "high"])
            .build()
    )
    .build();
```

### YAML Format

```yaml
name: "example_registry"
variables:
  - name: "cpu_frequency"
    description: "CPU frequency in MHz"
    read_only: false
    spec:
      type: "integer"
  - name: "gpio_pin_modes"
    description: "GPIO pin mode configuration"
    read_only: false
    spec:
      type: "enum_array"
      config:
        options: ["input", "output", "bidirectional"]
        size: 8
```

---

## Registry

Compile-time generation of hardware configuration registries from YAML files. During the build process, `build.rs` validates YAML configuration files from `databases/` and embeds them as static data structures, providing zero runtime parsing overhead.

### How It Works

```text
YAML Files → build.rs → Generated registries.rs → REGISTRIES constant
databases/     Validation    src/registry/          Static access
```

1. `build.rs` scans `databases/*.yaml` for configuration files
1. Each file is parsed and validated using self-contained deserialization types
1. Static Rust code is generated using builder patterns
1. Data becomes a static `REGISTRIES` constant via `once_cell::sync::Lazy`
1. Applications access pre-validated, zero-cost static data at runtime

### Usage

```rust
use libmlx::registry::registries;

// List all registries
println!("Available: {:?}", registries::list());

// Get all registries
for registry in registries::get_all() {
    println!("{}: {} variables", registry.name, registry.variables.len());
}

// Get specific registry
if let Some(registry) = registries::get("mlx_generic") {
    println!("Found: {} variables", registry.variables.len());
}

// Get registries matching a device
let matching = registries::get_registries_for_device(&device_info);
```

### Adding New Registries

1. Create `databases/my-new-registry.yaml` following the YAML format above
1. Run `cargo build` — the build script validates and generates code
1. Access via `registries::get("my-new-registry")`

### Device Filters

Registries can specify which hardware they apply to:

```yaml
name: "bluefield3_registry"
filters:
  - field: device_type
    values: ["Bluefield3"]
    match_mode: exact
  - field: part_number
    values: ["900-9D3D4-.*"]
    match_mode: regex
variables:
  # ...
```

Filter fields: `device_type`, `part_number`, `firmware_version`, `mac_address`, `description`, `pci_name`, `status`
Match modes: `regex` (default), `exact`, `prefix`

---

## Runner

High-level Rust wrapper around the `mlxconfig` CLI tool with type-safe, registry-driven configuration management.

### Quick Start

```rust
use libmlx::runner::runner::MlxConfigRunner;
use libmlx::registry::registries;

let registry = registries::get("mlx_generic").unwrap();
let runner = MlxConfigRunner::new("01:00.0".to_string(), registry.clone());

// Query all variables
let result = runner.query_all()?;
for var in &result.variables {
    println!("{}: current={}, next={}", var.name(), var.current_value, var.next_value);
}

// Set variables using the string API
runner.set(&[
    ("SRIOV_EN", true),
    ("NUM_OF_VFS", 16),
    ("INTERNAL_CPU_OFFLOAD_ENGINE", "ENABLED"),
    ("PCI_DOWNSTREAM_PORT_OWNER[0]", "HOST_1"),       // Sparse array index
])?;

// Smart sync: only changes what's different
let sync_result = runner.sync(&[("SRIOV_EN", false), ("NUM_OF_VFS", 32)])?;
println!("Changed {}/{}", sync_result.variables_changed, sync_result.variables_checked);

// Compare without making changes (dry-run analysis)
let comparison = runner.compare(&[("NUM_OF_VFS", 64)])?;
for change in &comparison.planned_changes {
    println!("Would change: {}", change.description());
}
```

### Flexible Input Types

```rust
// Queries accept multiple formats
runner.query(&["SRIOV_EN", "NUM_OF_VFS"])?;         // &[&str]
runner.query(vec!["SRIOV_EN".to_string()])?;         // Vec<String>

// Set/sync accept multiple formats
runner.set(&[("SRIOV_EN", true)])?;                  // &[(&str, T)]
runner.set(vec![config_value])?;                     // Vec<MlxConfigValue>
runner.set(vec![("SRIOV_EN".to_string(), "true".to_string())])?;  // Vec<(String, String)>
```

### Array Operations

```rust
// Complete array (must match registry-defined size)
runner.set(vec![array_var.with(vec!["HOST_0"; 16])?])?;

// Sparse array via Option<T>
let sparse = array_var.with(vec![
    Some("HOST_1".to_string()), None, None, Some("EMBEDDED_CPU".to_string()),
    None, None, None, None, None, None, None, None, None, None, None, None,
])?;
runner.set(vec![sparse])?;

// Sparse via string index syntax
runner.set(&[
    ("PCI_DOWNSTREAM_PORT_OWNER[0]", "HOST_2"),
    ("PCI_DOWNSTREAM_PORT_OWNER[15]", "EMBEDDED_CPU"),
])?;
```

### Execution Options

```rust
use libmlx::runner::exec_options::ExecOptions;
use std::time::Duration;

let options = ExecOptions::default()
    .with_timeout(Some(Duration::from_secs(60)))
    .with_retries(3)
    .with_retry_delay(Duration::from_millis(500))
    .with_max_retry_delay(Duration::from_secs(60))
    .with_retry_multiplier(2.0)
    .with_dry_run(true)
    .with_verbose(true)
    .with_confirm_destructive(true);

let runner = MlxConfigRunner::with_options("01:00.0".to_string(), registry, options);
```

### Generated Commands

```bash
# Query (arrays auto-expanded from registry)
sudo mlxconfig -d 01:00.0 -e -j /tmp/mlxconfig-runner-uuid.json q SRIOV_EN NUM_OF_VFS PCI_DOWNSTREAM_PORT_OWNER[0..15]

# Set
sudo mlxconfig -d 01:00.0 --yes set SRIOV_EN=true NUM_OF_VFS=16

# Sparse array set (only changed indices)
sudo mlxconfig -d 01:00.0 --yes set PCI_DOWNSTREAM_PORT_OWNER[0]=HOST_1 PCI_DOWNSTREAM_PORT_OWNER[15]=EMBEDDED_CPU
```

---

## Profile

Profile-based configuration management that ties together variables, registries, and the runner into a simple API for defining, comparing, and syncing expected configuration to devices.

### Profile Builder

```rust
use libmlx::profile::profile::MlxConfigProfile;
use libmlx::registry::registries;

let registry = registries::get("mlx_generic").unwrap().clone();

let profile = MlxConfigProfile::new("sriov_config", registry)
    .with_description("SR-IOV enabled configuration")
    .with("SRIOV_EN", true)?
    .with("NUM_OF_VFS", 16)?
    .with("PCI_DOWNSTREAM_PORT_OWNER", vec![
        "HOST_0", "HOST_0", "-", "-", "-", "-", "-", "-",
        "EMBEDDED_CPU", "-", "-", "-", "-", "-", "-", "HOST_1"
    ])?;

// Compare against device
let comparison = profile.compare("01:00.0", None)?;

// Sync to device
let sync_result = profile.sync("01:00.0", None)?;
```

### YAML Format

```yaml
name: "sriov_config"
registry_name: "mlx_generic"
description: "SR-IOV enabled configuration"
config:
  SRIOV_EN: true
  NUM_OF_VFS: 16
  NUM_OF_PF: 2
  INTERNAL_CPU_OFFLOAD_ENGINE: "ENABLED"
  PCI_DOWNSTREAM_PORT_OWNER:
    - "HOST_0"
    - "HOST_0"
    - "-"    # Sparse: unset positions use "-"
    - "-"
    - "-"
    - "-"
    - "-"
    - "-"
    - "EMBEDDED_CPU"
    - "-"
    - "-"
    - "-"
    - "-"
    - "-"
    - "-"
    - "HOST_1"
```

### File Operations

```rust
// Save
profile.to_yaml_file("configs/dpa.yaml")?;
profile.to_json_file("configs/dpa.json")?;

// Load
let profile = MlxConfigProfile::from_yaml_file("configs/dpu.yaml")?;
let profile = MlxConfigProfile::from_json_file("configs/dpu.json")?;

// String conversion
let yaml_string = profile.to_yaml_string()?;
let json_string = profile.to_json_string()?;
```

### Error Handling

```rust
use libmlx::profile::error::MlxProfileError;

match profile.sync("01:00.0", None) {
    Ok(result) => println!("Success: {}", result.summary()),
    Err(MlxProfileError::RegistryNotFound { registry_name }) => {
        println!("Registry '{}' not found", registry_name);
    }
    Err(MlxProfileError::VariableNotFound { variable_name, registry_name }) => {
        println!("Variable '{}' not in registry '{}'", variable_name, registry_name);
    }
    Err(e) => println!("Error: {}", e),
}
```

---

## Firmware

Firmware management for Mellanox NICs, including flash, verify, and reset operations. Supports sourcing firmware from local files, HTTPS URLs, and SSH/SCP, with optional authentication.

For read-only inspection, [`firmware::preflight`](src/firmware/preflight.rs) accepts caller-owned, digest-checked local files and compares a selected PCI device with an exact firmware target. It does not download artifacts, interpret binary configuration, change the device, or certify that flashing is safe. The module documents input requirements, query limits, and comparison errors.

### Lifecycle

The workflow executes the selected phases in this order:

1. Prepare both inputs, apply any device configuration, and burn through `flint`.
1. Request activation through `mlxfwreset`, when selected.
1. Compare the device image with the prepared image through `flint`, when selected.
1. Compare the queried version with the target, when selected.

### Firmware Sources

`FirmwareSource` supports three source types, all resolved through `from_url`:

**Firmware Sources**

| Prefix | Source | Example |
|---|---|---|
| `https://` or `http://` | HTTP download | `https://artifacts.example.com/fw/prod.bin` |
| `ssh://` | SSH transfer | `ssh://deploy@host:path/to/firmware.bin` |
| `file://` or bare path | Local file | `/opt/firmware/prod.bin` |

Each HTTP or SSH artifact transfer has one 30-minute network deadline, including connection, authentication, and the complete response. The deadline does not restart between response headers and body, and the library does not retry the transfer. Local disk writes finish outside that deadline because canceling a filesystem future does not necessarily stop its underlying write.

The following example selects remote sources and credentials:

```rust
use libmlx::firmware::source::FirmwareSource;
use libmlx::firmware::credentials::Credentials;

let source = FirmwareSource::from_url("https://artifacts.example.com/fw/prod.signed.bin")?
    .with_credentials(Credentials::bearer_token("my-token"));

let source = FirmwareSource::from_url("ssh://deploy@build-server:builds/fw/prod.signed.bin")?
    .with_credentials(Credentials::ssh_agent());
```

### Credentials

```rust
use libmlx::firmware::credentials::Credentials;

// HTTP
let bearer = Credentials::bearer_token("eyJhbGciOi...");
let basic = Credentials::basic_auth("deploy", "s3cret");
let header = Credentials::header("X-API-Key", "abc123");

// SSH
let key = Credentials::ssh_key("/home/deploy/.ssh/id_ed25519");
let agent = Credentials::ssh_agent();
```

### Supervised Firmware Operations

[`firmware::operation`](src/firmware/operation/mod.rs) owns accepted firmware work independently of Scout, a CLI invocation, or an RPC connection. Core-driven Scout firmware updates and non-dry-run `firmware flash` / `firmware flash-config` use this path. A long-lived `mlx-firmware-supervisor` daemon serves each PCI slot over private Tonic gRPC on a Unix-domain socket. Clients supply a PCI address such as `0000:4b:00.0`; `4b:00.0` means domain `0000`. The supervisor client does not resolve MST aliases; Scout resolves them when publishing its [startup inventory](../scout/README.md#supervised-nic-firmware-updates). All functions of `0000:4b:00` share ownership and a socket, but clients must use the daemon's exact execution address; another function is rejected rather than silently substituted. The protocol checks the daemon's version, slot, execution address, and boot identity, and admits only clients with the daemon's effective UID.

The daemon persists acceptance before starting work and records intent before each hardware phase. It runs the existing `FirmwareFlasher` pipeline: `Inspect`, `Prepare`, optional `Configure`, `Burn`, optional `Activate`, optional `VerifyImage`, and optional `VerifyVersion`. The receipt includes the operation ID, state, last recorded phase, partial report, and a nonsecret diagnostic. Durably recorded phase results remain available when a later phase fails, including a recorded mismatching version. A burn can succeed before its completion checkpoint is saved. If that write fails, the receipt can still show `flashed=false`; the operator must reconcile the device before another mutation. `Complete` is saved before removing prepared inputs.

An operation ID must contain 1–256 printable, non-space ASCII characters. Reuse the same ID to resolve a lost response: retrieving a receipt never restarts work, and resubmitting the identical profile and observer-exit policy returns the existing receipt. Reusing an ID with different work is rejected. After an uncertain result, read the original ID rather than inventing a replacement. Scout reads first and submits only after `NotFound`, preserving the accepted request even when Core's live profile changes or disappears. A new request with no profile records a no-work `Complete` receipt with `flashed=false`; it still passes predecessor and host-claim checks.

The typed client resolves local image, configuration, and SSH-key paths against the submitting process's working directory before submission. Remote SSH paths retain their remote-account meaning. Omitted SSH usernames use the submitter's `USER`, then `LOGNAME`, then `root`; usernames containing `@` or `:` are rejected. Omitted SSH credentials select the submitter's `$HOME/.ssh/id_rsa`, with `/root` used when `HOME` is unavailable. Explicit SSH-agent authentication and known-hosts checks use the supervisor's environment and home directory, not per-request environment forwarding. Configure that environment before launching the daemon.

Connecting and each RPC have a five-second total deadline, including channel readiness. Launch/readiness has a separate ten-second bound. Requests and responses are limited to 128 KiB. These bounds do not limit the firmware operation's lifetime; a timeout, dropped client, or closed socket does not cancel accepted work. The client does not automatically retry RPCs.

Private protocol 2 supports post-reset verification retries. The client can read and cancel receipts from a protocol-1 daemon, but refuses every submission to it because it cannot honor the retry policy. This includes resubmission through `firmware flash` or `firmware flash-config`; use `firmware status` to read an existing receipt. Upgrading the client does not replace a running daemon or replay accepted work. Protocol-1 clients reject protocol-2 daemons; use matching updated Scout or CLI binaries and helpers. Older Scouts omit the timing fields, so upgrading Core alone does not enable the retry policy.

To replace a protocol-1 daemon, pause its callers and inspect the existing receipt with the updated client. Let accepted work settle and reconcile any uncertain hardware effects. Identify the daemon serving the exact execution device and state root, send SIGTERM to that daemon only, and wait for it to exit. Preserve its journal, prepared inputs, and host claim; the next connection launches the installed matching helper against that state. Do not stop an active worker's scope or force termination to accelerate replacement. Replacing a daemon does not resolve an uncertain operation: its retained receipt and claim still block new work.

Each daemon permits eight journal RPC tasks across all connections, including tasks whose callers have disconnected. Excess requests return `RESOURCE_EXHAUSTED` without waiting for capacity. Execution progress and observer cancellation do not consume this quota. Transport limits separately allow 32 connected sockets and eight active HTTP/2 streams per connection; excess sockets are closed.

The supervisor exposes the following receipt states:

**Firmware Receipt States**

| State | Meaning |
|---|---|
| `Accepted` | The immutable request is recorded; execution has not necessarily started |
| `Running` | The daemon is inspecting, preparing, executing a phase, or waiting to retry verification |
| `CancelRequested` | A durable stop request awaits the next safe phase boundary |
| `Complete` | Every requested phase succeeded, or the request explicitly contained no firmware work |
| `Canceled` | Execution stopped at a safe boundary; completed effects were not undone |
| `Interrupted` | A previous daemon disappeared before any mutation intent was recorded; reconciliation is required |
| `ManualIntervention` | A mutation could have started, or later execution failed; automatic continuation is not justified |
| `Failed` | A known failure stopped execution; this is not permission to replay it |

`ObserverExitBehavior::Continue` is the default, used by Scout and the CLI. `Terminate` instead requests cancellation when the actual submitting process exits, using the kernel's Unix peer pidfd (`SO_PEERPIDFD`, Linux 6.5 or a supporting backport). It never treats socket closure as process death and never kills an in-flight mutation solely because the observer exited. Kernels without peer pidfds reject `Terminate` before admission but can use `Continue`. Library callers selecting `Terminate` must connect from the submitting process, not inherit another process's connection.

Explicit `cancel` also stops only at a safe phase boundary. It returns the receipt without waiting, so `CancelRequested` is not proof that a tool stopped. Pending file preparation and verification retry waits can be canceled; an active hardware command settles before the next phase is withheld. Cancellation does not undo configuration, a burn, or a reset. A request arriving during the final successful phase can still finish as `Complete`. Terminal receipts are returned unchanged; unknown IDs return an error. Per-tool deadlines and output limits remain enforced independently.

A durable host-wide mutation claim serializes cooperating supervised operations because PCI slots do not establish independent physical-card or reset domains. All callers must share the same state root. An occupied claim rejects new admission with `RESOURCE_EXHAUSTED`; Scout defers that request to a later poll without reporting a firmware failure. The claim is released after persisted `Complete`, or `Failed`/`Canceled` before any mutation intent. The terminal receipt remains immutable: its ID returns the same result, while a new ID can request new work. Interruption, uncertain persistence, and failure or cancellation after mutation retain exclusion. This does not protect direct `FirmwareFlasher` callers, standalone reset/configuration/lockdown commands, legacy firmware paths, or externally launched tools; do not run those concurrently with supervised updates.

### Launch and State

Scout and the firmware CLI read these environment variables through `SupervisorConfig::from_env`; library callers can override the resulting fields before connecting. There are no corresponding firmware CLI flags. A running daemon retains its original launch placement. The following table lists the accepted settings:

**Supervisor Environment Settings**

| Environment Variable | Default and Accepted Values |
|---|---|
| `MLX_FIRMWARE_STATE_DIRECTORY` | `/run/forge/mlx-firmware`; an absolute, nonempty shared state root other than `/`, with no `..` components |
| `MLX_FIRMWARE_SUPERVISOR_LAUNCH` | `standalone`; accepts exactly `standalone` or `systemd-scope`, with empty or unknown values rejected |

`standalone` starts the daemon in a new session without requiring systemd. A service manager's cleanup of Scout's cgroup, including after Scout exits, can still kill a standalone daemon. Optional `systemd-scope` uses `systemd-run --system --scope` in `system.slice`, outside Scout's service cgroup, and requires permission to create system scopes. Each launch gets a fresh scope name so a surviving worker cannot occupy its replacement daemon's scope. Failure never falls back to standalone. The separate scope protects against Scout's service cleanup, not system shutdown, stopping the helper scope, or external SIGKILL; it supplies no automatic restart. A readiness timeout leaves a launched daemon alive.

SIGINT or SIGTERM to the daemon stops admission and drains accepted work; it does not cancel a verification retry wait or interrupt an active burn. Drain includes the remaining requested phases, including up to 1,800 seconds of post-reset verification, plus command cleanup. Service-manager stop deadlines must allow for that drain. Forced termination after mutation leaves unfinished work requiring reconciliation when the journal is reopened.

The packaged [Scout service](../scout/misc/forge-scout.service) explicitly selects `systemd-scope`; its optional `/opt/forge/forge-scout.env` can override that setting. In Scout PXE images, `forge-scout-pre.sh` regenerates that file from `/opt/forge/forge-scout.env.template` before each start. Put persistent launch overrides in the template, for example `MLX_FIRMWARE_SUPERVISOR_LAUNCH=standalone`; edits to the generated `.env` file are discarded. This service configuration does not change the standalone binary's default.

The state root and per-slot directories require ownership by the effective UID, mode `0700`, and no symlink components. Ancestors must belong to root or that UID; shared writable ancestors must have the sticky bit. The socket is `<root>/<dddd:bb:ss>/control.sock`, mode `0600`, with a maximum full path length of 107 bytes. Launcher diagnostics are written to `<root>/<dddd:bb:ss>.supervisor.log`, mode `0600`. The default helper is the matching `mlx-firmware-supervisor` beside the caller executable, not a `PATH` lookup; library callers can set the absolute `SupervisorConfig.supervisor_path`.

Each slot retains its receipt ledger and exclusive ownership lock. The ledger pins the full execution address across daemon restarts; reopening it for another function is rejected even after successful work. Staged files live in that slot's private directory, overriding the profile's `cache_dir`; their relative paths and SHA-256 digests are recorded before mutation. Those digests identify the staged bytes, not their authenticity. Source URLs and credentials are not stored in receipts. Failed or uncertain work can retain prepared inputs. There is no automatic receipt eviction, staged-file expiry, replay, resume, or repair command. Each slot permits at most 128 receipts and a 1 MiB ledger; reaching either limit rejects new admission.

After daemon loss, reopening same-boot unfinished work records `Interrupted` or, once mutation intent exists, `ManualIntervention`. A free process lock does not prove a vendor tool stopped: its per-command helper can continue under the original deadline even after the daemon dies. The new daemon does not reattach or replay. Existing receipts remain readable, but unresolved work and a retained host claim block further admission. Do not delete state to bypass that protection; hardware and process effects require reconciliation first.

This state supports process recovery, not reboot or reinstall recovery. The default `/run` location and a PXE/tmpfs root can disappear on reboot. A surviving journal from another boot remains readable but rejects further mutation. Loss of the journal is not evidence that earlier device work completed safely.

For failed or uncertain work, inspect the receipt with [`firmware status`](#firmware-cli) and retain the journal, prepared inputs, and supervisor log. There is no supported repair or retry command for an uncertain operation. Deleting state or rebooting does not reconcile its hardware effects. Recovery across reboot is tracked in [the firmware recovery follow-up](https://github.com/dsx-ai-factory/infra-controller/issues/6677).

### FirmwareFlasher

Firmware execution and read-only preflight require Linux and the `mlx-firmware-supervisor` helper. Scout packages include it beside `forge-scout`. For the standalone CLI, build both binaries and keep them in the same directory:

```bash
cargo build -p carbide-libmlx --bin mlxconfig-embedded --bin mlx-firmware-supervisor
```

Library callers use that same sibling lookup by default. Applications that install the per-command helper elsewhere can pass `FirmwareToolExecutor::with_supervisor_path(path)` to `FirmwareFlasher::new_with_executor`, `discover_device_bounded_with_executor`, or `inspect_prepared_firmware_with_executor`. The helper is not searched for on `PATH`; a missing or non-executable helper fails before any firmware tool starts. The daemon and per-command helper are two modes of this one executable, not separate packaged binaries. A running daemon starts its workers through `/proc/self/exe`, preserving its loaded executable and worker protocol if the installed binary is replaced. New daemons and direct library callers still require a matching helper.

The kernel must enable `CONFIG_PROC_CHILDREN=y`, and `/proc` must be readable and mounted for the helper's PID namespace. The helper checks that it can open `/proc/thread-self/children` before accepting a tool request. Run this read-only check in the same runtime environment as Scout or the CLI:

```bash
cat /proc/thread-self/children
```

The output can be empty. If the entry cannot be opened, the helper reports that prerequisite and starts no firmware tool.

`FirmwareFlasher` queries the selected device and checks its part number and PSID with `mlxfwmanager`. Its device locator may be a PCI address or an MST path; the separate Scout preflight command requires a PCI address. Preparation resolves the image and optional configuration into separate owned directories and checks that both are readable, nonempty files. Legacy `FlashSpec` does not include digests; these checks do not verify image authenticity or grant permission to update hardware.

The full workflow prepares both files, applies the optional configuration, burns the image, and then runs the requested reset and verification steps. It reuses the prepared image for verification. Configuration, burn, and reset are not retried. Direct use of `FirmwareFlasher` is a caller-owned primitive: it does not provide the durable receipts or admission protection described above.

After a successful reset and its completion checkpoint, the requested verification steps share one deadline, defaulting to 1,800 seconds. Each step starts immediately when reached in phase order; an eligible unsuccessful read retries after a configurable delay, defaulting to 10 seconds. The budget includes tool-discovery probes, verification commands, and retry waits. Commands use the lesser of their normal limit or the remaining budget, and cleanup still finishes before another attempt. Image verification is not repeated after it succeeds while version verification retries. Standalone verification and verification without reset remain single-attempt.

During a retry wait, the receipt stays `Running` in its verification phase. Cancellation interrupts that wait; it does not undo completed work. Persistence failures, command-launch or tool-discovery failures, output limits, and unresolved command cleanup stop execution. Settled nonzero read exits and read deadlines, unusable query reports, and missing or mismatching versions can retry; a nonzero exit is treated as possible device unreadiness even when its underlying cause is persistent. Exhaustion records `ManualIntervention` with completed mutation evidence, the last usable observed version if one was recorded, and a diagnostic; inputs and host exclusion remain for reconciliation. A failed or uncertain reset never enters this retry loop. These limits allow time for a card to respond, but do not certify activation or host readiness.

`apply` returns a `FirmwareApplyError` containing the original cause and an optional progress report. A failure after the burn completion callback succeeds retains that report; an earlier failure has no report. For supervised work, that callback saves the burn checkpoint. A successful physical burn followed by a failed checkpoint write therefore does not guarantee a report with `flashed=true`. Requested optional steps are `Some(true)` only after successful completion and `Some(false)` when failed or unreached; unrequested steps are `None`. Version mismatches retain the actual queried version. The supervised operation layer exposes durably saved progress through its receipt; Core completion requires a matching operation ID and successful completion of the requested phases.

Load a profile and execute its requested phases as follows.

```rust
use libmlx::firmware::config::FirmwareFlasherProfile;
use libmlx::firmware::flasher::FirmwareFlasher;

let profile = FirmwareFlasherProfile::from_file("/etc/nico/firmware.toml")?;
let flasher = FirmwareFlasher::new("4b:00.0", &profile.firmware_spec).await?;
let report = flasher.apply(&profile).await?;
```

For separate preparation and burn steps, call `prepare(&profile.flash_spec).await` and then `burn(&mut prepared).await`. Keep `PreparedFirmware` through every requested reset and verification step. After all steps succeed, call `prepared.finish()` to consume it and remove its inputs; removal failures are logged. Dropping it after a real burn without calling `finish` retains the inputs, including after a failed or cancelled reset. Call `finish` after a failure only when reconciliation is complete and no command or cleanup still needs the files. `apply` handles this lifetime itself. `reset`, `reset_with_level`, and `verify_version` are also asynchronous.

Preparations live in unique `firmware-*` directories under `FlashSpec.cache_dir`, or `std::env::temp_dir()/mlxconfig-firmware` when omitted. Image and configuration files have separate subdirectories, even when their source basenames match. Standalone `verify_image` can use a legacy basename cache file when `verify_from_cache` is true; otherwise it resolves the source. Only callers or older library versions populate that cache: preparation does not write `<cache_dir>/<basename>`. A cache miss resolves the source. `apply` always verifies its own prepared image, regardless of that flag.

Each query, tool discovery probe, and configuration command has a 30-second deadline. Each burn, reset, and image verification command has a 30-minute deadline. These deadlines include all descendants and output collection. Each attempt has its own helper process, which adopts orphaned descendants and waits until the kernel reports that it has no children left. Failure cleanup has a separate shared 5-second limit. The caller allows up to 5 more seconds to receive the cleanup result; that does not extend cleanup itself. Each output stream is capped at 1 MiB; exceeding it requests termination and returns an uncertain failure. These are software operating limits, not measured hardware timings or guarantees that interrupting a burn is safe. They do not bound the entire workflow. Remote transfers have the separate per-artifact network deadline described in [Firmware Sources](#firmware-sources), and local staging writes are outside that deadline.

An error distinguishes a proven failure before tool launch from a result that is uncertain after dispatch, including a lost or malformed helper response. It preserves command diagnostics and cleanup failures. Reconcile any mutation that started or may have started before retrying. Interrupted preparation, uncertain execution, and any failure after a real burn retain staged inputs and log their directory. This includes a completed verification that finds a version mismatch. Direct callers can accumulate retained copies without automatic expiry. Successful workflows and settled failures before any mutation remove their inputs. Supervised failures retain their receipt; whether they also retain the host claim follows the mutation-intent rules in [supervised firmware operations](#supervised-firmware-operations).

Direct CLI dry-run, verification, and reset actions catch SIGINT, SIGTERM, SIGHUP, and SIGQUIT, drop the operation, and close its per-command control channel. The helper requests bounded cleanup independently, including if that direct caller crashes or is killed; cancellation does not wait for confirmed cleanup. Supervised flash commands instead stop observing and leave the accepted operation running under `Continue`. The synchronous `config-reset` command keeps its existing signal behavior. Killing a per-command helper, losing power, or terminating its entire service cgroup can prevent cleanup. No process cleanup proves that work inside the device has stopped. Cancellation uses the required proc entry to locate the helper's adopted children.

These limits apply to `FirmwareFlasher` and `discover_device_bounded`. The existing synchronous `FlintRunner` and `MlxFwResetRunner` methods, and `discover_device`/`discover_devices`, retain blocking execution without these deadlines or helper supervision.

Dry-run still checks identity and prepares inputs, but starts no mutation or verification commands. Its report contains no observed version and does not claim completed phases: `flashed` is `false`, requested optional steps are `Some(false)`, and unrequested optional steps are `None`.

### TOML Configuration

A profile identifies the target and explicitly selects optional phases. Reset and both verification options default to `false`; `reset_level` defaults to `3` and is passed unchanged to `mlxfwreset` when reset is requested. Unknown profile keys are rejected. The following example selects reset and both verification phases:

```toml
part_number = "target-part-number"
psid = "target-psid"
version = "32.43.1014"
firmware_url = "/path/to/firmware.bin"
device_conf_url = "/path/to/device.conf"
reset = true
verify_image = true
verify_version = true
```

The profile accepts these keys:

**Firmware Profile Keys**

| Key | Meaning and Default |
|---|---|
| `part_number`, `psid`, `version` | Required strings identifying the hardware and exact target version |
| `firmware_url` | Required image source, using the formats in [Firmware Sources](#firmware-sources) |
| `device_conf_url` | Optional source applied before burning; omission skips configuration apply |
| `firmware_credentials`, `device_conf_credentials` | Optional credential tables for the corresponding source; omission uses that source's default authentication |
| `cache_dir` | Optional staging parent for direct `FirmwareFlasher` use; defaults to `std::env::temp_dir()/mlxconfig-firmware`. Supervised execution uses its private slot directory instead |
| `verify_from_cache` | Defaults to `false`; only standalone image verification reads an existing legacy basename cache entry |
| `reset`, `verify_image`, `verify_version` | Optional booleans, each defaulting to `false`; select phases in that order after burn |
| `reset_level` | Integer from 0 through 255, defaulting to `3`; passed to `mlxfwreset` only when `reset` is `true`, without checking device support |
| `post_reset_verify_retry_interval_seconds` | Optional integer from 1 through 1,800 seconds; defaults to `10`. Delay after an unsuccessful post-reset verification attempt settles |
| `post_reset_verify_timeout_seconds` | Optional integer from 1 through 1,800 seconds; defaults to `1800`. Shared post-reset verification budget, including tool-discovery probes, commands, and waits |

Both timing settings are validated even when reset or verification is disabled. They never enable a phase: retry requires `reset = true` and at least one verification option. Settings are captured per operation; changing them requires a new operation after the predecessor safely completes. Omitted timing remains absent in the saved request. An explicit value equal to the default has the same timing behavior but a different request identity. Changing an accepted ID from omitted to explicit timing, or changing an explicit value, is a conflict, not reconfiguration.

Each credential table selects one `type` and its associated fields:

**Firmware Credential Types**

| Type | Fields | Source |
|---|---|---|
| `bearer_token` | Required `token` string | HTTP or HTTPS |
| `basic_auth` | Required `username` and `password` strings | HTTP or HTTPS |
| `header` | Required `name` and `value` strings | HTTP or HTTPS |
| `ssh_key` | Required `path` string; optional `passphrase` string | SSH |
| `ssh_agent` | No additional fields; uses `SSH_AUTH_SOCK` | SSH |

For example, use `[firmware_credentials]` for the image source and `[device_conf_credentials]` for the optional configuration source. Without an explicit credential table, HTTP adds no explicit credentials and SSH uses `$HOME/.ssh/id_rsa` (`/root/.ssh/id_rsa` when `HOME` is unavailable). Local sources ignore credentials. HTTP and SSH reject credentials for the other protocol when resolving the source. Unknown credential fields are rejected. `device_conf_credentials` has no effect when `device_conf_url` is omitted.

### Underlying Tools

Firmware operations use the following tools:

**Firmware Tools**

| Tool | Purpose |
|---|---|
| `flint` | Burn firmware, verify against image |
| `mlxconfig` | Apply device config, reset NV config |
| `mlxfwreset` | Device reset to activate firmware |
| `mlxfwmanager` | Query installed firmware version |

### Known Limitations

The firmware workflow has the following limitations:

- All firmware operations require root (`flint`, `mlxconfig`, and `mlxfwreset` all need root).
- SSH binary transfer uses base64 encoding; the remote host must have `base64` installed.
- SSH host key verification uses `~/.ssh/known_hosts`; use `sudo -E` to preserve `HOME` and `SSH_AUTH_SOCK`.
- Reset scope and the meaning of the reported firmware version need hardware qualification for each cohort, as tracked in [NIC Firmware Design](https://github.com/dsx-ai-factory/infra-controller/issues/6664).

---

## Lockdown

Device lockdown status checking and `flint` command execution for Mellanox NICs.

### Key Types

- **`LockStatus`** / **`StatusReport`** — Device lockdown status queried via `flint`
- **`FlintRunner`** — Wrapper around the `flint` CLI for lockdown queries and firmware operations

---

## Device

Device discovery, information, and filtering for Mellanox NICs.

### Key Types

- **`MlxDeviceInfo`** — Container for device details (type, part number, firmware version, MAC, PCI name, etc.)
- **`MlxDeviceReport`** — Full device report with discovery results
- **`DeviceFilter`** / **`DeviceFilterSet`** — Filter rules for matching devices against registries
- **`discover_device()`** — Discover Mellanox devices on the system via `mlxfwmanager`

### Protobuf Integration

Device types include `From`/`Into` conversions with `nico-rpc` protobuf types for gRPC communication.

---

## Embedded

Example CLI tool and utilities for working with the hardware configuration registry. In practice, these capabilities are embedded into `scout` (DPA management), `nico_dpu_agent` (DPU management), and `nico-api` (server-side validation).

### Commands

```bash
mlxconfig-embedded registry list                      # List all registries
mlxconfig-embedded registry show <name>               # Show registry details
mlxconfig-embedded registry show <name> --output json # JSON output
mlxconfig-embedded registry validate <file>           # Validate a YAML registry
mlxconfig-embedded registry generate <input>          # Generate YAML from show_confs output
mlxconfig-embedded registry check <name> \            # Check device compatibility
    --device-type "Bluefield3" \
    --part-number "900-9D3D4-00EN-HA0" \
    --fw-version "32.41.130"
```

### `show_confs` Parser

Converts `mlxconfig show_confs` output into YAML registry format:

```text
List of configurations:
ADVANCED CONFIG:
    CPU_FREQUENCY=<NUM> CPU frequency setting in MHz
    TURBO_MODE=<False|True> Enable or disable turbo mode
    POWER_LEVEL=<low|medium|high|turbo> Power management setting
```

Becomes:

```yaml
name: "MLX Hardware Configuration Registry"
variables:
  - name: "CPU_FREQUENCY"
    description: "CPU frequency setting in MHz"
    read_only: false
    spec:
      type: "integer"
  - name: "TURBO_MODE"
    description: "Enable or disable turbo mode"
    read_only: false
    spec:
      type: "enum"
      config:
        options: ["False", "True"]
```

### Firmware CLI

Non-dry-run `flash` and `flash-config` require `--operation-id` and a PCI address; an omitted domain means `0000`. They submit with `Continue`, poll the receipt once per second, and print the terminal receipt as JSON. Only `Complete` returns success. Reuse the same operation ID and unchanged request after a lost response; use `status` to read the receipt without resubmitting. `cancel` records a safe-boundary stop and prints the returned receipt without waiting for completion. Both receipt commands can launch a daemon to reopen its journal, but neither resumes work.

The following examples use the standalone `mlxconfig-embedded` executable. Its recovery diagnostic prints the executable path and shell-quotes the device and operation ID. Keep the same supervisor environment settings when copying that command:

```bash
mlxconfig-embedded firmware flash 0000:4b:00.0 /path/to/firmware.signed.bin \
    --part-number target-part-number --psid target-psid --version 32.43.1014 \
    --operation-id maintenance-2026-10-21
mlxconfig-embedded firmware flash-config 0000:4b:00.0 /etc/nico/firmware.toml \
    --operation-id maintenance-2026-10-22
mlxconfig-embedded firmware status -- '0000:4b:00.0' 'maintenance-2026-10-21'
mlxconfig-embedded firmware cancel -- '0000:4b:00.0' 'maintenance-2026-10-21'
```

The parent `firmware --dry-run` option keeps flash preparation local and does not admit a durable operation; an operation ID is not required. `--work-dir` controls staging for dry-run and standalone image verification and is rejected for supervised flash. `status` and `cancel` reject both parent options. Direct verification, reset, and configuration reset remain outside durable admission and host-wide exclusion:

```bash
mlxconfig-embedded firmware verify-version 4b:00.0 32.43.1014 \
    --part-number target-part-number --psid target-psid
mlxconfig-embedded firmware reset 4b:00.0 \
    --part-number target-part-number --psid target-psid --version 32.43.1014
mlxconfig-embedded firmware verify-image 4b:00.0 /path/to/firmware.signed.bin \
    --part-number target-part-number --psid target-psid --version 32.43.1014
mlxconfig-embedded firmware config-reset 4b:00.0
```

---

## Architecture

```text
┌──────────────────────────────────────────────────────────┐
│                        libmlx                            │
│                                                          │
│  ┌─────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐ │
│  │ profile │  │ firmware  │  │ lockdown │  │ embedded │ │
│  └────┬────┘  └────┬─────┘  └────┬─────┘  └────┬─────┘ │
│       │            │             │              │        │
│  ┌────▼────────────▼─────────────▼──────────────▼────┐  │
│  │                    runner                          │  │
│  └────────────────────┬──────────────────────────────┘  │
│                       │                                  │
│  ┌────────────────────▼──────────────────────────────┐  │
│  │                   registry                         │  │
│  │          (build.rs → registries.rs)                │  │
│  └────────────────────┬──────────────────────────────┘  │
│                       │                                  │
│  ┌────────────────────▼──────────┐  ┌────────────────┐  │
│  │          variables            │  │     device     │  │
│  │  (types, specs, validation)   │  │  (discovery,   │  │
│  │                               │  │   filters,     │  │
│  │                               │  │   info)        │  │
│  └───────────────────────────────┘  └────────────────┘  │
└──────────────────────────────────────────────────────────┘
                        │
                        ▼
              External CLI tools
         (mlxconfig, flint, mlxfwreset,
              mlxfwmanager)
```

### Data Flow

```text
YAML Files → build.rs → Static Registries → Runner/Profile → mlxconfig CLI → Device
databases/   Compile     registries.rs       Type-safe API    sudo commands    Hardware
```

## Requirements

- **mlxconfig**, **flint**, **mlxfwreset**, **mlxfwmanager** CLI tools installed and on `PATH`
- **sudo privileges** for hardware access
- **Rust 2021 edition** or later

## Firmware Tests

Firmware tests use fake tools but still run the compiled Linux supervisor. Build that helper before running scoped library or Scout unit tests:

```bash
cargo make build-nic-firmware-test-helper
cargo test -p carbide-libmlx --lib
cargo test -p carbide-scout --bin forge-scout firmware
```

Use the same `CARGO_TARGET_DIR` for the helper and tests. These commands use the default debug profile; custom profiles also require a helper built with the same profile. `cargo make correctly-execute-tests` builds the helper automatically through its debug test prerequisites.

Unix-socket tests need a short, private temporary path. If `TMPDIR` is too long or has untrusted ancestors, set `LIBMLX_TEST_STATE_DIRECTORY` to an existing short directory owned by the test user with mode `0700`. Its ancestors must satisfy the [state-directory trust requirements](#launch-and-state). This fixture-only setting does not change the production state root.

## Internal Dependencies

- `nico-rpc` — protobuf/gRPC type conversions
- `nico-uuid` — UUID generation
