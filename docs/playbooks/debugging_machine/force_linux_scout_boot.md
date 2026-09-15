# Force a Managed Host to Boot into Linux Scout

Use this playbook when you need the Linux Scout environment for managed-host
diagnostics and the normal NICo boot workflow does not select it.

<Warning>

Rebooting interrupts the tenant workload. Confirm the maintenance window before
continuing.

</Warning>

## Prerequisites

- Access to `nico-admin-cli` for the site.
- The ID of the instance assigned to the managed host.

## Request a Linux Scout Boot

Request an instance reboot through the custom PXE flow:

```bash
nico-admin-cli instance reboot \
  --instance <instance-id> \
  --custom-pxe
```

NICo selects the appropriate Linux Scout boot instructions for the managed
host. For an instance in `Assigned/Ready`, the state machine first verifies the
boot order and then advances through `Assigned/BootingWithDiscoveryImage`.

## Verify the Boot

1. Confirm that the command reports that the reboot was requested.
1. Inspect the instance state:

   ```bash
   nico-admin-cli instance show <instance-id>
   ```

1. Confirm that the managed-host console boots Linux Scout.

If the host does not boot Linux Scout, inspect the instance state and managed
host history for boot-order, BMC connectivity, or reboot failures. For general
state inspection, refer to
[State Machine Debugging](../stuck_objects/state_machine_debugging.md).
