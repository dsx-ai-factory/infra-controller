# Decommission Managed Power Shelves

Use this workflow to return a managed power shelf BMC or power-management
controller (PMC) to its factory baseline. After the shelf reaches
`Decommissioning/Decommissioned`, force-delete it to remove the control-plane
records.

This procedure uses `nico-admin-cli` against the Core gRPC API. REST
decommission is outside this procedure.

Related guidance:

- [Decommission NICo-managed hardware](index.md)
- [Power Shelf State Diagram](../architecture/state_machines/power_shelf.md)
- [power-shelf force-delete command](../manuals/nico-admin-cli/commands/power-shelf/power-shelf-force-delete.md)
- [expected-power-shelf add command](../manuals/nico-admin-cli/commands/expected-power-shelf/expected-power-shelf-add.md)

## Prerequisites

- The power shelf must be in the exact controller state `Ready`.
- No managed host assigned to an instance can remain in the same rack.
- Keep the NICo API, database, credentials store, DHCP service, Site
  Explorer, and BMC management network available.
- If the shelf will be ingested again, record the BMC MAC, shelf serial
  number, rack ID, and factory BMC credentials needed to create the Expected
  Power Shelf.

Decommission power shelves last, after managed hosts and managed switches.

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

**Expected result**: The state reaches `Decommissioning/Decommissioned`.
Transient Redfish, database, or credentials-store failures usually leave the
power shelf in the same decommissioning substate; the state controller retries
on the next iteration. Intervene when the state or handler message indicates
`manual_intervention_required` or the workflow stays blocked after retries.
Inspect the controller outcome before intervening.

## What the workflow changes

NICo performs these operations in order:

1. Creates a Site Explorer suppression for the shelf BMC and waits for Site
   Explorer to acknowledge it.
2. Creates a [DHCP suppression](../operations/dhcp-suppression.md) for the shelf BMC.
3. Uses a direct Redfish connection to factory-reset the BMC or PMC. This
   operation does not use RMS.
4. Waits for the DHCP service to acknowledge the BMC suppression.
5. Deletes the shelf's managed BMC root credential from the old credentials
   store.
6. Deletes the BMC credential-convergence record.
7. Stops in `Decommissioning/Decommissioned`.

A successful Redfish response means that the BMC accepted the factory-reset
request. The BMC can still be restarting when the workflow advances.
Decommissioning resets management state; it does not delete the expected
inventory definition or explicitly change rack power output.

## Resulting state

The workflow aims for the following state:

| Component | Intended state |
| --- | --- |
| BMC or PMC | Factory credentials |
| Management interface | No leases from this site's DHCP service |
| Per-shelf BMC credential | Removed from the credentials store |
| Rack power | Unchanged by decommissioning |

The terminal state means NICo completed or received acceptance for every
workflow operation.

The `Decommissioned` record and its Site Explorer and DHCP suppressions remain
until you force-delete them. The database also retains the Expected Power
Shelf, interface records, state history, rack association, and metadata until
that delete. Site-wide BMC rotation targets remain in the credentials store;
only the per-shelf credential is deleted.

## After decommissioning

When the shelf reaches `Decommissioning/Decommissioned`, remove its
control-plane records with the power-shelf command in
[Force-delete after decommissioning](index.md#force-delete-after-decommissioning).

If the shelf is still physically present, Site Explorer ingests it from the
reset state. If the hardware is not present, it does not come back and those
records are gone.

Refer to the
[power-shelf force-delete command](../manuals/nico-admin-cli/commands/power-shelf/power-shelf-force-delete.md)
for the complete flag list.

## Prepare the new installation

Create the new Expected Power Shelf with values that match the reset device:

- `--bmc-mac-address` and `--shelf-serial-number` identify the shelf.
- `--bmc-username` and `--bmc-password` must be the factory credentials that
  work after the reset.
- Set `--bmc-retain-credentials true` only when the new site must keep the
  factory credential. Otherwise, Site Explorer rotates the BMC password to the
  new site's configured value.

Refer to the
[expected-power-shelf add command](../manuals/nico-admin-cli/commands/expected-power-shelf/expected-power-shelf-add.md)
for the complete interface.

## Recover a shelf after the old site is gone

If you know the current shelf BMC credentials, request the reset directly:

```bash
nico-admin-cli redfish \
  --address <power-shelf-bmc-ip> \
  --username <bmc-user> \
  --password '<current-bmc-password>' \
  bmc-reset-to-defaults
```

Wait for the BMC to restart and verify the factory login before adding the
Expected Power Shelf to the new site. If the old credential is unknown, use the
power-shelf vendor's approved recovery procedure; the new credentials store
cannot infer it.

<Warning>
Command-line passwords can be visible in shell history and process listings.
Use direct Redfish commands only in an approved recovery environment and follow
your site's secret-handling policy.
</Warning>
