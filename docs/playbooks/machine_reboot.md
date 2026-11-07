# Rebooting a machine

This page describes how to reboot a machine managed by NVIDIA Infra Controller (NICo) (i.e. a managed host or DPU)
in any potential state of its lifecycle.

## Important note

*This is not a site-provider or tenant facing workflow.
Rebooting a machine while it is in-use for a tenant can have unexpected
side effects. If a tenant requires a reboot, they should use the
`InvokeInstancePower` request - which is properly integrated into the
instance lifecycle.**

## Reboot Steps

<Steps toc={true}>

### Obtain access to `nicocli`

Configure `nicocli` for the target REST API. The caller's organization must have an Infrastructure Provider that owns the Site containing the Machine, and the caller must have the `PROVIDER_ADMIN` role.

### Execute the Machine power control operation

Use `GracefulRestart` when the operating system can shut down cleanly. Use `ForceRestart` only when a graceful restart is not possible.

```bash
MACHINE_ID='machine-id'
nicocli machine power-control-machine \
  --action GracefulRestart \
  "$MACHINE_ID"
```

If a graceful restart is not possible, use the forced action explicitly:

```bash
nicocli machine power-control-machine \
  --action ForceRestart \
  "$MACHINE_ID"
```

If the Machine has an attached Instance, acknowledge the workload disruption explicitly:

```bash
nicocli machine power-control-machine \
  --action GracefulRestart \
  --acknowledge-attached-instance true \
  "$MACHINE_ID"
```

A successful request returns HTTP 202 after NICo accepts the power-control
request. The API does not expose a reboot task or terminal reboot status, and
`nicocli machine get "$MACHINE_ID"` can return the same REST lifecycle state
before, during, and after the reboot. Confirm that the machine becomes
unavailable and returns through the Site's host or BMC monitoring instead of
treating a single Machine status, or a poll of that status, as completion.

</Steps>
