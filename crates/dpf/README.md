# NICo DPF SDK

Rust SDK for managing NVIDIA BlueField DPUs through the DPF (DOCA Platform Framework) Kubernetes operator. The flow we implement follows the official [DPF Component Description](https://docs.nvidia.com/networking/display/dpf25101/component-description) and [DPF Overview](https://docs.nvidia.com/networking/display/dpf25101/overview).

## What is DPF?

DPF uses a dual-cluster architecture: a **Host Cluster** (provisions and manages DPUs, hosts DPU Cluster control plane) and a **DPU Cluster** (manages services on DPUs). Key components (from the [Component Description](https://docs.nvidia.com/networking/display/dpf25101/component-description)):

- **In the host cluster control plane**: BFB controller (downloads BFB from URL), DPUSet controller (creates DPU objects and manages their lifecycle), DPU controller (flashes BFB via DMS).
- **On each host node**: Node Feature Discovery (labels nodes with DPU information), DOCA Management Service (DMS, flashes BFB to DPU), Host Network Configuration (VFs, bridges, routes for host-to-DPU communication).

User-facing CRDs include: **BFB** (BlueField Boot image), **DPUFlavor** (hardware/config profile), **DPUSet** (references BFB and DPUFlavor), **DPUDevice** (physical DPU, BMC address), **DPUNode** (host with one or more DPUs). The operator creates **DPU** objects from the DPUSet and drives the lifecycle (DMS, install, reboot, Host Network Configuration, DPU cluster join).

## What This SDK Does

This SDK provides a Rust interface for NICo to interact with the DPF operator. It maps to the four provisioning user flows from the [Component Description](https://docs.nvidia.com/networking/display/dpf25101/component-description). Details: `design-docs/DPF NICo SDK - DPF background.md`.

1. **Provision a DPU** (Component Description steps 2-5 + manual discovery + external node effect and reboot):
   - Steps 2-5: `create_initialization_objects` creates a BFB or
     BlueFieldSoftware provisioning source, a DPUFlavor, and a DPUDeployment
     referencing that source and flavor.
   - Manual discovery: `register_dpu_device` and `register_dpu_node` register DPUDevice/DPUNode CRDs (NICo uses manual registration instead of NFD auto-discovery).
   - Monitor flow (per design): watcher fires **MaintenanceNeeded** (DPU in NodeEffect) -> NICo calls `release_maintenance_hold`; then **RebootRequired** -> NICo reboots the host and calls `reboot_complete`; then **Ready**.

2. **Reprovision a DPU**: Delete the DPU CR (`reprovision_dpu`), watch for the new DPU (creation timestamp after delete), then run the same monitor flow as provisioning (maintenance -> release hold; reboot -> reboot + clear annotation; ready).

3. **Update a DPU** (Component Description step 1): `update_deployment_bfb` patches the DPUDeployment BFB reference. The operator handles steps 2-5.

4. **Delete a DPU** (Component Description step 1): `force_delete_dpu`, `delete_dpu_device`, `delete_dpu_node`, and `force_delete_dpu_node` remove DPU resources. The operator handles steps 2-4.

5. **Event Watching**: Subscribes to DPF CRD events and invokes callbacks when:
   - A DPU's phase changes (`on_dpu_phase_change`)
   - A DPU enters NodeEffect and needs maintenance (`on_maintenance_needed`)
   - The operator requests a host reboot (`on_reboot_required`)
   - A DPU becomes ready (`on_dpu_ready`)
   - A DPU enters error phase (`on_error`)

## API harness CLI

The `carbide-dpf-api-harness` binary exercises the DPF SDK (same surface as NICo API) against a real DPF operator for integration testing. Build with the `driver` feature:

```bash
cargo build -p carbide-dpf --features driver --bin carbide-dpf-api-harness
```

**Main flows:**

```bash
# Full provisioning flow. --dpus is device_name:dpu_bmc_ip:serial (comma-separated for multiple).
# Optional: --services-file <PATH> for service definitions JSON.
carbide-dpf-api-harness provision --bfb-url <URL> --bmc-password <PWD> --host-bmc-ip <IP> --dpus device1:10.0.0.1:SN001

# Reprovision a single DPU: delete DPU CR, watch for new DPU (creation time after delete), then same monitor flow.
# Node name is dpu-node-<device_name>. Use --timeout for wait/monitor (default varies).
carbide-dpf-api-harness reprovision --host-bmc-ip <IP> --device-name <NAME> --timeout 600

# Clean up all DPF resources for a host (node derived from first device name)
carbide-dpf-api-harness cleanup --host-bmc-ip <IP> --dpu-device-names name1,name2

# Show current DPF resource state (optional: --host-bmc-ip to filter)
carbide-dpf-api-harness status

# Stream DPF events
carbide-dpf-api-harness watch
```

Other subcommands: `get-phase`, `force-delete-dpu`, `force-delete-node`, `delete-device`, `delete-node`, `is-reboot-required`, `clear-reboot`, `release-hold`, `update-bfb`. Run `carbide-dpf-api-harness --help` for the full list and options.

## Regenerating CRDs

This crate commits CRD YAML files in `crates/dpf/crds/`. Rust CRD bindings are generated at compile time by `build.rs` using [kopium](https://github.com/kube-rs/kopium) as a build dependency.

To refresh committed YAML inputs from NVIDIA doca-platform and verify generation:

```bash
cd crates/dpf
cargo make generate
```

`cargo make generate` removes existing YAML files in `crds/`, copies the latest CRDs from the pinned doca-platform branch, and runs `cargo check` to validate `build.rs` generation.
