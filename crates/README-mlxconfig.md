# Mellanox Hardware Configuration Management Registry

A compile-time, zero-overhead hardware configuration management system for Mellanox devices, with support for
constrained, device-specific variable registries.

## Overview

This project provides a mechanism for managing multiple hardware configuration registries with
compile-time validation and zero runtime parsing overhead. The system transforms YAML configuration files
into embedded static data structures, ensuring type safety and eliminating runtime configuration errors.

The idea is to be able to have better control over managing configuration variables on Mellanox devices,
above and beyond exec'ing `mlxconfig` commands with bash. Originally I had variable definitions hard-coded
in their own module, but after some discussion, it was decided it would be better in the long term to
support various "schemas", since some Mellanox devices may not support the same configurations that
others do.

This ultimately allows us to drive managing config on both the card and API sides, and gives us a lot
of safety + control over what we allow configuration of (and what values we can set them to).

## Architecture

The system consists of two cooperating crates:

```
┌─────────────────────┐    ┌─────────────────────┐
│  mlxconfig_variables│    │  mlxconfig_registry │
│                     │    │                     │
│  Core Types &       │◄───┤  Compile-time       │
│  Validation Logic   │    │  Registry Generation│
│                     │    │                     │
│  • MlxVariableSpec  │    │  • build.rs         │
│  • MlxConfigVariable│    │  • YAML → Rust      │
│  • Constraints      │    │  • Static embedding │
│  • Device validation│    │  • Zero-cost access │
└─────────────────────┘    └─────────────────────┘
```

## Key Features

### Compile-Time Safety

- **Impossible to ship bad configs**: Build fails on invalid YAML
- **Type validation**: All configuration fields verified at compile time
- **Constraint checking**: Device compatibility validated during build
- **No runtime surprises**: All errors caught before deployment

### Zero Runtime Overhead

- **Static data structures**: Configurations embedded in binary
- **No parsing**: Zero file I/O or YAML processing at runtime
- **Memory efficient**: No dynamic allocation for config data
- **Fast startup**: Instant access to all configuration data

### Developer Experience

- **Clean APIs**: Builder patterns for intuitive construction
- **Multiple formats**: YAML, JSON, and table output support
- **IDE support**: Full autocomplete and type checking

## Quick Start

### 1. Define Hardware Configurations (YAML)

Create registry files in `mlxconfig/registry/databases/`:

```yaml
# databases/bluefield3-registry.yaml
name: "Bluefield-3 DPU Configuration"
constraints:
  device_types: [ "Bluefield3" ]
  part_numbers: [ "900-9D3D4-00EN-HA0" ]
variables:
  - name: "cpu_frequency"
    description: "CPU frequency in MHz"
    read_only: false
    spec:
      type: "integer"
  - name: "power_mode"
    description: "Power management mode"
    read_only: false
    spec:
      type: "enum"
      config:
        options: [ "low", "medium", "high", "turbo" ]
```

### 2. Build (Validates & Embeds at Compile Time)

```bash
cd mlxconfig_registry && cargo build
# Validates YAML and generates static registries
```

### 3. Use in Applications

```rust
use mlxconfig_registry::registries;
use mlxconfig_variables::*;

// Access embedded registries (zero runtime cost)
for registry in registries::get_all() {
println ! ("Registry: {}", registry.name);

// Check device compatibility
let device = DeviceInfo::new()
.with_device_type("Bluefield3")
.with_part_number("900-9D3D4-00EN-HA0");

if registry.validate_compatibility( & device).is_valid() {
println ! ("Compatible with {} variables", registry.variables.len());
}
}
```

### 4. Use CLI Tool

```bash
cd mlxconfig_embedded && cargo run

# List available registries
cargo run -- registry list

# Show registry details
cargo run -- registry show "Bluefield-3 DPU Configuration"

# Check device compatibility
cargo run -- registry check "Bluefield-3 DPU Configuration" \
    --device-type "Bluefield3" \
    --part-number "900-9D3D4-00EN-HA0"

# Generate registry from MLX show_confs output
cargo run -- registry generate show_confs_output.txt --out-file new-registry.yaml
```

## Crate Details

### [mlxconfig/variables](mlxconfig_variables/)

**Core type definitions and validation logic**

- Defines all configuration variable types and constraints
- Provides device compatibility validation
- Includes builder patterns for easy construction
- Foundation layer consumed by other crates

### [mlxconfig/registry](mlxconfig_registry/)

**Compile-time registry generation**

- Processes YAML files during build with `build.rs`
- Validates configurations and generates static Rust code
- Embeds validated data as zero-cost static constants
- Provides runtime access functions

### [mlxconfig_embedded](mlxconfig_embedded/)

**Command-line interface and utilities**

- Comprehensive CLI for registry management
- Device compatibility checking
- Configuration format conversion (YAML ↔ JSON ↔ Table)
- MLX `show_confs` output parser

## Use Cases

### Hardware Management Applications

```rust
// Check if device supports specific configurations
let device = detect_hardware();
for registry in registries::get_all() {
if registry.validate_compatibility( & device).is_valid() {
apply_configuration(registry);
}
}
```

### Configuration Validation in CI/CD

```bash
# Validate all registry files before deployment
for file in databases/*.yaml; do
    mlx-config registry validate "$file" || exit 1
done
```

### Device Discovery and Setup

```bash
# Generate registry from hardware introspection
mlxconfig show_confs | mlx-config registry generate - --out-file detected-config.yaml
mlx-config registry validate detected-config.yaml
```

### Development and Debugging

```bash
# Inspect available configurations
mlx-config registry list
mlx-config registry show registry_name --output json | jq '.variables[].name'
```

## Benefits Over Traditional Approaches

| Traditional Config Systems | This Configuration System |
|----------------------------|---------------------------|
| Runtime YAML/JSON parsing  | Compile-time embedding    |
| Runtime validation errors  | Build-time validation     |
| File I/O overhead          | Zero runtime I/O          |
| Dynamic memory allocation  | Static data structures    |
| Configuration drift risks  | Immutable embedded data   |
| Complex error handling     | Impossible invalid states |

## Supported Variable Types

- **Simple**: `Boolean`, `Integer`, `String`, `Binary`, `Bytes`, `Array`, `Opaque`
- **Enums**: Choice from predefined options
- **Presets**: Numbered configurations (0 to max)
- **Arrays**: Fixed-size arrays of basic types
- **Complex Arrays**: Arrays of enums with size constraints

## Device Constraints

Registries can be restricted to specific hardware:

- **Device Types**: e.g., "Bluefield3", "ConnectX-7"
- **Part Numbers**: e.g., "900-9D3D4-00EN-HA0"
- **Firmware Versions**: e.g., "32.41.130"

## Development Workflow

1. **Define**: Create YAML registry files with hardware configurations
2. **Validate**: `cargo build` checks all YAML files and generates code
3. **Test**: Use CLI to inspect and validate registries
4. **Deploy**: Embedded registries provide zero-overhead runtime access
5. **Maintain**: Update YAML files and rebuild for new configurations

## Production Benefits

- **Impossible to deploy invalid configurations** (build fails)
- **Zero runtime configuration parsing overhead**
- **Type-safe access to all configuration data**
- **Automatic device compatibility checking**
- **Version control friendly YAML configuration format**
- **Self-documenting generated code**

This architecture is particularly well-suited for embedded systems, hardware management tools, and any
application where configuration reliability and performance are critical.
