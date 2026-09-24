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
- Managed-host console access, or SSH access to Linux Scout when the site
  configures it, for post-boot verification.

## Request a Linux Scout Boot

1. Show the instance and record its `MACHINE ID`:

   ```bash
   nico-admin-cli instance show <instance-id>
   ```

1. Confirm that the managed host is in `Assigned/Ready`:

   ```bash
   nico-admin-cli managed-host show <machine-id>
   ```

   Continue only when the `State` field is `Assigned/Ready`. In any other
   state, the command below can use the synchronous Redfish reboot path without
   verifying the boot order. Wait for the host to return to `Assigned/Ready`,
   or resolve the state-machine issue before continuing.

1. Request an instance reboot through the custom PXE flow for a machine in
   `Assigned/Ready`:

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

1. Confirm through the managed-host console, or through site-configured SSH
   access, that the managed host boots Linux Scout.

If the host does not boot Linux Scout, inspect the instance state and managed
host history for boot-order, BMC connectivity, or reboot failures. For general
state inspection, refer to
[State Machine Debugging](../stuck_objects/state_machine_debugging.md).
