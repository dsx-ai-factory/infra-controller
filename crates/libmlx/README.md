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

| Binary | Description |
|---|---|
| `mlxconfig-device` | Device discovery and information CLI |
| `mlxconfig-embedded` | Registry management, device compatibility, and configuration generation CLI |

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
2. Each file is parsed and validated using self-contained deserialization types
3. Static Rust code is generated using builder patterns
4. Data becomes a static `REGISTRIES` constant via `once_cell::sync::Lazy`
5. Applications access pre-validated, zero-cost static data at runtime

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
2. Run `cargo build` — the build script validates and generates code
3. Access via `registries::get("my-new-registry")`

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

For read-only inspection, [`firmware::preflight`](src/firmware/preflight.rs)
accepts caller-owned, digest-checked local files and compares a selected PCI
device with an exact firmware target. It does not download artifacts, interpret
binary configuration, change the device, or certify that flashing is safe. The
module documents input requirements, query limits, and comparison errors.

### Lifecycle

1. **Flash** — burn new firmware onto the device via `flint`
2. **Verify version** — confirm expected firmware version is staged (works pre-reset)
3. **Reset** — activate new firmware via `mlxfwreset`
4. **Verify image** — compare running firmware against a known-good image (works post-reset)

### Firmware Sources

`FirmwareSource` supports three source types, all resolved via `from_url()`:

| Prefix | Source | Example |
|---|---|---|
| `https://` | HTTP download | `https://artifacts.example.com/fw/prod.bin` |
| `ssh://` | SCP transfer | `ssh://deploy@host:path/to/firmware.bin` |
| `file://` or bare path | Local file | `/opt/firmware/prod.bin` |

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

### FirmwareFlasher

Builder-pattern orchestrator for firmware operations:

```rust
use libmlx::firmware::flasher::FirmwareFlasher;
use libmlx::firmware::source::FirmwareSource;

// Flash local firmware
let flasher = FirmwareFlasher::new("4b:00.0")
    .with_firmware(FirmwareSource::local("/path/to/prod.signed.bin"))
    .with_expected_version("32.43.1014");

let result = flasher.flash().await?;

// Verify version (works pre-reset)
flasher.verify_version()?;

// Reset to activate
flasher.reset()?;

// Verify image (works post-reset)
flasher.verify_image("/path/to/prod.signed.bin".as_ref())?;
```

#### Debug firmware with device config

Debug firmware requires a device configuration (e.g., debug token) to be applied before burning:

```rust
let flasher = FirmwareFlasher::new("4b:00.0")
    .with_firmware(
        FirmwareSource::from_url("https://artifacts.example.com/fw/debug.signed.bin")?
            .with_credentials(Credentials::bearer_token("my-token")),
    )
    .with_device_conf(
        FirmwareSource::from_url("ssh://deploy@build-server:builds/tokens/debug.conf.bin")?
            .with_credentials(Credentials::ssh_agent()),
    );

let result = flasher.flash().await?;
assert!(result.device_conf_applied);
```

### TOML Configuration

For integration with NICo API config or standalone use:

```toml
firmware_url = "https://artifacts.example.com/fw/prod-32.43.1014.signed.bin"
expected_version = "32.43.1014"

[firmware_credentials]
type = "bearer_token"
token = "eyJhbGciOi..."

device_conf_url = "ssh://deploy@build-server:builds/configs/debug.conf.bin"

[device_conf_credentials]
type = "ssh_agent"
```

```rust
let flasher = FirmwareFlasher::from_config_file("4b:00.0", "/etc/nico/firmware.toml")?;
let result = flasher.flash().await?;
```

### Underlying Tools

| Tool | Purpose |
|---|---|
| `flint` | Burn firmware, verify against image |
| `mlxconfig` | Apply device config, reset NV config |
| `mlxfwreset` | Device reset to activate firmware |
| `mlxfwmanager` | Query installed firmware version |

### Known Limitations

- All firmware operations require root (`flint`, `mlxconfig`, `mlxfwreset` all need root)
- SSH binary transfer uses base64 encoding (remote host must have `base64` installed)
- SSH host key verification uses `~/.ssh/known_hosts` — use `sudo -E` to preserve `HOME` and `SSH_AUTH_SOCK`
- `verify-image` requires a device reset first; use `verify-version` for pre-reset validation

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

```bash
mlxconfig-embedded firmware flash 4b:00.0 /path/to/firmware.signed.bin
mlxconfig-embedded firmware flash 4b:00.0 https://url/fw.bin --firmware-bearer-token "token"
mlxconfig-embedded firmware verify-version 4b:00.0 32.43.1014
mlxconfig-embedded firmware reset 4b:00.0
mlxconfig-embedded firmware verify-image 4b:00.0 /path/to/firmware.signed.bin
mlxconfig-embedded firmware config-reset 4b:00.0
mlxconfig-embedded firmware flash-config 4b:00.0 /etc/nico/firmware.toml
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

## Internal Dependencies

- `nico-rpc` — protobuf/gRPC type conversions
- `nico-uuid` — UUID generation
