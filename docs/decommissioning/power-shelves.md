# Decommission Managed Power Shelves

Use this workflow to return a managed power shelf to a pre-ingestion baseline. After the shelf reaches
`Decommissioning/Decommissioned`, force-delete it to remove the control-plane
records.

This procedure uses `nico-admin-cli` against the Core gRPC API.

## Prerequisites

- The site must be able to reach the managed power shelf.
- Make sure to save relevant credentials and know their factory default values before performing decommissioning in case of an error:
  - BMC credentials
  - Site-wide credentials
- The power shelf must be in the exact controller state `Ready`.
- No managed host in the same rack as the power shelf can be assigned to an instance.

## Start decommissioning

Start the asynchronous workflow with the stable power-shelf ID:

```bash
nico-admin-cli -a <api-url> power-shelf decommission <power-shelf-id>
```

**Expected result**: The command records the request and returns. The shelf
leaves `Ready` and enters `Decommissioning`.

## Monitor decommissioning

```bash
nico-admin-cli -a <api-url> power-shelf show <power-shelf-id>
```

**Expected result**: Successful decommissioning ends in `Decommissioning/Decommissioned`.
If progress stops or errors persist, refer to [Troubleshooting](index.md#troubleshooting).

## What the workflow changes

NICo performs these operations in order:

1. Suppresses Site Explorer for the shelf BMC/PMC. Site Explorer will skip this endpoint during periodic exploration to avoid instability and authentication lockout.
2. Suppresses DHCP for the shelf BMC/PMC. DHCP Request packets coming from these interface MACs are dropped.
3. Uses Redfish to factory-reset the BMC or PMC.
4. Deletes the shelf's per-device secrets and convergence records.
5. Stops in `Decommissioning/Decommissioned`.

## Resulting state

The workflow aims for the following state:

| Component | Intended state |
| --- | --- |
| Shelf BMC/PMC | Factory defaults |
| Management interface | No leases from this site's DHCP service |
| Per-shelf BMC/PMC credential | Removed from the credentials store |
| Rack power | Unchanged by decommissioning |

NICo suppresses discovery and responses from its own DHCP service for the
device's associated interfaces until you remove the suppressions with force-delete.
Endpoints can remain reachable if they use static IP addresses or receive an
address from an external DHCP server. Complete any required network changes or
physical hardware removal before force-deleting the device records.

## After decommissioning

When the shelf reaches `Decommissioning/Decommissioned`, remove its
control-plane records with the power-shelf command in
[Force-delete after decommissioning](index.md#force-delete-after-decommissioning).

If the shelf is still physically present, Site Explorer ingests it from the
reset state. If the hardware is not present, it does not come back and those
records are gone.
