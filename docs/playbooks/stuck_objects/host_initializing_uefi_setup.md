# Managed Host Stuck in UEFI Setup

Use this playbook when a managed host remains at
`HostInitializing/UefiSetup/SetUefiPassword` and the controller reports a
Redfish `Bios.ChangePassword` failure.

## Symptoms

- Machine history reaches `setuefipassword` and does not advance.
- NICo logs report HTTP 400 from `Bios.ChangePassword`.
- The BMC reports that `OldPassword` is absent or has an unsupported format.

Inspect the recent state history:

```bash
nico-admin-cli -f json machine show <machine-id> \
  | jq -r '.events[] | "\(.time) \(.event)"' \
  | tail -10
```

## Cause

The password known to NICo does not match the UEFI password on the BMC. This
can happen after force-deletion when NICo cannot resolve the required
credential or cannot reach the BMC during its best-effort UEFI cleanup. On
re-ingestion, the BMC rejects the password-change request and host
initialization cannot continue.

NICo force-deletion attempts to clear a recorded host UEFI password before
removing the machine. It logs a warning and continues when that cleanup cannot
complete. The `--delete-bmc-credentials` option removes saved BMC login
credentials; it does not request UEFI-password cleanup.

## Clear the Recorded UEFI Password

Request UEFI-password clearing for the affected host:

```bash
nico-admin-cli host clear-uefi-password --query <machine-id>
```

The command also accepts a host MAC address. NICo must have a recorded UEFI
password and enough BMC information to select the current credential. If NICo
reports that no password is recorded, do not repeatedly run the command; use
the BMC procedure below to reconcile the device state.

## Reconcile the BMC Directly

When NICo cannot clear the password from its recorded state, use the Redfish
command with the password configured on the BMC. Obtain BMC
and UEFI credentials from your approved credential store.

```bash
nico-admin-cli redfish \
  --address <bmc-ip-address> \
  --username <bmc-username> \
  --password <bmc-password> \
  change-uefi-password \
  --current-password <current-uefi-password> \
  --new-password ''
```

After the BMC accepts the clear operation, allow the NICo state controller to
retry `SetUefiPassword` and apply the configured site credential.

<Warning>

Do not force-delete the machine again as the first remediation. Preserve its
state and logs until you have confirmed why UEFI cleanup or credential
selection failed.

</Warning>

## Verify Recovery

1. Confirm that state history advances past `setuefipassword`:

   ```bash
   nico-admin-cli -f json machine show <machine-id> \
     | jq -r '.events[] | "\(.time) \(.event)"' \
     | tail -10
   ```

1. Confirm that the host leaves `HostInitializing/UefiSetup`.
1. Confirm that NICo logs no longer report a `Bios.ChangePassword` failure for
   the host.

For force-deletion behavior and its cleanup options, refer to
[Force Deleting and Rebuilding Hosts](../force_delete.md).
