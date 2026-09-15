# Decommission Managed Hosts and DPUs

Use this workflow to return a managed host, its DPUs, BMCs, and credentials to
a pre-ingestion baseline. After the host reaches
`Decommissioning/Decommissioned`, force-delete it to remove the control-plane
records.

This procedure uses `nico-admin-cli` against the Core gRPC API. REST
decommission is outside this procedure. Run it while NICo and its credentials
are available.

Related guidance:

- [Decommission NICo-managed hardware](index.md)
- [Tenant Lifecycle Cleanup](../operations/tenant-lifecycle-cleanup.md)
- [Managed Host State Diagrams](../architecture/state_machines/managedhost.md)
- [Force deleting and rebuilding NICo hosts](../playbooks/force_delete.md)

## Prerequisites

For each host:

- Release any instance and wait for the exact top-level state `Ready`. Follow
  [Tenant Lifecycle Cleanup](../operations/tenant-lifecycle-cleanup.md) and
  verify its sanitization outcome first.
- Confirm that every DPU supports BFB installation through Redfish. NICo rejects
  the request if any DPU does not support it.
- Keep the NICo API, database, credentials store, DHCP service, PXE service,
  and Redfish connectivity available until the host reaches
  `Decommissioning/Decommissioned`.
- If the host will be ingested again, retain the host BMC MAC address, chassis
  serial number, and factory-default BMC credentials for the Expected Machine
  entry.

<Warning>
Do not destroy the old credentials store first. NICo needs the current
per-device BMC credentials and the host UEFI credential version to authenticate
the reset operations. The new installation cannot recover a password known only
to the previous installation.
</Warning>

## Start decommissioning

Start the workflow with the stable host machine ID:

```bash
nico-admin-cli -a <api-url> managed-host decommission <host-machine-id>
```

**Expected result**: The command records the request and returns. The host
leaves `Ready` and enters `Decommissioning`.

## Monitor decommissioning

```bash
nico-admin-cli -a <api-url> managed-host show <host-machine-id>
```

**Expected result**: The state reaches `Decommissioning/Decommissioned`. Use
the state and handler message to identify a blocked operation. Transient
Redfish, RMS, database, or credentials-store failures usually leave the host
in the same decommissioning substate; the state controller retries on the
next iteration. Intervene when the state or handler message indicates
`manual_intervention_required` or the workflow stays blocked after retries.

## What the workflow changes

NICo performs these operations in order:

1. Suppresses Site Explorer for the host BMC and every DPU BMC.
2. Disables host BMC lockdown. NICo restarts Supermicro hosts after disabling
   lockdown; other supported vendors continue without that restart.
3. Unlocks managed SuperNICs and waits for them to report an unlocked state.
4. Resets the host BIOS/UEFI settings to factory defaults.
5. Clears the host UEFI administrator password. If the platform schedules a
   Redfish job, NICo restarts the host and waits for the job to complete.
6. Deletes DPF resources when DPF provisioned the host.
7. Installs the vanilla `preingestion.bfb` on every DPU through Redfish and
   waits for each DPU to boot. This image does not contain the NICo DPU agent,
   HBN configuration, DPU-local NICo DHCP service, MDS, Scout, or old NICo root
   CA.
8. [Suppresses DHCP](../operations/dhcp-suppression.md) for host and DPU OOB
   interfaces, performs an AC power cycle, and waits for the DHCP service to
   acknowledge the suppressions.
9. Suppresses DHCP for the host and DPU BMC interfaces.
10. Factory-resets the host BMC and every DPU BMC, then waits for DHCP
    suppression acknowledgement.
11. Deletes the old installation's per-device BMC, DPU SSH, and DPU HBN secrets,
    and removes host and DPU UEFI convergence records.
12. Stops in `Decommissioning/Decommissioned`.

## Resulting state

The workflow aims for the following state:

| Component | Intended state |
| --- | --- |
| Host BIOS/UEFI settings | Factory defaults |
| Host UEFI administrator password | Empty |
| Host and DPU BMCs | Returned from reset and accept the applicable factory credentials |
| SuperNIC lockdown | Unlocked |
| DPU operating system | Vanilla `preingestion.bfb` |
| NICo agent, HBN, DPU-local DHCP, MDS, and old NICo CA on each DPU | Absent |
| Host and DPU OOB path | Layer-2 path to the site DHCP service remains; this site's DHCP ignores those MACs |

The terminal state means NICo completed or received acceptance for every
workflow operation.

### DPU UEFI limitation

Decommissioning does not explicitly clear a DPU UEFI password. Installing the
vanilla BFB and factory-resetting the DPU BMC are the implemented reset
boundaries, but DPU UEFI password behavior can be platform-specific.

Before destroying the old credentials store, verify the behavior for the
installed DPU model. If the password persists, use the platform-supported
service procedure to reset it or retain the current value for the new site's
first credential transition.

### Credentials and identity for the new site

| Item | New-site expectation |
| --- | --- |
| Host and DPU BMC passwords | Factory credentials; the Expected Machine data and model-specific DPU factory credentials must match |
| Host UEFI password | Empty before ingestion; the new site sets its configured value |
| DPU UEFI password | Factory or platform baseline; verify manually |
| Site CA and machine certificates | Generated by the new installation |
| DPU SSH and HBN credentials | Generated by the new installation |

Do not copy the old site CA merely to accept leftover agents. The host was
sanitized when it returned to `Ready`. Certificates and controller ownership
must come from the installation that ingests the host.

The stable machine ID is derived from hardware identity and can be the same
after re-ingestion.

## After decommissioning

When the host reaches `Decommissioning/Decommissioned`, remove its
control-plane records with the host command in
[Force-delete after decommissioning](index.md#force-delete-after-decommissioning).

If the host is still physically present, Site Explorer ingests it from the
reset state. If the hardware is not present, it does not come back and those
records are gone.
