# Decommission NICo-managed hardware

Use this workflow to return managed hardware to a factory or pre-ingestion
baseline while NICo still has credentials and management access. Use it when
you intend one of these outcomes:

1. The hardware is permanently leaving the site, either because the site is
   being torn down or the device is leaving service.
2. The hardware should be ingested again from a clean pre-ingestion state.

After the controller reaches `Decommissioning/Decommissioned`, remove the
hardware from this site by force-deleting it, or move it to a new site. If
you force-delete and the hardware is still physically present, Site Explorer
ingests it again. If the hardware is not present, it does not return and the
control-plane records are removed.

Decommissioning is the graceful device-cleanup step before you force-delete
or move the hardware.

This procedure documents the Core `nico-admin-cli` workflow against the NICo
gRPC API. REST decommission endpoints are outside this procedure.

<Warning>
Run decommissioning while the current NICo API, database, credentials store,
DHCP service, PXE service, and required management backends are available. If
you destroy the control plane first, the credentials and state needed to reset
the devices can be unrecoverable.
</Warning>

Related guidance:

- [Decommission Managed Hosts and DPUs](hosts.md)
- [Decommission Managed Switches](switches.md)
- [Decommission Managed Power Shelves](power-shelves.md)
- [Rack-Scale Decommissioning](racks.md)
- [Tenant Lifecycle Cleanup](../operations/tenant-lifecycle-cleanup.md)
- [Force deleting and rebuilding NICo hosts](../playbooks/force_delete.md)

## Prerequisites

- Configure `nico-admin-cli` with the Core API URL. Refer to
  [Connecting to nico-api](../manuals/nico-admin-cli.md#connecting-to-nico-api).
- Keep the NICo API, database, credentials store, DHCP service, PXE service,
  and required management backends available until each device reaches
  `Decommissioning/Decommissioned`.
- Release assigned instances and wait for each host to reach the exact
  top-level state `Ready` before you start. Host machines are already
  sanitized while returning from Assigned to Ready. Switch and power-shelf
  decommissioning reject the request while a managed host in the same rack
  remains assigned.

## Choose a procedure

- [Decommission Managed Hosts and DPUs](hosts.md): reset host firmware configuration,
  SuperNIC lockdown, DPU images, host and DPU BMCs, and managed credentials.
- [Decommission Managed Switches](switches.md): factory-reset NVOS and the switch BMC, then
  remove managed NVOS and BMC credentials.
- [Decommission Managed Power Shelves](power-shelves.md): factory-reset the shelf BMC or
  PMC, then remove its managed BMC credential.

## Decommission a rack

Decommission each component with `nico-admin-cli`. Use this order so network
and rack power management stay available while compute devices reset:

1. Decommission every managed host and wait for all of them to reach
   `Decommissioning/Decommissioned`.
2. Decommission every managed switch and wait for all of them to reach
   `Decommissioning/Decommissioned`.
3. Decommission every power shelf and wait for all of them to reach
   `Decommissioning/Decommissioned`.

**Expected result**: Every in-scope device is in `Decommissioning/Decommissioned`.
A transient failure on one device usually retries automatically in the same
decommissioning substate. Intervene when monitoring shows
`manual_intervention_required` or a persistent handler error on that device.

After those steps finish, remove the hardware from this site with
[force-delete](#force-delete-after-decommissioning), or move it to a new
site.

## Understand the terminal state

`Decommissioning/Decommissioned` is a terminal controller state. It does not
delete the object. Site Explorer and DHCP suppressions remain until you
force-delete them, so the same installation does not immediately ingest a
reset device.

After [DHCP is suppressed](../operations/dhcp-suppression.md), the BMCs and OOB
interfaces become unreachable because the DHCP server ignores requests from
those MAC addresses. Redfish and RMS reset calls return after the request is
accepted, and NICo cannot poll the hardware to completion.

## Force-delete after decommissioning

Use these commands when you are removing decommissioned hardware from this
site. They are the canonical flag sets for this workflow. They remove
interfaces, suppressions, and retained boot entries where those exist.

Host:

```bash
nico-admin-cli -a <api-url> machine force-delete \
  --machine <host-machine-id> \
  --delete-interfaces \
  --delete-bmc-interfaces \
  --delete-bmc-suppressions \
  --delete-retained-boot-interfaces
```

Switch:

```bash
nico-admin-cli -a <api-url> switch force-delete \
  <switch-id> \
  --delete-interfaces \
  --delete-bmc-suppressions
```

Power shelf:

```bash
nico-admin-cli -a <api-url> power-shelf force-delete \
  <power-shelf-id> \
  --delete-interfaces \
  --delete-bmc-suppressions
```

**Expected result**: Control-plane records for that device are removed. If the
hardware is still present, Site Explorer can ingest it from the reset state.

Host flag details and credential-retention behavior are in
[Force deleting and rebuilding NICo hosts](../playbooks/force_delete.md).
Switch and power-shelf flag details are in the generated CLI reference linked
from each device procedure.

## Rebuild and re-ingest

When the same hardware will join a rebuilt NICo installation:

1. Decommission the hardware and force-delete the old records, or tear down
   the old database after decommissioning completes.
2. Rebuild NICo and its credentials store and database.
3. Point the rack management networks and DHCP relays at the new site.
4. Recreate the expected host, switch, and power-shelf inventory with the
   factory credentials that now exist on the devices.
5. Configure the new site's desired BMC, UEFI, and NVOS credentials.
6. Start ingestion and verify that Site Explorer discovers only identities
   owned by the new installation.

Refer to [Ingesting Hosts](../provisioning/ingesting-hosts.md) and
[Rack-Level Administration](../manuals/rack_level_admin.md) for site and rack
setup.
