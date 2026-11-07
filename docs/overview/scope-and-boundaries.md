# Scope and Boundaries

NICo manages the infrastructure lifecycle layer: hardware discovery, firmware management, DPU provisioning, network isolation, and tenant sanitization. Everything above the boot handoff and below the physical network underlay is outside NICo's scope.

This page defines the boundaries between NICo and the platform, orchestration, or automation layer that sits above it — so operators and integrators know what NICo handles and what their software needs to cover.

## Host OS and Application Layer

| NICo handles | Your platform handles |
|---|---|
| OS image delivery via PXE/iPXE | OS patching, upgrades, and runtime configuration |
| UEFI boot order and secure boot settings | Application software (Kubernetes, SLURM, storage) |
| Host attestation on ingestion and between tenants | In-band monitoring and agents during tenant use |

NICo provisions the OS and hands off at boot. It runs no agents or daemons inside the host OS during tenant use, and does not attest hosts continuously — attestation occurs on ingestion and between tenant transitions.

## Cluster Assembly and Workload Scheduling

| NICo handles | Your platform handles |
|---|---|
| Individual host provisioning and lifecycle | Assembling hosts into clusters (SLURM, K8s) |
| Instance type definitions and host allocation via API | Workload scheduling and resource allocation |
| Network isolation per tenant | Cluster networking (CNI, service mesh) |

NICo provisions hosts and assigns them to tenants as instances via API. Building those hosts into a functioning cluster — installing Kubernetes, configuring SLURM, deploying workloads — is handled by the ISV control plane, BMaaS layer, or orchestration system consuming NICo's APIs.

## Network Underlay

| NICo handles | Your network team handles |
|---|---|
| Tenant isolation at the DPU layer (Ethernet via HBN) | Leaf switch, spine switch, and router configuration |
| InfiniBand partition assignment via UFM APIs | UFM deployment and management |
| NVLink partition management via NMX-C APIs | NMX-C deployment and management |
| BGP route advertisement from DPUs | Physical underlay design and cabling |

NICo enforces isolation without reconfiguring physical switches — the underlay is expected to be stable and pre-configured. NICo does not install or manage Cumulus Linux on switches, UFM, or network observability tools such as NetQ.

## External Dependencies

NICo depends on several services that must be pre-deployed and managed externally. NICo coordinates with these services but does not install, configure, or operate them. For the full list with configuration details, see [Prerequisite Components](what-is-nico.md#prerequisite-components).
