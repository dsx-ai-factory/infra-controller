# Key Capabilities

## Hardware Readiness and Validation

NICo automates hardware onboarding before any host is made available to tenants:

- **Auto-discovery via Redfish**: Discovers BMCs over the OOB network and pairs DPUs and hosts automatically via LLDP and serial number matching.
- **SKU validation**: Confirms that each machine has the expected hardware components and flags incomplete or misconfigured hosts.
- **Hardware burn-in and testing**: Validates single- and multi-node setups, including NVLink and InfiniBand connectivity, before provisioning.
- **Firmware baseline enforcement**: Inventories UEFI and BMC firmware on ingestion and brings any out-of-baseline host into compliance before it becomes available.

## DPU Lifecycle Management

NICo manages the BlueField DPU throughout its lifecycle:

- Installs the DPU OS and provisions HBN (Host-Based Networking with Containerized Cumulus, configured via NVUE).
- Manages all DPU firmware components: BMC, NIC, UEFI, ATF.
- Runs the DPU agent, which periodically fetches desired configuration from NICo over gRPC and executes configuration instructions.
- Disables in-band BMC access from within the host, enforcing out-of-band-only management.

## Secure Multi-Tenancy and Network Isolation

NICo enforces workload isolation across all network planes without reconfiguring physical switches:

| Plane | Mechanism |
|---|---|
| Ethernet (N/S) | BlueField HBN — L3 VXLAN/EVPN, VRFs, Network Security Groups |
| InfiniBand (E/W) | UFM-based P_Key partition assignment per tenant |
| NVLink | NMX-C gRPC partition management |
| Spectrum-X (E/W) | SP-X partitioning for high-performance East-West traffic |

VPC and subnet configuration are API-driven. Tenant transitions fully clear and re-establish isolation boundaries.

## Trust and Attestation

NICo treats every host as untrustworthy by default:

- Host attestation via Measured Boot PCR checking and TPM signature verification through the TPM Manufacturer CA
- UEFI lockdown during active use, which prevents unauthorized BIOS changes while a tenant has the host
- Managed BMC credentials (per site) and UEFI credentials (per device)
- Secure erase of NVMe storage, GPU memory, and system memory between tenant uses
- Out-of-band monitoring only, as NICo never relies on in-band host reporting for security decisions

## Continuous Compliance and Firmware Control

NICo maintains a consistent hardware baseline across the fleet on an ongoing basis:

- Schedules UEFI and BMC firmware updates on healthy, unoccupied hosts.
- Continuously monitors hardware health via Redfish and the DPU agent.
- Exports metrics in Prometheus format for integration with operator monitoring stacks.
- Alerts on firmware or configuration discrepancies against the site baseline.
- Maintains a full inventory of firmware versions per machine per site.

## Flexible Deployment and Integration

- **API-first**: Provides a REST API for operator and ISV integration, a gRPC API and admin CLI for direct administration, and a debugging UI for engineering use.
- **JWT authentication**: Integrates with Keycloak and compatible IAM solutions.
- **Any OS**: Supports any operating system installable via iPXE; NICo imposes no OS requirements.
- **BYO monitoring**: Provides Prometheus metrics export, which integrates with Grafana, Loki, and OpenTelemetry-compatible stacks.
- **Kubernetes-native**: Deploys on any conformant Kubernetes v1.30+ environment
- **Broad hardware support**: Supports NVIDIA L40/L40S PCIe, HGX/DGX A100/H100/B200, GB200 NVL72, CPU-only x86, and Grace systems

## GB200 NVL72 and Rack-Scale Capabilities

For GB200 NVL72 SuperCluster deployments, NICo extends lifecycle management to the rack level:

**NICo Flow** treats racks and NVL domains as first-class management entities rather than collections of individual hosts. It sequences power operations, firmware updates, and maintenance workflows safely across rack components, preventing unsafe actions and ensuring correct ordering across dense, multi-tray systems.

**NVLink Clustering** manages NVLink domain formation, health monitoring, and partition management. NICo gates instance allocation on cluster readiness — if the NVLink fabric is not healthy or fully formed, provisioning is blocked until it is.

**Leakage Detection** integrates BMS and tray sensor signals and applies configurable, policy-based responses: graceful shutdown of affected trays, gravity-aware handling of trays below a leak source, and rack-level isolation decisions. Critical safety actions remain with BMS; NICo handles orchestration and automation for non-critical conditions.

**Domain Power Provisioning** supports Max-P and Max-Q power profiles per instance. NICo coordinates with the external power engine before and after lifecycle events (power-on, firmware updates, maintenance) to prevent unsafe power states and enable optimal power utilization across racks.
