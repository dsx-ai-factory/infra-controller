# What is NICo?

NICo (NVIDIA Infra Controller) is an open source suite of microservices for site-local, zero-trust bare-metal lifecycle management. It automates hardware discovery, firmware validation, DPU provisioning, network isolation, and tenant sanitization — enabling NVIDIA Cloud Partners (NCPs) and infrastructure operators to stand up and operate AI factory-scale infrastructure.

NICo is open source under the Apache 2.0 license.

## Why NICo exists

AI factory-scale infrastructure requires rack-level management, host lifecycle automation, and network isolation that existing tools do not provide as an integrated solution. Without purpose-built infrastructure management, operators face:

- Manual hardware discovery, firmware alignment, and network setup that slow rack bringup
- No unified enforcement of workload isolation across Ethernet, InfiniBand, and NVLink
- Custom scripts for tenant sanitization, attestation, and trust re-establishment
- Firmware and configuration drift across mixed hardware generations

NICo fills this gap — it enables physical servers to behave like cloud instances: deploy, manage, and scale bare-metal infrastructure through APIs.

## What NICo does

NICo manages the full lifecycle of bare-metal hosts — from initial rack discovery through tenant provisioning, ongoing operations, and secure reuse.

Each managed host is a **host server with one or more BlueField DPUs**. The attached DPUs act as enforcement boundaries for network isolation and security; NICo provisions and manages them directly, independently of what runs on the host.

NICo's core responsibilities:

- Provision and manage DPU OS, firmware, and HBN configuration
- Maintain hardware inventory of all managed hosts
- Automate discovery, validation, and attestation via Redfish (out-of-band)
- Monitor hardware health continuously and react to health state changes
- Manage host firmware (UEFI, BMC) and enforce security lockdown
- Manage BMC and UEFI credentials per device via Redfish
- Allocate IP addresses, configure BGP routing, and manage DNS
- Enforce network isolation across Ethernet, InfiniBand, and NVLink planes
- Orchestrate host provisioning (PXE/iPXE), tenant release, and sanitization

## Architecture overview

![NICo architecture diagram](../static/nico_arch_diagram.svg)

NICo is deployed as a suite of microservices on a Kubernetes cluster co-located in the datacenter it manages. This suite of microservices forms the control plane, known as the **Site Controller**. The Kubernetes cluster requires a minimum of three nodes for high availability, and all NICo control plane services communicate over mTLS/gRPC.

### NICo Components

The green boxes in the architecture diagram are the services that NICo provides.

**Site Controller services:**

- **API Service (NICo Core)** — the central control plane and single source of truth. All other NICo services communicate with it over mTLS/gRPC. It is the only service that reads from and writes to PostgreSQL. Implements state machines for all managed resources (hosts, network segments, InfiniBand and NVLink partitions). Exposes a debug web UI on `/admin` for operators via HTTPS with OIDC authentication.
- **DHCP Server** — the central `nico-dhcp` service responds to relayed
  requests from physical networks: conventional `Underlay` endpoints such as
  host BMCs, DPU BMCs, and DPU OOB interfaces, plus zero-DPU host OS interfaces
  and optionally their host BMCs on `HostInband`. It does not serve the local
  overlay DHCP of a host with a managed DPU. The service is stateless and
  forwards requests to the API Service for IP address management.
- **PXE Service** — serves boot artifacts (iPXE scripts, cloud-init user-data, OS images) to managed hosts and DPUs over HTTP. Fetches the correct artifact for each host from the API Service via mTLS/gRPC.
- **Hardware Health** — scrapes host and DPU BMCs over Redfish HTTPS for sensor data and firmware inventory. Service-level Prometheus metrics are available on `/metrics`, and per-sensor measurements are available on `/telemetry`. The service reports health alerts to the API Service over mTLS/gRPC.
- **SSH Console Service** — maintains persistent SSH/IPMI connections to all host BMCs for serial console access. Streams console output to Loki for logging and provides live console access to tenants and administrators. Connects to the API Service via mTLS/gRPC.
- **Authoritative DNS Service** — handles DNS queries from the site controller and managed nodes. Authoritative for NICo-delegated zones. Connects to the API Service via mTLS/gRPC.
- **Recursive DNS (unbound)** — provides recursive DNS resolution to managed machines and tenant instances via the OOB network.
- **Site Agent** — maintains a Temporal connection to NICo REST (JSON API), syncing data and delegating gRPC requests to the on-site API Service. Enables NICo REST to be deployed centrally in cloud while the site controller runs on-premises.
- **JSON API (NICo REST)** — exposes NICo capabilities as a REST API for operators and ISVs. Can be deployed co-located with the site controller or centrally in cloud. Orchestrators and admins connect over HTTP/JWT. Multiple site controllers can connect to a single NICo REST deployment through their respective Site Agents.
- **Admin CLI** — command-line interface for site administrators, connecting directly to the API Service via mTLS/gRPC.

**Managed Host agents:**

- **Scout** — a temporary agent that runs on the x86 host during discovery (before a tenant is assigned). Collects hardware inventory that cannot be determined out-of-band, runs machine validation tests, and reports to the API Service via mTLS/gRPC.
- **DPU Agent** — a persistent daemon on the DPU (ARM OS). Polls the API Service every 30 seconds for desired network configuration, applies it via HBN (Host-Based Networking with Containerized Cumulus), and reports observed state back. Also manages DPU health checks, the Metadata Service, auto-updates, and hotfix deployment.
- **Metadata Service (FMDS)** — runs on the DPU. Provides tenant workloads with instance metadata (machine ID, boot info) via a local HTTP API on the host-facing interface.
- **DHCP (DPU)** — a per-host DHCP server used when the host's DPU is managed
  in DPU mode. It handles that host's requests locally so they do not reach the
  physical underlay. A zero-DPU or DPU-NIC-mode host instead uses central
  `nico-dhcp` on HostInband. See
  [IP and Network Configuration](../provisioning/ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os).

### Prerequisite Components

The white boxes in the architecture diagram are off-the-shelf services that NICo depends on but does not build. They must be deployed before NICo installation. See [Software Prerequisites](../getting-started/prerequisites/software.md) for validated versions and configuration details.

- **PostgreSQL** — stores all NICo system state in the `nico_system_nico` database. Only the API Service reads from and writes to it. The reference deployment uses the Zalando Postgres Operator with Spilo-15.
- **Vault** — provides a PKI engine for certificate issuance and a KV secrets engine for credential storage. Consumed by the API Service and credsmgr (cloud-cert-manager). Uses Kubernetes authentication to authorize NICo service accounts.
- **Temporal** — workflow orchestration engine used by NICo REST for multi-step operations (instance provisioning, reboot, release). The Site Agent connects to NICo REST through Temporal. Requires registered namespaces: `cloud`, `site`, and a per-site UUID.
- **cert-manager** — issues and rotates the TLS certificates that NICo services use for mTLS/gRPC communication. Includes approver-policy for certificate request authorization.
- **External Secrets Operator (ESO)** — syncs secrets from Vault into Kubernetes Secret objects, making credentials (database, PKI, bootstrap material) available to NICo workloads in each namespace.
- **Telemetry and Logging (Prometheus, Grafana, OpenTelemetry, Loki)** — collects metrics and logs from all NICo services and managed hosts. Prometheus scrapes Hardware Health service metrics from `/metrics` and, when enabled, per-sensor measurements from `/telemetry`. Loki aggregates logs from the SSH Console Service and DPU agents. OpenTelemetry Collector ships telemetry from both the site controller and DPUs. Optional but strongly recommended.
- **MetalLB** — provides load-balanced virtual IPs for NICo services on the Kubernetes cluster, making them reachable from the underlay network.
- **ArgoCD** — GitOps-based continuous delivery for deploying and updating NICo components. Optional.
- **NGC Registry** — NVIDIA's container registry, used to pull NICo service images during deployment and upgrades.
- **IDP (KeyCloak)** — identity provider for admin web UI authentication via OIDC. Optional.

For a deeper look at each component and the state machine design, see [Architecture: Overview and Components](../architecture/overview.md).

## Where NICo fits

NICo sits below Kubernetes and platform layers. It exposes REST and gRPC APIs that higher-level systems — BMaaS, VMaaS, orchestration engines, ISV control planes — can consume directly. It does not dictate how scheduling, tenancy policy, or workloads are managed above it.

```text
┌─────────────────────────────────────┐
│   ISV / NCP Control Plane           │
├─────────────────────────────────────┤
│   Kubernetes / BMaaS / VMaaS        │
├─────────────────────────────────────┤
│   NICo  ◄── you are here            │
├─────────────────────────────────────┤
│   BlueField DPU + Host Hardware     │
└─────────────────────────────────────┘
```

NICo is the layer that makes the hardware predictable, repeatable, and safe — so the layers above it can treat bare metal as a reliable building block.
