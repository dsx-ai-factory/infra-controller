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

## Core Version Requirement

Server-side Instance consent and the Instance Type override require a Core build
containing [the forced-deletion safeguards in PR #5394](https://github.com/dsx-ai-factory/infra-controller/pull/5394).
Use the per-server `git_sha` verification procedure in
[Optional DPU Acknowledgement Wait](#optional-dpu-acknowledgement-wait), checking
for these changes in each server's build. CLI help or one load-balanced request
does not establish support on every server.

Older Core ignores unknown protobuf fields: it does not enforce the new
attached-Instance consent check or support the new Instance Type override.
REST performs its own checks against REST records before dispatch, but those
checks do not establish Core's current ownership state or replace Core's
server-side enforcement. During a mixed-version rollout, use only endpoints
that reach verified Core builds before relying on these safeguards.

## Force-Deletion Steps

Use these steps to delete the host's records and restart discovery.

### 1. Obtain access to `nico-admin-cli`

Access `nico-admin-cli` on your NICo deployment.

### 2. Execute the `nico-admin-cli machine force-delete` command

The command removes most machine and instance records from the database and
cleans up associated CRDs. You can identify the host or DPU by machine ID,
hostname, MAC address, or IP address. NICo deletes the host and its DPUs together.
If it cannot find a host for the specified DPU, it deletes only that DPU. The
response lists the affected machine and instance IDs and, when available, the
host's BMC IP.

By default, the API rejects a Machine assigned to an Instance Type or attached
to an Instance with `FailedPrecondition`. Add `--allow-delete-with-instance-type`
to remove the Instance Type association. `--allow-delete-with-instance` implies
that override and removes the attached Instance control-plane record without
first requesting a graceful workload shutdown. These checks also apply to the
host when the query identifies one of its DPUs.

By default, a new deletion does not wait for a DPU response, even if the DPU is
offline. To wait for confirmation that tenant networking has stopped, refer to
[Optional DPU Acknowledgement Wait](#optional-dpu-acknowledgement-wait) before
running the command.

This example assumes that the host has no Instance Type association or attached
Instance:

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

### 3. Power-cycle the host through its BMC

Wait for the force-delete CLI to finish successfully before continuing.

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

## Optional DPU Acknowledgement Wait

Deleting records does not prove that a DPU has stopped forwarding tenant traffic.
Add `--wait-for-instance-dpu` to request that confirmation. The flag defaults to
false.

If an instance exists when you start deletion with this flag, the API requests
the Admin network configuration. It keeps the instance and machine records until
every attached DPU confirms that configuration. While waiting, the API returns
`all_done=false`. A new deletion without an instance does not wait, even with
the flag.

Returning to Admin stops tenant networking through the DPUs. It does not shut
down the tenant operating system or wipe its data.

**Before you use the wait.** Check the build on every API server that can receive
the command or its retries. Run this read-only command against each server
directly, replacing the example address with that server's endpoint:

```bash
nico-admin-cli --api-url https://api-server.example.com:1079 version
```

Read `git_sha` in the `carbide-api` result. Use your deployment's build records
or source history to confirm that revision includes commit
`47b8097a8252bd866c1fb02942bfe1945cb9be0e` from
[Add an Optional DPU Wait to Machine Force Deletion](https://github.com/dsx-ai-factory/infra-controller/pull/7147).
Checking the CLI's help or one load-balanced request does not confirm support
on every API server. If you cannot verify a server's build, do not rely on the wait.

Older servers ignore both the flag and a wait saved by a newer server. During a
rolling upgrade or after a downgrade, an older server can delete records without
DPU acknowledgement.

Deletion consent and the DPU wait are separate choices:

- `--allow-delete-with-instance` acknowledges deleting an allocated instance.
  It does not enable the wait.
- `--wait-for-instance-dpu` enables the wait. It does not grant deletion consent.

The API enforces deletion consent for machine ID, hostname, MAC address, and IP
address queries, including queries that identify a DPU associated with the host.

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
removal of the instance. Omitting the flag on a retry does not cancel it. An
unavailable DPU can leave the machine in `ForceDeletion` indefinitely. You cannot
cancel the deletion.

If the command stops before completion, resolve the reported error or restore
DPU communication. Then rerun the same command to resume or confirm completion.
The machine controller does not finish force deletion in the background.

## REST API Deletion and Retained-Record Recovery

Provider Admins can force-delete a Machine through
`DELETE /v2/org/{org}/nico/machine/{machineId}?force=true`.
The Site must be Registered when starting a deletion. An assigned Instance Type
requires `allowDeleteWithInstanceType=true`. An attached Instance requires
`allowDeleteWithInstance=true`, which also permits its Instance Type removal.
A loss of capacity below Allocation Constraints separately requires
`allowDeleteWithAllocation=true`. These acknowledgments do not wipe tenant data
or guarantee that the workload stops.

Before contacting Core, REST reports the Machine as `Initializing` with the
message `Machine is being force deleted` and marks it unusable by Tenants.
Automatic and batch Instance placement skip it. A Machine already missing on
Site keeps its status. An Instance override retains the REST records even when
no Instance is attached. An Allocation override retains them when capacity
would be insufficient. Allocation Constraints remain owned by their existing
owners.

HTTP `202` confirms Site deletion completion. It does not mean retained REST
records have been removed. Retained records report the Machine as missing on
Site with status `Error`, as the next inventory would. A retained Machine keeps
counting toward Allocation capacity until its Instance Type association is
removed. HTTP `409` with `Retry-After: 5` means Core confirmed incomplete
cleanup. Wait at least five seconds and repeat the same forced DELETE. The
caller must continue the operation, REST does not schedule another attempt
automatically.

Forced requests for the same Machine join a Core deletion that is still
running. After a timeout or transport error, repeat the forced DELETE. Core
treats a Machine it has already deleted as a completed deletion, so the retry
finishes the REST cleanup.

To remove retained REST records:

1. Have the owners remove the retained Instance. Existing quota guards may
   require the owners to reduce or rebalance Allocation Constraints first.
   This cleanup does not delete Allocation records.
2. Repeat the forced DELETE without `allowDeleteWithInstance`. Ordinary
   `DELETE /v2/org/{org}/nico/machine/{machineId}` also removes the records
   once the Machine has been missing on Site for 24 hours without an Instance
   or Instance Type.

If Core rediscovers the Machine before the records are removed, inventory
restores it as a present Machine. A repeated forced DELETE then deletes it on
the Site again. The direct `nico-admin-cli` polling and recovery behavior
described above is separate from this REST workflow.

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
