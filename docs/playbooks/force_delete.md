# Force Deleting and Rebuilding NICo Hosts

Use force deletion to remove NVIDIA Infra Controller (NICo) records for a host
and restart discovery. This can help when a host cannot recover from an error
or a software update requires rediscovery.

## Important Note

Force deletion is for operator recovery, not normal tenant or site-provider
instance release. It skips normal instance cleanup. It does not wipe the tenant
operating system or its data, and it does not ensure that tenant workloads stop.
BIOS cleanup can force-restart the host. Before reuse, verify that no tenant
workload is running, including after any recovery reboot.

To clean the host before removing its records,
[decommission the host](../decommissioning/hosts.md) first. Then follow
[Force Delete After Decommissioning](../decommissioning/index.md#force-delete-after-decommissioning)
for the required flags. The recovery steps below do not apply to decommissioning.

## Force-Deletion Steps

Use these steps to delete the host's records and restart discovery.

### 1. Obtain access to `nico-admin-cli`

Access `nico-admin-cli` on your NICo deployment.

### 2. Execute the `nico-admin-cli machine force-delete` command

The command removes most machine and instance records from the database and
cleans up associated CRDs. You can identify the host or DPU by machine ID,
hostname, MAC address, or IP address. Either choice deletes records for both the
host and its DPUs. The response lists the affected machine and instance IDs and
the host's BMC IP.

Before deleting by host ID, remove the host's instance type association. If it
still has one, the API returns `FailedPrecondition`. Neither
`--allow-delete-with-instance` nor `--wait-for-instance-dpu` bypasses this check.

By default, a new deletion does not wait for a DPU response, even if the DPU is
offline. To wait for confirmation that tenant networking has stopped, refer to
[Optional DPU Acknowledgement Wait](#optional-dpu-acknowledgement-wait) before
running the command.

This example assumes that the host has no instance type association:

```bash
/opt/nico/nico-admin-cli -a https://127.0.0.1:1079 machine force-delete --machine="60cef902-9779-4666-8362-c9bb4b37184f"
```

To also remove interfaces, BMC interfaces, BMC suppressions, and retained boot
targets for a full rediscovery, use these flags:

```bash
/opt/nico/nico-admin-cli -a https://127.0.0.1:1079 machine force-delete \
  --machine="60cef902-9779-4666-8362-c9bb4b37184f" \
  --delete-interfaces --delete-bmc-interfaces \
  --delete-bmc-suppressions --delete-retained-boot-interfaces
```

### Optional DPU Acknowledgement Wait

Deleting records does not prove that a DPU has stopped forwarding tenant traffic.
Add `--wait-for-instance-dpu` to request that confirmation. The flag defaults to
false.

If an Instance exists when you start deletion with this flag, the API requests
the Admin network configuration. It keeps the Instance and machine records until
every attached DPU confirms that configuration. While waiting, the API returns
`all_done=false`. A new deletion without an Instance does not wait, even with
the flag.

Returning to Admin stops tenant networking through the DPUs. It does not shut
down the tenant operating system or wipe its data.

**Before you use the wait.** Every API server that can receive the command or
its retries must include the
[optional-wait implementation](https://github.com/dsx-ai-factory/infra-controller/pull/7147).
Older servers ignore both the flag and a wait saved by a newer server. During a
rolling upgrade or after a downgrade, an older server can delete records without
DPU acknowledgement.

Deletion consent and the DPU wait are separate choices:

- `--allow-delete-with-instance` acknowledges deleting an allocated Instance.
  It does not enable the wait.
- `--wait-for-instance-dpu` enables the wait. It does not grant deletion consent.

To request both, use the following command:

```bash
/opt/nico/nico-admin-cli -a https://127.0.0.1:1079 machine force-delete \
  --machine="60cef902-9779-4666-8362-c9bb4b37184f" \
  --allow-delete-with-instance --wait-for-instance-dpu
```

**Completion and recovery.** Wait for the CLI to finish successfully before
continuing to the power cycle. It waits 5 seconds between polling calls, with a
nominal 20-minute retry limit.
The CLI checks that limit after each RPC call, so a slow call can extend the
total time. An RPC error also makes the CLI exit and can leave the result unknown.

Stopping the CLI or reaching its time limit does not cancel deletion. After the
API saves the wait, it remains in effect across retries, API restarts, and later
removal of the Instance. Omitting the flag on a retry does not cancel it. An
unavailable DPU can leave the machine in `ForceDeletion` indefinitely. You cannot
cancel the deletion.

If the command stops before completion, resolve the reported error or restore
DPU communication. Then rerun the same command to resume or confirm completion.
The machine controller does not finish force deletion in the background.

### 3. Power-cycle the host through its BMC

Use [`nico-admin-cli redfish ac-power-cycle`](../manuals/nico-admin-cli/commands/redfish/redfish-ac-power-cycle.md)
with the returned BMC IP and current BMC credentials. This command connects
directly to the BMC and does not require the deleted machine record.

When Site Explorer configured BMC credentials for the host, force-delete
retains the last set in Vault by default so the site controller can continue
to access the device. If no credentials were configured, there is nothing to
retain and the site controller cannot access the BMC through Vault. The
optional `--delete-bmc-credentials` flag deletes configured credentials; do
not use it until any required device recovery is complete.

After the first reboot, the DPU should boot into the NICo discovery image and
start discovery. Reboot a second time to start host discovery. After both steps,
the host should be rebuilt and available.

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
