# nico-ipxe-renderer

Template-based iPXE script renderer for NICo Core operating system management.

## Overview

This crate provides a flexible, template-based approach to generating iPXE boot scripts for operating systems. It supports:

- Template-based iPXE script generation
- Parameter substitution with validation
- Potential artifact caching with local URL generation enabling a future caching service
- Deterministic hashing for change/tamper detection
- Required and reserved parameter enforcement
- Support for optional parameters if needed

## Features

### Core Components

- **`IpxeScriptRenderer`**: Main trait for rendering iPXE scripts
- **`DefaultIpxeScriptRenderer`**: Default implementation with built-in templates
- **Template Management**: Support for multiple iPXE script templates
- **Artifact Caching**: Potential ocal caching of remote artifacts (kernels, initrds, images)
- **Parameter Validation**: Validates required, reserved, and optional parameters

## Usage

```rust
use nico_ipxe_renderer::{
    IpxeScriptRenderer, DefaultIpxeScriptRenderer, IpxeScript, IpxeTemplateParameter
};

// Create renderer
let renderer = DefaultIpxeScriptRenderer::new();

// Define an iPXE OS
let mut ipxeos = IpxeScript {
    id: "test-os".to_string(),
    name: "Ubuntu 22.04".to_string(),
    ipxe_template_id: "ea756ddd-add3-5e42-a202-44bfc2d5aac2".to_string(),
    parameters: vec![
        IpxeTemplateParameter {
            name: "image_url".to_string(),
            value: "http://example.com/ubuntu.qcow2".to_string(),
        },
    ],
    // ... other fields
};

// Compute hash for validation
ipxeos.hash = renderer.hash(&ipxeos);

// Render iPXE script
let reserved_params = vec![
    IpxeTemplateParameter {
        name: "base_url".to_string(),
        value: "http://pxe.local".to_string(),
    },
    IpxeTemplateParameter {
        name: "console".to_string(),
        value: "ttyS0,115200".to_string(),
    },
];

let script = renderer.render(&ipxeos, &reserved_params)?;
```

## Testing

This crate is designed to be tested independently without platform-specific dependencies:

```bash
# Run all tests
cargo test --package nico-ipxe-renderer

# Run specific test
cargo test --package nico-ipxe-renderer test_hash_computation

# Run tests matching pattern
cargo test --package nico-ipxe-renderer render

# Run with output
cargo test --package nico-ipxe-renderer -- --nocapture
```

## Template System

Parameters and Artifacts are typically used on the kernel command line in iPXE scripts.<br>
They can be defined multiple times as needed (example: console, crashkernel, ...).

### Parameters

Templates support three types of parameters:

1. **Required**: Must be provided in OS definition (e.g., `image_url`)
2. **Reserved**: Provided by nico-core at render time (e.g., `base_url`, `console`)
3. **Optional**: Extra parameters added via `{{extra}}` placeholder

### Artifacts

Artifacts represent downloadable components (kernels, initrds, images, ...):

```rust
IpxeTemplateArtifact {
    name: "kernel".to_string(),
    url: "http://example.com/vmlinuz".to_string(),
    sha: Some("sha256:abc123...".to_string()),
    cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded,
    cached_url: Some("http://pxe.local/artifacts/kernel-abc123".to_string()),
}
```

### Validation

The renderer validates:
- Reserved parameters are NOT in OS definition
- Required parameters ARE present and non-empty
- Optional parameters only allowed if template has `{{extra}}`
- Hash matches computed hash

## Design

This implementation follows the design specified in `nvmetal/designs/designs/0076-Operating-System-Management-Move-to-NICo-Core.md`.

**Usage hierarchy**:
1. Templates (currently a static list)
2. iPXE OS definitions referencing templates, providing parameters/artifacts and optional user data
3. Instances referencing iPXE OS definitions and providing optional user data
