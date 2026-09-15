# Day 0, Day 1, and Day 2 Lifecycle

NICo organizes bare-metal lifecycle management into three phases: Day 0
(bring-up), Day 1 (configuration), and Day 2 (operations).

## Day 0: Discovery, Validation, and Ingestion

Day 0 covers everything from hardware arriving in the rack to a host being
declared ready for tenant use. The design goal is zero-touch: after a host is
racked and cabled, NICo handles discovery through the provisioning-ready state.

### Hardware Discovery

NICo discovers hardware using Redfish over the out-of-band (OOB) network. The
site controller's crawler probes BMC endpoints and collects the full hardware
inventory, including CPUs, GPUs, NICs, DPUs, and storage. It links each DPU to
its host server using LLDP and serial-number matching. No manual inventory entry
is required.

### SKU Validation and Burn-In

Before ingestion, NICo validates that each machine matches its expected SKU,
flagging any missing or unexpected components. It then runs hardware and
connectivity tests, including multi-node tests for systems participating in
InfiniBand or NVLink fabrics.

### Firmware Baseline

NICo inventories UEFI and BMC firmware and updates any host that does not meet
the site's baseline before making it available. NICo automatically quarantines
hosts that it cannot bring to the baseline.

### DPU Provisioning

NICo installs the DPU OS, provisions Host-Based Networking (HBN) with
Containerized Cumulus, and configures all DPU firmware components: BMC, NIC,
UEFI, and ATF. After provisioning, the DPU agent periodically fetches the
desired configuration from NICo over gRPC and reports the applied state.

### Attestation

NICo attests each host using measured boot platform configuration register
(PCR) checks and TPM signature verification before it enters the available
pool.

### Network and IP Setup

As part of the ingestion workflow, NICo allocates and configures IP address
pools for BGP, loopback, and the host OS. NICo also configures DHCP and DNS.

## Day 1: Isolation, Lockdown, and Provisioning

Day 1 covers the configuration of isolation boundaries and the provisioning of
hosts for tenant use.

### Network Isolation

Before a host is assigned to a tenant, NICo establishes isolation across all
applicable network planes:

- **Ethernet**: BlueField HBN enforces L3 VXLAN/EVPN boundaries and per-tenant
  VRFs. This requires no leaf switch configuration changes.
- **InfiniBand**: UFM assigns P_Key partitions to the host's InfiniBand ports
  for the specific tenant.
- **NVLink**: NMX-C APIs configure GPU membership in the requested NVLink
  logical partitions.

### Host Lockdown

NICo applies UEFI lockdown to prevent unauthorized BIOS changes during tenant
use. NICo also configures BMC security settings and disables in-band
host-to-BMC communications.

### OS Provisioning

NICo coordinates the PXE/iPXE boot sequence to install the tenant's chosen OS
image. It sets the UEFI boot order, applies security settings, and hands off to
the caller after the host starts booting. Beyond the boot handoff, NICo does not
manage the installed software. OS configuration is the operator's or tenant's
responsibility.

### Instance Management

Operators define instance types, such as hardware classes for GPU node
configurations. They allocate hosts to tenants as instances through the REST
API or gRPC API. For GB200 NVL72 systems, NICo batches allocations by NVL
domain to preserve NVLink topology integrity.

## Day 2: Operations, Health, and Tenant Transitions

Day 2 covers the ongoing operation of active infrastructure and the lifecycle
between tenant uses.

### Continuous Monitoring

NICo continuously monitors hardware health through Redfish polling and DPU
agent telemetry. NICo exports metrics in Prometheus format for operator
monitoring stacks, such as Grafana, Loki, and OpenTelemetry. NICo surfaces
hardware events and health anomalies through its API and alerting integrations.

### Firmware Updates

NICo schedules UEFI and BMC firmware updates on healthy, unoccupied hosts. The
updates occur entirely out of band without disrupting active tenants. NICo
applies updates against the site baseline and tracks them in the per-machine
firmware inventory.

### Platform Upgrades

Upgrading NICo itself is also a Day 2 operation. Re-running `setup.sh` from a
newer release upgrades each component in place while preserving Vault state,
PostgreSQL data, and the site configuration. Refer to the
[Upgrading NICo](../manuals/upgrade.md) guide for the pre-upgrade checklist,
version-specific migration notes, and rollback procedure.

### Tenant Transitions (Sanitization)

When a tenant releases a host, NICo performs a full cleanup sequence before the
host re-enters the available pool. Host and platform sanitization covers:

1. Secure erase of all NVMe storage
1. GPU memory and system memory wipe
1. TPM reset
1. Re-attestation using measured boot and TPM verification
1. Firmware integrity revalidation

Network cleanup runs in the same transition and returns the host to a
tenant-free state before any reuse:

- **Tenant configuration removed.** NICo removes the prior tenant's
  configuration from the DPU data path and detaches every host interface from
  the tenant VPC/overlay. NICo returns the DPU and any DPA/Spectrum-X interfaces
  to the admin overlay. This fail-closed default prevents the host from carrying
  tenant traffic.
- **Routes withdrawn and addresses reclaimed.** When NICo tears down the tenant
  VRF, the DPU stops advertising its BGP EVPN routes. The fabric withdraws the
  routes, and NICo returns tenant IP, VNI, and DPU-loopback allocations to their
  pools.
- **DHCP and metadata redirected.** NICo reconfigures the DPU-side DHCP and
  metadata services for the post-tenant phase. The released host then boots
  the discovery image and cannot access the prior tenant's metadata.
- **Convergence gated.** NICo does not report the release complete until the
  DPU confirms that it has moved off all tenant networks. This gate prevents a
  released instance from lingering on the wire or leaking connectivity,
  addresses, or metadata to the next tenant.

For the step-by-step operator view, refer to
[Tenant Lifecycle Cleanup](../operations/tenant-lifecycle-cleanup.md#network-cleanup-between-tenants).

### Hardware Decommissioning

Decommission managed hosts, switches, and power shelves when hardware is
leaving the site or should be ingested again from a clean pre-ingestion
state. After `Decommissioning/Decommissioned`, force-delete the records from
this site or move the hardware to a new site. Refer to
[Decommission NICo-managed hardware](../decommissioning/index.md).

### Break-Fix

NICo supports directed provisioning for break-fix workflows. These workflows
include targeted provisioning to specific hosts, machine labels for tracking
repairs, and issue-reporting APIs for integration with service management
tools.

### Rack-Scale Health Response (GB200)

For GB200 NVL72 systems, NICo's rack-level management layer responds to health
signals such as leakage events, power anomalies, and NVLink fabric degradation.
Configurable policies can trigger graceful workload shutdown, rack isolation,
and recovery sequencing.
