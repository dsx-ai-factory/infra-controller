# Decommission Managed Switches

Use this workflow to return a managed NVIDIA switch to a pre-ingestion baseline.
After the switch reaches `Decommissioning/Decommissioned`, force-delete it to
remove the control-plane records. The workflow resets both the NVOS data plane
and the switch BMC.

This procedure uses `nico-admin-cli` against the Core gRPC API.

## Prerequisites

- The site must be able to reach the managed switch.
- Make sure to save relevant credentials and know their factory default values before performing decommissioning in case of an error:
  - BMC credentials
  - NVOS credentials
  - Site-wide credentials
- The switch must be in the exact controller state `Ready`.
- No managed host in the same rack as the switch can be assigned to an instance.
- The site must be using the RMS component-manager backend.

## Start decommissioning

Start the asynchronous workflow with the stable managed-switch ID:

```bash
nico-admin-cli -a <api-url> managed-switch decommission <switch-id>
```

**Expected result**: The command records the request and returns. The switch
leaves `Ready` and enters `Decommissioning`.

## Monitor decommissioning

```bash
nico-admin-cli -a <api-url> managed-switch show <switch-id>
```

**Expected result**: Successful decommissioning ends in `Decommissioning/Decommissioned`.
If progress stops or errors persist, refer to [Troubleshooting](index.md#troubleshooting).

## What the workflow changes

NICo performs these operations in order:

1. Suppresses Site Explorer for the switch BMC. Site Explorer will skip this endpoint during periodic exploration to avoid instability and authentication lockout.
2. Uses the RMS component-manager backend to submit and poll an NVOS factory-reset job to completion. The RMS reset wipes NVOS configuration, restarts the switch, and resets the NVOS password to factory default.
3. Suppresses DHCP for the switch's NVOS interface. DHCP Request packets coming from this interface MAC are dropped.
4. Reboots the switch.
5. Creates a DHCP suppression for the switch BMC.
6. Uses Redfish to factory-reset the switch BMC.
7. Deletes the switch's per-device secrets and convergence records.
8. Stops in `Decommissioning/Decommissioned`.

## Resulting state

The workflow aims for the following state:

| Component | Intended state |
| --- | --- |
| NVOS | Factory reset |
| Switch BMC | Factory credentials |
| BMC and NVOS interfaces | No leases from this site's DHCP service |
| Per-switch BMC and NVOS credentials | Removed from the credentials store |

NICo suppresses discovery and responses from its own DHCP service for the
device's associated interfaces until you remove the suppressions with force-delete.
Endpoints can remain reachable if they use static IP addresses or receive an
address from an external DHCP server. Complete any required network changes or
physical hardware removal before force-deleting the device records.

## After decommissioning

When the switch reaches `Decommissioning/Decommissioned`, remove its
control-plane records with the switch command in
[Force-delete after decommissioning](index.md#force-delete-after-decommissioning).

If the switch is still physically present, Site Explorer ingests it from the
reset state. If the hardware is not present, it does not come back and those
records are gone.
