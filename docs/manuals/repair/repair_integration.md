# Repair System Integration

This page describes how NICo coordinates full machine repair after a tenant releases an instance with a reported machine issue. It is intended for provider operators, repair automation owners, and platform engineers who need to understand the health overrides, repair tenant behavior, and manual recovery paths behind the tenant-facing repair runbooks.

Tenant admins should start with one of these runbooks:

- [Online Repair](online_repair.md) for non-disruptive repair while the tenant keeps the instance assignment.
- [Release Instance for Full Repair](release_instance_for_repair.md) for disruptive repair after the tenant releases the instance.

Platform admins and repair tenant operators should use [Repair Tenant Workflow](repair_tenant_workflow.md) for the step-by-step targeted instance creation, repair outcome labeling, and repair tenant release procedure.

## Workflow Overview

Full repair begins when a tenant releases an instance and includes a `machineHealthIssue` in the request body:

```text
DELETE /v2/org/{org}/nico/instance/{instanceId}
```

Restish example:

```bash
restish nico-stg delete-instance <tenant-org-id> <instance-id> < release-for-repair.json
```

The release request removes the tenant assignment. NICo then uses the reported issue and site configuration to decide whether the machine should wait for manual intervention or be made available to repair automation.

The high-level flow is:

1. Tenant releases an instance with a machine health issue.
2. The API marks the instance as terminating and sends a release request to the site.
3. NICo releases and sanitizes the machine through the normal cleanup path.
4. NICo records the reported issue as a health override.
5. If auto-repair is enabled, NICo also applies a repair request override.
6. A repair tenant or repair automation claims the machine, diagnoses it, and performs repair.
7. Repair completion determines whether the machine returns to the normal allocation pool or remains quarantined.

## Repair Signals

NICo uses health overrides to keep repair intent visible and to prevent unsafe allocation while a machine is under investigation.

| Override | Purpose | When Applied | Result |
|---|---|---|---|
| `tenant-reported-issue` | Records the tenant-reported problem. | When an instance is released with `machineHealthIssue`. | Prevents normal allocation until the issue is cleared. |
| `repair-request` | Signals that repair automation should claim the machine. | When auto-repair is enabled, or when an operator manually requests repair. | Makes the machine eligible for repair workflows instead of normal tenant allocation. |
| `request-online-repair` | Signals active online repair. | When online repair is enabled through `update-machine`. | Keeps the assigned instance in `Repairing` while repair is active. |

Full repair and online repair are intentionally different:

- Full repair releases the tenant instance and moves the machine through cleanup before repair.
- Online repair keeps the tenant instance assigned and moves the instance from `Ready` to `Repairing`.
- The two workflows are not active at the same time. If an online repair attempt needs to escalate to full repair, the tenant must clear online repair first and then release the instance for full repair.
- When online repair fails and is escalated to full repair, the tenant should update instance labels, for example `onlineRepair.status: Failed`, before releasing the instance. This preserves failure context for tenant and provider tooling during the escalation.

## Auto-Repair Configuration

Auto-repair is controlled by the site API configuration:

```toml
[auto_machine_repair_plugin]
enabled = true
```

When auto-repair is enabled, a tenant release with `machineHealthIssue` applies both `tenant-reported-issue` and `repair-request`.

When auto-repair is disabled, only `tenant-reported-issue` is applied. The machine remains unavailable for normal allocation until a provider operator clears the issue or manually triggers repair. If provider operations use a repair tenant and expect the repair tenant release to clear or reroute repair health overrides automatically, a `repair-request` override must be present before that release.

## Repair Tenant Behavior

Repair tenants or repair automation use targeted provisioning to claim machines marked for repair. A repair tenant release is different from the original tenant's release:

- The original tenant releases the instance with `machineHealthIssue`.
- The repair tenant releases the repair instance after investigation or repair.
- Repair tenant releases should set `isRepairTenant: true`.
- `isRepairTenant: true` requires effective targeted instance creation for the tenant, granted by a Provider Admin on the tenant account. See [Granting Targeted Instance Creation](../../configuration/tenant_management.md#granting-targeted-instance-creation) for what makes it effective.
- The repair tenant should set the machine label `repair_status: InProgress` after claiming the machine, then set the final `repair_status` before releasing the repair instance. This prevents stale completion labels from older repair attempts.
- A final `repair_status: Completed` with no new issue returns the machine toward the ready pool; failed, incomplete, missing, or unknown status keeps the machine blocked for repair-failed or manual handling.

Repair automation must report the repair result before releasing the repair instance. The expected repair status metadata is:

| Value | Meaning | Result |
|---|---|---|
| `Completed` | Repair succeeded. | NICo can clear repair-related overrides and return the machine to the normal pool. |
| `Failed` | Repair did not resolve the issue. | NICo keeps the machine quarantined for provider intervention. |
| `InProgress` | Repair work is not complete. | Treat the release as incomplete and keep the machine out of the normal pool. |

If the repair tenant releases the machine without a successful completion signal, NICo treats the repair as incomplete. This prevents a partially repaired or unverified machine from returning to normal tenant allocation.

## Completion Outcomes

The completion outcomes below apply when the machine has an active `repair-request` override at the time of the repair tenant release. Without `repair-request`, NICo does not clear `tenant-reported-issue` from a successful repair tenant release with no new issue.

### Repair Completed

When repair is successful and the repair tenant reports completion:

1. The repair request is cleared.
2. The tenant-reported issue is cleared.
3. The machine completes validation and becomes eligible for normal allocation.

Provider verification:

```bash
nico-admin-cli machine show <machine-id>
nico-admin-cli machine health-override show <machine-id>
```

Expected state:

- No active `repair-request` override.
- No active `tenant-reported-issue` override for the repaired issue.
- Machine is healthy and available for normal allocation.

### Repair Failed or Incomplete

When repair fails or the repair tenant releases without a successful completion signal:

1. The repair request may be removed because the repair attempt has ended.
2. The tenant-reported issue remains or is re-applied.
3. The machine does not return to the normal allocation pool.
4. Provider operations must investigate, retry repair, or escalate to manual intervention.

Provider verification:

```bash
nico-admin-cli machine show <machine-id>
nico-admin-cli machine health-override show <machine-id>
```

Expected state:

- `tenant-reported-issue` remains present.
- Machine is not available for normal allocation.

## Manual Provider Actions

Use admin tooling for provider-only recovery actions. The exact command syntax can vary by deployment and CLI version, but the common operations are:

Inspect machine state:

```bash
nico-admin-cli machine show <machine-id>
nico-admin-cli machine health-override show <machine-id>
```

Manually request repair:

```bash
nico-admin-cli machine health-override add <machine-id> --template RequestRepair \
  --message "Manual repair trigger for tenant-reported issue"
```

Clear a resolved tenant-reported issue:

```bash
nico-admin-cli machine health-override remove <machine-id> tenant-reported-issue
```

Clear a stale repair request:

```bash
nico-admin-cli machine health-override remove <machine-id> repair-request
```

Escalate a machine that needs manual provider investigation:

```bash
nico-admin-cli machine health-override add <machine-id> --template OutForRepair \
  --message "Repair unsuccessful, requires manual investigation"
```

Do not clear repair-related overrides unless the repair outcome is known. Clearing them early can return an unhealthy machine to tenant allocation.

## Operational Scenarios

### Auto-Repair Disabled

If `auto_machine_repair_plugin.enabled` is `false`, a tenant release with `machineHealthIssue` records the issue but does not trigger repair automation.

Provider action:

1. Inspect the machine and issue details.
2. Decide whether to manually request repair.
3. Apply a manual repair request if the machine should enter the repair workflow.

### Repair Automation Does Not Claim the Machine

If auto-repair is enabled but no repair tenant claims the machine:

1. Confirm that `repair-request` is present.
2. Confirm repair tenant capacity and matching criteria.
3. Check repair automation health and connectivity.
4. Manually re-apply or escalate the repair request if needed.

Common causes include repair tenant capacity limits, missing allocation criteria, repair automation downtime, or site connectivity issues.

### Repair Tenant Reports Success but Issue Persists

If the repair tenant reports success but validation or monitoring still shows the issue:

1. Re-apply or keep `tenant-reported-issue`.
2. Add provider investigation details to the machine record or repair ticket.
3. Escalate with an `OutForRepair` style override if the machine should not re-enter automated repair immediately.

Avoid repeatedly sending the same machine through automated repair without new evidence. That can create loops where the repair system keeps claiming and releasing the same unhealthy machine.

## Relationship to Tenant Runbooks

The tenant-facing runbooks define when a tenant should use each repair path:

| Tenant need | Runbook | Result |
|---|---|---|
| Repair while keeping the assignment | [Online Repair](online_repair.md) | Instance moves `Ready` to `Repairing`, then back to `Ready` when cleared. |
| Disruptive repair or online repair failed | [Release Instance for Full Repair](release_instance_for_repair.md) | Online repair is cleared first if active; then the instance is released and the machine enters cleanup, quarantine, and repair handling. |

This page covers what provider systems do after the tenant has chosen the full repair path.

For the operational runbook used by a repair tenant, see [Repair Tenant Workflow](repair_tenant_workflow.md).
