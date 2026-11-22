# Rebooting a machine

An infrastructure provider administrator can request a restart of a managed host through the NVIDIA Infra Controller (NICo) REST API. For a data processing unit (DPU), use the direct Core procedure below.

Restarting an occupied host can interrupt the tenant's workload. For a tenant-requested reboot, follow [Rebooting an Instance](../configuration/tenant_management.md#rebooting-an-instance).

## Restart a managed host

<Steps toc={true}>

### Configure access

Configure `nicocli` for the target REST API. Your organization's infrastructure provider must own the host's Site, and you must have the `PROVIDER_ADMIN` role. The Site must be registered and the host must be present in its Machine inventory.

### Request a restart

Request `GracefulRestart` when the operating system can shut down cleanly.

```bash
MACHINE_ID='machine-id'
nicocli machine power \
  --action GracefulRestart \
  "$MACHINE_ID"
```

Request `ForceRestart` if a graceful restart is not possible. The actual reset depends on the platform. Some devices use the same reset for both requests.

```bash
nicocli machine power \
  --action ForceRestart \
  "$MACHINE_ID"
```

If the host has an attached instance, acknowledge the workload disruption in the request.

```bash
nicocli machine power \
  --action GracefulRestart \
  --acknowledge-attached-instance true \
  "$MACHINE_ID"
```

HTTP 202 means the device-facing power operation returned success to Core. The REST API does not expose a reboot task or completion status. Look for a new host boot event or a reset in host uptime, then verify that the host returns to its expected health. A host restart can leave the baseboard management controller (BMC) online. If a request times out, check the host’s boot history before sending another restart.

</Steps>

## Restart an attached DPU

The REST Machine inventory contains hosts. A DPU ID normally returns HTTP 404 from the Machine power endpoint. Substituting the host ID targets the host.

A Site operator with [direct Core API access](../manuals/nico-admin-cli.md) can use [`nico-admin-cli bmc-machine admin-power-control`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/bmc-machine/bmc-machine-admin-power-control.md). Specify the DPU's own Core machine ID and choose a graceful or forced restart. Confirm the DPU restarted using a DPU boot event or its uptime.

## Handle a refused restart

Sites running a Core build that includes [the administrator power checks](https://github.com/dsx-ai-factory/infra-controller/pull/9511) can refuse a restart before invoking the power backend. These refusals return HTTP 412 through the REST Machine API or `FAILED_PRECONDITION` through the direct Core API. Check the reason in the error.

- **Target changed during preparation.** Verify the machine ID and BMC against Site inventory before requesting another restart. For a host, `nicocli machine get --include-metadata "$MACHINE_ID"` shows the last reported BMC details. For a DPU, also verify its host attachment. If Core reports multiple hosts for the same DPU, have the Site operator resolve the conflicting attachments.
- **NIC firmware work already owns the host.** `nicocli machine get "$MACHINE_ID"` can show “Machine NIC firmware is being updated” or “Machine NIC firmware update failed” in the host's status history. If the update failed or stopped progressing, give the host ID to the Site operator. Before retrying the restart, have the operator confirm that firmware work on the host has finished and that Core has cleared the update.
- **The host has an unfinished instance reboot.** Give the Site operator the host ID and reboot token from the error if the request is stuck. After the queued request settles, check whether the host already restarted before issuing another restart.

For an attached DPU, Core checks the owning host. The check runs just before Core sends the restart. New firmware work can start while the target is restarting, and standalone firmware tools run outside Core’s checks. Wait for the target to come back before starting firmware work.
