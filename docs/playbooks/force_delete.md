# Force deleting and rebuilding NICo hosts

In various cases, it might be necessary to force-delete knowledge about hosts from
the database and to restart the discovery process for those hosts. The following are
use-cases where force-delete can be helpful:

- If a host managed by NVIDIA Infra Controller (NICo) has entered an erroneous state from which it can not
automatically recover.
- If a non backward compatible software update requires the host to go through the discovery phase again.

## Important note

*This is not a site-provider facing workflow, since force-deleting a machine
does skip any cleanup on the machine and leaves it in an undefined state where the tenants OS could be still running.
force-deleting machines is purely an operational tool. The operator which executed the
command needs to make sure that either no tenant image is running anymore, or take additional steps
(like rebooting the machine) to interrupt the image.
Site providers would get a safe version of this workflow later on that moves the machine through all necessary cleanup steps*

To leave the host in a clean pre-ingestion state before you remove its
control-plane records, [decommission the host](../decommissioning/hosts.md)
first, then force-delete it with the flags that remove interfaces,
suppressions, and retained boot entries.

## Force-Deletion Steps

The following steps can be used to force-delete knowledge about a NICo host:

### 1. Obtain access to `nico-admin-cli`

See nico-admin-cli access on a NICo deployment.

### 2. Execute the `nico-admin-cli machine force-delete` command

Executing `nico-admin-cli machine force-delete` will wipe most knowledge about
machines and instances running on top of them from the database, and clean up associated CRDs.
It accepts the machine-id, hostname, MAC or IP of either the managed host or DPU as input,
and will delete information about both of them (since they are heavily coupled).

It returns all machine-ids and instance-ids it acted on, as well as the BMC
IP for the managed host.

Example:

```bash
/opt/nico/nico-admin-cli -a https://127.0.0.1:1079 machine force-delete --machine="60cef902-9779-4666-8362-c9bb4b37184f"
```

For a full rediscovery wipe (interfaces, BMC interfaces, BMC suppressions, and
retained boot targets), add:

```bash
/opt/nico/nico-admin-cli -a https://127.0.0.1:1079 machine force-delete \
  --machine="60cef902-9779-4666-8362-c9bb4b37184f" \
  --delete-interfaces --delete-bmc-interfaces \
  --delete-bmc-suppressions --delete-retained-boot-interfaces
```

### 3. Use the returned BMC IP and machine-id to reboot the host

See [Rebooting a machine](machine_reboot.md).
`machine force-delete` returns the managed host BMC IP and machine IDs; it
does not return a BMC port. Supply the returned BMC IP, port `443` unless you
know the device uses a different management port, and `machine_id` as
parameters.

When Site Explorer configured BMC credentials for the host, force-delete
retains the last set in Vault by default so the site controller can continue
to access the device. If no credentials were configured, there is nothing to
retain and the site controller cannot access the BMC through Vault. The
optional `--delete-bmc-credentials` flag deletes configured credentials; do
not use it until any required device recovery is complete.

Once a reboot is triggered, the DPU of the Machine should boot into the
NICo discovery image again. This should initiate DPU discovery. A second
reboot is required to initiate host discovery. After those steps, the host
should be fully rebuilt and available.

## Reinstall OS Steps

Deleting and recreating a NICo instance can take upwards of 1.5 hours. However, if you do not need to change the
PXE image you can reinstall the OS in place and reuse your allocated system. All the other information about your
instance will stay the same. *This procedure will delete any data on the host!*

The following steps can be used to reinstall the host OS on a NICo host:

### 1. Obtain access to the `nico-admin-cli` tool

See nico-admin-cli access on a NICo deployment.

### 3. Execute the `nico-admin-cli instance reboot --custom-pxe` command

```text
nico-admin-cli -f json -c https://127.0.0.1079/ instance reboot --custom-pxe -i 26204c21-83ac-445e-8ea7-b9130deb6315
Reboot for instance 26204c21-83ac-445e-8ea7-b9130deb6315 (machine fm100hti4deucakqqgteo692efnfo7egh7pq1lkl7vkgas4o6e0c42hnb80) is requested successfully!
```
