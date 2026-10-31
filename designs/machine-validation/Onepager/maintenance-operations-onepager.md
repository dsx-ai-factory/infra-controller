# Maintenance Operations through Machine Validation Framework

**Discussion proposal · 2026-10-28** · [Issue #7806][issue]

## Proposal

Introduce an `Assigned/Maintenance` substate for maintenance during approved
downtime while preserving the tenant's instance. Evolve Machine Validation into
a generic operation framework that Cleanup, Validation, and Assigned/Maintenance
can all use. Start with firmware updates and verification; extend to other
qualified operations over time.

The principle is **one reusable execution framework, with lifecycle-specific
control**. Plugins perform hardware-specific operations. NICo controls admission,
Scout boot, activation reboots or power cycles, recovery, and return to service.
This follows the issue's [permanent direction for in-band firmware][direction].
The short-term plan uses existing workflows; the long-term plan introduces the
shared framework and assigned maintenance lifecycle.

## Short-term plan — deliver through existing workflows

**Tenant Released machines — use Machine Validation as it is.** Configure a qualified
container plugin for the `Cleanup` context to perform maintenance and verification
together during post-release validation. Inspect the device, update only when
needed, and verify the intended state. When both complete in one execution,
no additional maintenance or repair mode is required.

If verification requires a host reboot, use [maintenance mode][maintenance] with
release, or [release-for-repair][repair] for a reported issue, to prevent allocation
between runs. Complete the update run, allow its [completion reboot][validation],
and wait for unassigned `Ready`. Start a verification-only [on-demand run][admission],
which also begins with a reboot. Retain the block until verification passes and
complete the maintenance-exit or repair-resolution procedure. Coordinate
auto-repair claiming and any device-required power cycle separately. Existing
tests run sequentially without automatic reboots between them.

**Retained instances — build on the assigned firmware workflow.** Use existing
[`Assigned/HostReprovision`][assigned] for supported firmware updates. It already
supports approval, Scout execution, activation, version checks, and return to the
same instance. No Machine Validation integration is needed for those updates.

**Container plugins require assigned-lifecycle integration.** Machine Validation
execution can be reused within `Assigned/HostReprovision` by extending Scout
dispatch, result acceptance, and completion/failure handling to preserve the
instance. Initially support qualified plugins that perform maintenance and
verification in one execution. Cross-reboot verification additionally requires
persisted sequencing and continuation. This integration can be delivered
incrementally before the broader shared framework and `Assigned/Maintenance`
lifecycle.

Opening on-demand validation to `Assigned` is insufficient: the assigned controller
does not execute it, and existing validation completes through `HostInit/Ready`.
Targeted instance creation requires release and creates another instance.
Neither provides maintenance while preserving the assignment.

**Next release increment:** add the proposed reboot-before-each-test option with
persisted progress and continuation. Registering two tests alone does not provide
ordered activation and recovery. General validation suites on assigned machines
remain outside the first delivery.

## Long-term plan — shared operations and Assigned/Maintenance

**Generalize the Machine Validation executor.** Build on its existing capabilities
and make it reusable across Cleanup, Validation, and `Assigned/Maintenance` for
sanitization, firmware updates, and validation tests. Extend it with durable
reboot support so operations resume from persisted checkpoints after a reboot.

| Lifecycle owner | Operations using the shared framework | Successful completion |
| --- | --- | --- |
| Cleanup | Required disk/TPM sanitization | Continue cleanup |
| Validation | BOM checks, maintenance, verification, tests | Complete required checks before availability |
| Assigned/Maintenance | Approved maintenance, starting with firmware | Return the same instance to service |

Sanitization stays owned by Cleanup while its execution moves to the shared
framework. Retained-instance plans exclude disk erasure and TPM wiping. Each
lifecycle retains its boot, inventory, lockdown, failure, and availability rules.

**Introduce Assigned/Maintenance.** This substate follows the same lifecycle
pattern as the existing `Assigned/HostReprovision`: perform maintenance during
approved downtime while retaining the tenant's instance. Migrate the existing
assigned firmware behavior into this broader maintenance substate, reusing Scout
preparation, activation, version checks, and restoration logic:

```text
Assigned/Ready → Assigned/Maintenance { Prepare → Execute → Activate/Verify → Restore }
               → Assigned/Ready
```

Preserve the instance, addresses, attachments, and tenant boot configuration.
Report `Updating` with progress; serialize conflicting requests and define
termination handling. On failure or timeout, preserve the assignment and expose
controlled recovery. Do not reinstall the OS or repeat instance creation.
Define an observable restoration check and timeout: the existing firmware path's
restart alone does not prove tenant OS recovery.

## Delivery and acceptance

Deliver plugins for Tenant Released machines first. Add assigned plugin execution
within the existing firmware workflow and reboot/continuation support incrementally,
converging on the long-term shared executor and `Assigned/Maintenance` lifecycle.
Preserve persisted-state and API/status compatibility during migration, including
in-progress firmware updates. Validate restart recovery, stale/duplicate results,
bounded retries, concurrent tenant requests, restoration without data erasure,
and required sanitization before released machines become available.

[issue]: https://github.com/dsx-ai-factory/infra-controller/issues/7806
[direction]: https://github.com/dsx-ai-factory/infra-controller/issues/7806#issuecomment-6322603160
[maintenance]: https://github.com/dsx-ai-factory/infra-controller/blob/71248272b1e429d76094d432b9f69659a9fcb84f/crates/api-core/src/handlers/managed_host.rs#L193
[repair]: https://github.com/dsx-ai-factory/infra-controller/blob/71248272b1e429d76094d432b9f69659a9fcb84f/crates/api-core/src/handlers/instance.rs#L645
[admission]: https://github.com/dsx-ai-factory/infra-controller/blob/71248272b1e429d76094d432b9f69659a9fcb84f/crates/api-core/src/handlers/machine_validation.rs#L943
[validation]: https://github.com/dsx-ai-factory/infra-controller/blob/71248272b1e429d76094d432b9f69659a9fcb84f/crates/machine-controller/src/handler/machine_validation.rs
[assigned]: https://github.com/dsx-ai-factory/infra-controller/blob/71248272b1e429d76094d432b9f69659a9fcb84f/crates/machine-controller/src/handler.rs#L9390
