# Tenant Lifecycle Cleanup

Use this workflow to release an instance, track NICo cleanup progress, and
verify that the host is ready for reuse.

When an instance is released, NICo removes the host from tenant service and
returns networking to the admin side. NICo then runs cleanup and sanitization
workflows, performs the configured trust checks, and validates the host. When
the host is eligible for allocation again, NICo returns it to `Ready`.

Refer to the following resources for more information:

- [Managed Host State Diagrams](../architecture/state_machines/managedhost.md)
- [Repair Workflows](../manuals/repair/overview.md)
- [Measured Boot Ingest Guidance](../provisioning/ingesting-hosts.md#approve-all-machines-for-ingestion)
- [Core Metrics](../observability/core_metrics.md)

## Release an Instance

Release the instance:

```bash
nicocli instance delete <instance-id>
```

In TUI mode:

```text
nicocli tui
> instance delete
```

Instance deletion triggers the same cleanup and sanitization workflow described
on this page. Track the REST-side instance lifecycle with:

```bash
nicocli instance status-history <instance-id>
nicocli instance get <instance-id>
```

`nico-admin-cli` can also release by instance ID or by machine ID when a
Core gRPC operation is required:

```bash
nico-admin-cli -a <core-api-url> instance release --instance <instance-id>
nico-admin-cli -a <core-api-url> instance release --machine <machine-id>
```

`<core-api-url>` is the NICo Core gRPC API endpoint used by
`nico-admin-cli`. REST and `nicocli` commands use the REST API base URL from
the `nicocli` config.

To report a hardware, network, performance, or other issue during release,
refer to [Repair Workflows](../manuals/repair/overview.md).

When the release request is accepted, cleanup is asynchronous. Track the
instance lifecycle first, then inspect the managed-host state when site-level
cleanup detail is needed.

## Cleanup Flow

NICo drives tenant cleanup through the managed-host state machine. The normal
release-to-ready flow is:

```text
Assigned/BootingWithDiscoveryImage
Assigned/SwitchToAdminNetwork
Assigned/WaitingForNetworkReconfig
PostAssignedMeasuring/WaitingForMeasurements   (when attestation is enabled)
WaitingForCleanup/Init
WaitingForCleanup/SecureEraseBoss              (Dell BOSS platforms)
WaitingForCleanup/HostCleanup
WaitingForCleanup/CreateBossVolume             (Dell BOSS platforms)
BomValidating/UpdatingInventory
Ready
```

If attestation is disabled, NICo moves from
`Assigned/WaitingForNetworkReconfig` directly into `WaitingForCleanup/Init`.

During the flow, NICo:

1. Reboots the host into the discovery image used by Scout.
1. Switches DPU and DPA networking back to the admin network.
1. Waits for network configuration, extension services, and cleanup-related
   health reports to converge.
1. Deletes the instance record and releases tenant network resources.
1. Runs measured boot or attestation checks when configured.
1. Runs storage, memory-overwrite, and InfiniBand cleanup from Scout.
1. Applies Redfish power control where needed to complete cleanup and pending
   platform changes.
1. Validates inventory before returning the host to `Ready`.

## Network Cleanup (Between Tenants)

Before a released host re-enters the available pool, NICo removes all tenant
networking from the host path and returns the host to a tenant-free state. This
runs in the `Assigned/SwitchToAdminNetwork` and
`Assigned/WaitingForNetworkReconfig` states, ahead of storage and platform
sanitization.

NICo drives DPU networking declaratively. Roughly every 30 seconds, the DPU
agent polls NICo for the managed host's desired network configuration and
applies it to HBN through NVUE. As part of the same update, the agent
reconfigures the DPU-side DHCP server. The agent then reports the applied
configuration version and health to NICo.

On release, NICo sets `use_admin_network` on the managed-host configuration and
increments its version. On its next poll, the DPU re-renders without the tenant
configuration. NICo advances the release only after the DPU reports that it
applied the new configuration and is healthy.

Operators can rely on and verify the following network cleanup behaviors:

**Remove prior tenant configuration from the DPU/host path.** NICo sets
`use_admin_network` and bumps the managed-host config version. On its next
poll, the DPU agent re-renders HBN/NVUE without the tenant interface
configuration. The removed configuration includes VPC VRF membership, SVI,
VNI, tenant prefixes, and any Network Security Group ACLs. The agent also
reconfigures the DPU-side DHCP server. The transition does not complete until
the DPU confirms the applied version and reports healthy.

**Detach the host from the tenant overlay/VPC.** NICo moves every host
interface out of the tenant VPC's VRF and into the admin VRF. NICo tears down
the per-VPC VRF, its link-net/SVI addressing, and the per-VPC DPU loopback used
as the EVPN next-hop. NICo returns the instance's tenant IPs and the per-VPC DPU
loopback to their pools.

**Revert the host to the admin or isolated network state.** With
`use_admin_network` set, the DPU places all host interfaces on the admin
overlay. This overlay is a NICo-owned VPC/VRF that is separate from every
tenant VPC, so the host cannot exchange traffic with any tenant instance. If
NICo has no configuration for the DPU, the DPU enters isolated mode and
detaches every interface from any overlay (fail-closed). This can occur when
the host is unknown and the lookup returns `NOT_FOUND`. On transient errors,
the DPU keeps its last-known configuration.

**Reconfigure DHCP behavior on the DPU.** As part of the same configuration
update, the DPU agent reconfigures the DPU-side DHCP server. The server provides
the admin interface's DHCP parameters instead of the tenant's parameters. These
parameters include the server address, NTP, nameservers, and MTU. The released
host—now booting the discovery image—receives an admin/discovery lease. Because
the instance is no longer assigned, NICo also serves the host's discovery
cloud-init rather than the prior tenant's instance metadata.

**Withdraw old routes and BGP configuration.** With the tenant VRF removed,
the DPU stops advertising the tenant's host routes as BGP EVPN type-5 prefixes.
These prefixes are tagged with the VPC's route-target. The fabric withdraws the
prefixes, and every other DPU that imports the route-target drops them from its
copy of the VPC VRF. The DPU also removes the host-to-DPU BGP session in the
tenant VRF, per-interface routing policy, and any routes previously leaked to
the underlay.

**Prevent unknown or released hosts from using network resources.** The
release flow is convergence-gated. The state machine blocks until either the
DPU confirms that all tenant interfaces have moved onto the admin overlay or
the machine carries a health alert that prevents reuse. This gate prevents a
released instance from lingering as a "ghost instance." NICo returns tenant IP,
VNI, and DPU-loopback allocations to their pools and deletes the instance
record. Force-delete tears down the same fabric state through the same APIs.

**Prevent cross-tenant leakage of IPs, connectivity, or metadata.** NICo does
not report the instance as deleted until every fabric—Ethernet, InfiniBand, and
NVLink—reports that the host has been removed from all tenant partitions.
Addresses and VNIs return to their pools only after teardown. The released
instance's DNS records are also removed. These actions prevent the next tenant
from inheriting a live address or reaching the prior tenant. The DPU metadata
service is reconfigured for the post-tenant phase so that a later occupant
cannot read the prior tenant's metadata or identity.

Verify network cleanup from the per-host cleanup state and DPU/machine network
status:

```bash
# Per-host cleanup state (includes the network-reconfig states)
nico-admin-cli -a <core-api-url> managed-host show <machine-id>

# DPU / machine network status (admin vs tenant, per-fabric sync state)
nico-admin-cli -a <core-api-url> machine network status --extended
nico-admin-cli -a <core-api-url> dpu network status --extended
```

A host stuck in `Assigned/SwitchToAdminNetwork` or
`Assigned/WaitingForNetworkReconfig` has not yet converged onto the admin
network. Check DPU agent status, DPA status, admin-network config generation,
and any cleanup-related health reports. Refer to
[Troubleshooting Stuck Cleanup](#troubleshooting-stuck-cleanup) for the
per-state checks.

For the isolation model that supports these guarantees, refer to
[Default Isolation: The Admin Overlay](../manuals/vpc/vpc_network_virtualization.md#default-isolation-the-admin-overlay)
and [Network Isolation](../manuals/network_isolation.md).

## Track Progress

Use two layers of inspection:

| Layer | Tool | Use |
|---|---|---|
| REST tenant and provider lifecycle | `nicocli` | Instance deletion, instance status, status history, and tenant-visible errors. |
| Core site cleanup lifecycle | `nico-admin-cli` | Managed-host state, machine state history, health reports, measured boot, and cleanup-specific debugging. |

Start with the REST-side instance status:

```bash
nicocli instance status-history <instance-id>
nicocli instance get <instance-id>
```

If cleanup progress is unclear from the instance lifecycle, check the
managed-host state:

```bash
nico-admin-cli -a <core-api-url> managed-host show <machine-id>
```

Check the machine view for state history and platform details:

```bash
nico-admin-cli -a <core-api-url> machine show <machine-id>
```

Check health reports when cleanup appears blocked:

```bash
nico-admin-cli -a <core-api-url> machine health-report show <machine-id>
```

### Happy Path Verification

Verify a normal release with this sequence:

```bash
nicocli instance delete <instance-id>
nicocli instance status-history <instance-id>
nico-admin-cli -a <core-api-url> managed-host show <machine-id>
nico-admin-cli -a <core-api-url> machine health-report show <machine-id>
```

Success indicators:

- The instance moves through deletion or termination from the REST perspective.
- The managed host progresses through the cleanup states and reaches `Ready`.
- Cleanup-related health reports are clear.
- No blocking health report prevents allocation.

Useful metrics for fleet-level monitoring include:

| Metric | Use |
|---|---|
| `carbide_machines_per_state` | Count machines in each managed-host state. |
| `carbide_machines_per_state_above_sla` | Find machines that have remained in a state longer than the state-machine SLA. |
| `carbide_machines_time_in_state_seconds` | Review time spent in each state. |
| `carbide_reboot_attempts_in_booting_with_discovery_image` | Detect hosts that require repeated discovery-image reboots. |
| `carbide_measured_boot_machines_per_machine_state_total` | Review measured boot machine state coverage. |
| `carbide_pending_host_firmware_update_count` | Count hosts that need host firmware updates. |
| `carbide_pending_dpu_nic_firmware_update_count` | Count DPUs that need NIC firmware updates. |
| `carbide_active_host_firmware_update_count` | Count hosts actively updating firmware. |
| `carbide_running_dpu_updates_count` | Count DPUs actively updating firmware. |

## Sanitization Steps

Scout reports cleanup through `CleanupMachineCompleted`. The cleanup report can
include these step results:

| Field | Meaning |
|---|---|
| `nvme` | NVMe cleanup result. |
| `hdd` | HDD/SAS block-device cleanup result. |
| `ram` | RAM cleanup result, when present. |
| `mem_overwrite` | UEFI `MemoryOverwriteRequestControl` validation result. |
| `ib` | InfiniBand cleanup result. |

Each step has a result and a message. A failed NVMe cleanup moves the host to an
`NVMECleanFailed` failure state and keeps the host out of `Ready`.

### NVMe Secure Erase

Scout discovers NVMe controller devices and formats each namespace with secure
erase:

```bash
nvme format <controller-device> -s2 -f -n <namespace-id>
```

When namespace management is supported, Scout deletes existing namespaces after
format, creates a replacement namespace sized from controller capacity, and
attaches it to the controller.

On supported Lenovo M.2 NVMe 2-Bay RAID Kit systems, Scout uses `mnv_cli` to
remove RAID virtual disks and send NVMe passthrough cleanup commands to the
underlying disks.

### HDD and SAS Cleanup

Scout also reports an `hdd` cleanup result for HDD/SAS block-device cleanup.
Treat a failed `hdd` result the same way as other cleanup failures: keep the
host out of allocation until the failure is remediated and the cleanup path
completes successfully.

### Memory Overwrite

Scout validates the UEFI memory-overwrite control variable:

```text
MemoryOverwriteRequestControl-e20939be-32d4-41be-a150-897f85d49829
```

The `mem_overwrite` cleanup step passes when the variable is set to `1`. If site
policy requires a manual volatile-memory procedure, such as a full AC drain,
complete that procedure before returning the host to allocation.

### Dell BOSS Cleanup

On supported Dell platforms with a BOSS controller, NICo performs additional
storage cleanup:

1. Disable iDRAC lockdown for the storage operation.
1. Decommission the BOSS storage controller through Redfish.
1. Wait for the Redfish job to complete.
1. Run Scout host cleanup.
1. Recreate the BOSS virtual disk as `VD_0`.
1. Re-enable host lockdown.
1. Continue to post-cleanup validation.

If the Redfish job fails, NICo retries the job path and, if required, power-cycles the host as part of the recovery loop.

### InfiniBand Cleanup

Scout reports InfiniBand cleanup through the `ib` cleanup step. NICo also uses
cleanup-related health reports, including `IbCleanupPending`, to prevent the
state machine from advancing before InfiniBand cleanup has cleared.

## Platform Reset and Trust Controls

Tenant cleanup includes platform and trust controls that run through Redfish,
firmware management, measured boot, and site policy.

| Control | How to Verify |
|---|---|
| Redfish power control | The state machine uses `ForceRestart` during cleanup and after Scout cleanup completion. Redfish `ForceRestart` is also the reset type used to apply pending BIOS or UEFI changes. |
| TPM clear | NICo includes vendor-specific Redfish support for TPM clear. Verify completion through the platform-specific cleanup evidence used by the site. |
| BIOS recommit | Verify that pending BIOS or UEFI settings have been applied after the cleanup `ForceRestart` path. |
| DPU restricted mode and BMC in-band restrictions | Verify that tenant-side network configuration has been removed, admin-network configuration has synced, and platform lockdown settings are in the expected post-cleanup state. |
| Firmware default version | Verify that host and DPU firmware match the configured site default or are under an approved firmware update workflow. |
| Measured boot | Verify measured boot state when attestation is enabled. Measured boot can be configured in permissive mode; in that mode, use measurement results as cleanup evidence according to site policy. |

Useful attestation commands include:

```bash
nico-admin-cli -a <core-api-url> attestation measured-boot machine show <machine-id>
nico-admin-cli -a <core-api-url> att mb machine show <machine-id>
```

## Return-to-Pool Checklist

A released host is ready for reuse when all required gates pass:

- The prior instance is released and no longer active.
- Tenant VPC prefix segments and DPU loopback IP allocations are released.
- DPU and DPA networking have returned to the admin network.
- Extension services from the prior tenant have terminated.
- Scout cleanup has completed.
- NVMe and HDD/SAS cleanup have succeeded, or an approved exception exists.
- The memory-overwrite check has passed, and any required manual
  volatile-memory procedure is complete.
- InfiniBand cleanup has completed and blocking cleanup health reports are
  clear.
- TPM, BIOS/UEFI, lockdown, and firmware checks satisfy site policy.
- Measured boot or attestation checks satisfy site policy.
- Inventory validation has completed.
- The managed host is in `Ready`.
- No blocking health report prevents allocation.

## Troubleshooting Stuck Cleanup

Use the current managed-host state to choose the next check.

Start with the REST lifecycle:

```bash
nicocli instance status-history <instance-id>
nicocli instance list --status error --output table
```

If the REST lifecycle does not explain the stall, inspect the Core cleanup
state:

```bash
nico-admin-cli -a <core-api-url> managed-host show <machine-id>
nico-admin-cli -a <core-api-url> machine health-report show <machine-id>
```

| State | What It Means | Checks |
|---|---|---|
| `Assigned/BootingWithDiscoveryImage` | The host is rebooting into the discovery image. | Check BMC reachability, host power state, boot order, and repeated reboot metrics. |
| `Assigned/SwitchToAdminNetwork` | NICo is moving the host out of tenant networking. | Check DPU agent status, DPA status, and admin-network config generation. |
| `Assigned/WaitingForNetworkReconfig` | NICo is waiting for network configuration to converge. | Check DPU sync, DPA sync, extension-service termination, and cleanup-related health reports. |
| `PostAssignedMeasuring/WaitingForMeasurements` | Attestation is enabled and NICo is waiting for measurements. | Check measured boot machine state, trusted profile or bundle status, and site policy for permissive mode. |
| `WaitingForCleanup/SecureEraseBoss` | NICo is decommissioning Dell BOSS storage. | Check iDRAC lockdown state, Redfish job status, and BOSS controller reachability. |
| `WaitingForCleanup/HostCleanup` | NICo is waiting for Scout cleanup completion. | Check Scout logs, cleanup report submission, NVMe/HDD cleanup, memory-overwrite result, and InfiniBand cleanup result. |
| `WaitingForCleanup/CreateBossVolume` | NICo is recreating the Dell BOSS virtual disk. | Check Redfish job status and confirm the recreated volume is `VD_0`. |
| `BomValidating/UpdatingInventory` | Cleanup completed and NICo is validating inventory. | Check BMC reachability, inventory collection, firmware update status, and blocking health reports. |
| `Failed` with `NVMECleanFailed` | Storage cleanup failed. | Keep the host out of allocation, inspect the cleanup error message, remediate the storage issue, and rerun the approved cleanup recovery path. |

For log review, start with the NICo API or state-controller logs, Scout cleanup
logs, DPU agent logs, hardware-health logs, and Redfish job status from the BMC.

## Manual Procedures

Some environments require additional manual assurance before a host is reused.
Apply these only when required by site policy:

- Full AC drain for volatile-memory handling.
- Firmware bundle reflash.
- Manual TPM clear if automated platform cleanup is unavailable.
- Manual firmware remediation when a host or DPU does not match the configured
  site default.

Record the completed procedure, the target machine ID, the reason, the operator,
and the evidence used to approve return to allocation.
