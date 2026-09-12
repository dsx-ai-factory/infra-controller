# Online Repair

Online repair lets a privileged tenant admin report a machine health issue and move the assigned instance from `Ready` to `Repairing` without releasing the instance. Use this workflow when the repair can be attempted while the tenant keeps the instance assignment.

If online repair cannot fix the issue, clear online repair first and then release the instance for full repair. See [Release Instance for Full Repair](release_instance_for_repair.md).

## Audience and Access

This page is intended for tenant admins and platform operators writing tenant-facing runbooks.

The caller must have access to the Infra Controller REST API through an API profile such as `nico-stg`. The online repair operation is allowed for provider admins and privileged tenant admins. In tenant workflows, the privileged capability is targeted instance creation, and it must be effective at the site being repaired. It is the only tenant capability NICo defines, so no other grant substitutes for it. See [Granting Targeted Instance Creation](../../configuration/tenant_management.md#granting-targeted-instance-creation) for what makes it effective.

## What Online Repair Does

When online repair is enabled:

1. The API validates that the machine is assigned to an instance and that the instance is currently `Ready`.
2. The API records online repair metadata on the instance.
3. The API sends a health override to the site for the reported issue.
4. The assigned instance is moved to `Repairing`.
5. NICo Core treats the online repair health override as an active repair signal, so tenant-facing state remains `Repairing` while the repair override is active and the instance is otherwise tenant-ready.

When online repair is disabled:

1. The API removes the online repair health override.
2. Online repair metadata is removed from the instance.
3. The assigned instance is moved back to `Ready`.

## REST and Restish

The REST API operation is:

```text
PATCH /v2/org/{org}/nico/machine/{machineId}
```

The OpenAPI operation ID is `update-machine`. Restish exposes operation IDs as commands, so the command shape is:

```bash
restish <api-profile> update-machine <tenant-org-id> <machine-id> < <request-body-json>
```

For example, in staging:

```bash
restish nico-stg update-machine <tenant-org-id> <machine-id> < online-repair-on.json
```

`nico-stg` is the Restish API profile or environment. Replace it with the profile for your deployment.

Use Restish help to confirm the operation signature in the target environment:

```bash
restish nico-stg update-machine --help
```

Restish prints the HTTP status and JSON error body when a request fails. Use that response body when troubleshooting validation or permission errors.

## Before You Start

Collect the following values:

| Value | Description |
|---|---|
| `<api-profile>` | Restish profile, for example `nico-stg`. |
| `<tenant-org-id>` | Tenant organization identifier used by the REST API. |
| `<machine-id>` | Machine ID assigned to the tenant instance. This is the `fm...` machine identifier, not the instance UUID. |

Confirm these preconditions:

- The machine is assigned to an instance.
- The assigned instance is in `Ready`.
- The machine is present on the site.
- The issue can be investigated without the tenant releasing the instance.

## Enter Online Repair

Create `online-repair-on.json`:

```json
{
  "onlineRepair": {
    "enabled": true,
    "policy": {
      "allowAutoInstanceDeletionOnFailure": false
    },
    "acknowledgments": {
      "acceptDataCorruptionRisk": true,
      "acceptRepairTeamAccess": true,
      "acceptInstanceDeletionRisk": true
    }
  },
  "healthIssue": {
    "category": "Hardware",
    "summary": "GPU diagnostics show intermittent ECC errors",
    "details": "Tenant observed intermittent GPU ECC errors during workload execution. Please perform online repair while preserving the assigned instance."
  }
}
```

Run:

```bash
restish nico-stg update-machine <tenant-org-id> <machine-id> < online-repair-on.json
```

Expected result:

- The API returns the updated machine.
- The assigned instance moves to `Repairing`.
- A site health override is applied for the tenant-reported repair request.
- The instance remains assigned to the tenant.

Set `allowAutoInstanceDeletionOnFailure` to `false` unless the tenant explicitly authorizes the platform to delete the instance if online repair fails.

## Health Issue Fields

`healthIssue` is required when entering online repair.

| Field | Requirement |
|---|---|
| `category` | Required. Must be one of `Hardware`, `Network`, `Performance`, `Storage`, `Software`, or `Other`. |
| `summary` | Required. Maximum 512 characters. |
| `details` | Required. Maximum 8192 characters. |

Use a short operational summary and detailed reproduction or evidence. The summary is used in the tenant-facing health message.

## Exit Online Repair

After the repair team confirms that the issue is fixed, create `online-repair-off.json`:

```json
{
  "onlineRepair": {
    "enabled": false
  }
}
```

Run:

```bash
restish nico-stg update-machine <tenant-org-id> <machine-id> < online-repair-off.json
```

Expected result:

- The online repair health override is removed.
- Online repair metadata is removed from the instance.
- The assigned instance moves back to `Ready`.

Do not include `healthIssue`, `policy`, or `acknowledgments` when exiting online repair.

If online repair failed and the machine now needs disruptive repair, this exit step is still required before releasing the instance. An instance cannot be released for full repair while it remains in online repair. Clear online repair, confirm the instance is back in `Ready`, and then follow [Release Instance for Full Repair](release_instance_for_repair.md).

## Mark Failed Online Repair

If online repair is being cleared because the repair failed, update the instance labels before releasing the instance for full repair. This leaves a visible breadcrumb for tenant and operator tooling after the instance leaves online repair.

The REST API operation is:

```text
PATCH /v2/org/{org}/nico/instance/{instanceId}
```

The OpenAPI operation ID is `update-instance`. Restish command shape:

```bash
restish <api-profile> update-instance <tenant-org-id> <instance-id> < <request-body-json>
```

First inspect the instance and preserve any existing labels. Instance label updates replace the full label map; labels not included in the update request are removed. Labels are limited to 16 key/value pairs, so use the minimum failure labels if the instance is already near that limit.

```bash
restish nico-stg get-instance <tenant-org-id> <instance-id>
```

Create `online-repair-failed-labels.json` using the existing labels plus the failure labels:

```json
{
  "labels": {
    "env": "staging",
    "owner": "tenant-platform",
    "onlineRepair.status": "Failed",
    "onlineRepair.escalation": "FullRepair",
    "onlineRepair.failureReason": "GPU ECC errors persisted after online repair"
  }
}
```

Run:

```bash
restish nico-stg update-instance <tenant-org-id> <instance-id> < online-repair-failed-labels.json
```

Recommended labels:

| Label | Value | Purpose |
|---|---|---|
| `onlineRepair.status` | `Failed` | Shows that online repair was attempted and did not resolve the issue. |
| `onlineRepair.escalation` | `FullRepair` | Shows that the next step is full repair release. |
| `onlineRepair.failureReason` | Short reason | Captures the concise failure reason. Keep within the label value length limit. |

After the label update succeeds, release the instance using [Release Instance for Full Repair](release_instance_for_repair.md).

## Verification

Use the deployment's normal machine and instance inspection commands after each step. With Restish, the common pattern is:

```bash
restish nico-stg get-machine <tenant-org-id> <machine-id>
```

Check that:

- The machine still has the same assigned instance.
- The instance state is `Repairing` after enabling online repair.
- The instance state is `Ready` after disabling online repair.
- The online repair health alert is present while online repair is active and cleared after exit.

## Validation Rules

The update request must only contain one kind of machine update. Do not combine `onlineRepair` with label updates, maintenance mode updates, instance type updates, or `clearInstanceType`.

Entering online repair requires:

- `onlineRepair.enabled: true`
- `onlineRepair.policy.allowAutoInstanceDeletionOnFailure`
- All three acknowledgment fields set to `true`
- A valid `healthIssue`

Exiting online repair requires:

- `onlineRepair.enabled: false`
- No `healthIssue`
- No `onlineRepair.policy`
- No `onlineRepair.acknowledgments`

## Troubleshooting

| Error | Meaning | Action |
|---|---|---|
| `403 Forbidden` | Caller is not a provider admin or privileged tenant admin for this machine. | Use an account with the required tenant admin privileges and repair capability. |
| `Machine must be assigned to an Instance` | The machine has no active tenant instance. | Online repair is not applicable. Use the full repair workflow or provider repair process. |
| `Instance must be in Ready state to enter online repair` | The assigned instance is not eligible for online repair. | Wait for the instance to become `Ready`, or use full repair if it cannot recover. |
| `healthIssue is required when onlineRepair.enabled is true` | The enter request did not include issue details. | Add a valid `healthIssue` object. |
| `healthIssue, onlineRepair.policy, and onlineRepair.acknowledgments must not be set when exiting online repair` | The exit request included enter-only fields. | Use only `{ "onlineRepair": { "enabled": false } }`. |

If online repair fails or requires disruptive work, clear online repair first and then release the instance for full repair using [Release Instance for Full Repair](release_instance_for_repair.md).
