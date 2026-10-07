# Decommission NICo-managed hardware

Decommissioning is NICo's graceful cleanup step before force-deleting
or moving managed hardware. Use this workflow when
you intend one of the following outcomes:

1. The hardware is permanently leaving the site, either because it is moving to a new site or it is leaving service.
2. The hardware will be ingested from a clean pre-ingestion state into the same site.

After decommissioning begins, the hardware performs a series of cleanup steps
and reaches the terminal state `Decommissioning/Decommissioned`. NICo then
performs no further decommissioning actions.

NICo suppresses discovery and responses from its own DHCP service for the
device's associated interfaces until you remove the suppressions with force-delete.
Endpoints can remain reachable if they use static IP addresses or receive an
address from an external DHCP server. Complete any required network changes or
physical hardware removal before force-deleting the device records.

After cleaning up, use [force-delete](#force-delete-after-decommissioning) to remove a decommissioned device and its records from NICo.

## Choose a procedure

The following procedures document the Core `nico-admin-cli` workflow against the NICo
gRPC API.

- [Decommission Managed Hosts and DPUs](hosts.md): reset host firmware configuration,
  SuperNIC lockdown, DPU images, host and DPU BMCs, and managed credentials.
- [Decommission Managed Switches](switches.md): factory-reset NVOS and the switch BMC, then
  remove managed NVOS and BMC credentials.
- [Decommission Managed Power Shelves](power-shelves.md): factory-reset the shelf BMC or
  PMC, then remove its managed BMC credential.

## Troubleshooting

If decommissioning stops progressing, check the device's controller state and
handler message to identify the blocked operation.

NICo retries temporary failures automatically. If an error persists or the message
requests manual intervention, check connectivity, credentials, and the procedure's
prerequisites. Resolve the reported problem, then monitor the device for progress.

If the workflow cannot recover automatically, manually advance the controller
state after resolving the failure, or force-delete the device and ingest it
again. After the device returns to `Ready`, start a new decommissioning request.
Force-delete removes records but does not complete unfinished cleanup.

## Force-delete after decommissioning

Use these commands when you are removing decommissioned hardware from a
site. They remove records associated with the device:
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
