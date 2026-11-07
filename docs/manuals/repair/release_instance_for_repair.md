# Release Instance for Full Repair

Releasing an instance for full repair removes the tenant assignment and lets NICo clean, quarantine, and route the underlying machine through the full repair workflow. Use this workflow when the tenant cannot keep using the instance, when online repair failed, or when repair requires disruptive work such as rebooting, reimaging, component replacement, or deep diagnostics.

For non-disruptive repair while the tenant keeps the instance, see [Online Repair](online_repair.md).

If the instance is already in online repair, clear online repair before using this workflow. NICo does not allow releasing an instance for full repair while the instance is still in online repair.

## Audience and Access

This page is intended for tenant admins and platform operators writing tenant-facing runbooks.

The caller must be a tenant admin for the organization that owns the instance. The instance must belong to the tenant organization in the request.

## What Full Repair Release Does

When the tenant releases an instance with a machine health issue:

1. The REST API marks the instance as terminating.
2. The site workflow releases the instance from the machine.
3. NICo runs normal cleanup and tenant sanitization for the machine.
4. The reported issue is sent to NICo Core.
5. A `tenant-reported-issue` health override is applied so the machine is not used for normal tenant allocation.
6. If auto-repair is enabled, a repair request is also applied so the repair system can claim and work on the machine.

This workflow is intentionally different from online repair. The tenant gives up the instance, and the machine returns to the available pool only after repair and validation are complete.

## REST and Restish

The REST API operation is:

```text
DELETE /v2/org/{org}/nico/instance/{instanceId}
```

The OpenAPI operation ID is `delete-instance`. In tenant runbooks, this operation is commonly described as "release instance" because it releases the tenant's instance and starts cleanup.

Restish exposes operation IDs as commands, so the command shape is:

```bash
restish <api-profile> delete-instance <tenant-org-id> <instance-id> < <request-body-json>
```

For example, in staging:

```bash
restish nico-stg delete-instance <tenant-org-id> <instance-id> < release-for-repair.json
```

`nico-stg` is the Restish API profile or environment. Replace it with the profile for your deployment.

Use Restish help to confirm the operation signature in the target environment:

```bash
restish nico-stg delete-instance --help
```

The `delete-instance` operation accepts an optional JSON request body. When releasing for repair, include the body with shell redirection (`< release-for-repair.json`) so the machine health issue is reported with the release.

Restish prints the HTTP status and JSON error body when a request fails. Use that response body when troubleshooting validation, permission, or workflow errors.

## Before You Start

Collect the following values:

| Value | Description |
|---|---|
| `<api-profile>` | Restish profile, for example `nico-stg`. |
| `<tenant-org-id>` | Tenant organization identifier used by the REST API. |
| `<instance-id>` | Instance UUID to release. |

Confirm these preconditions:

- The caller is a tenant admin for the owning tenant organization.
- The instance belongs to the tenant organization in the request.
- The tenant understands that this releases the instance and the same instance does not return to `Ready`.
- The issue requires full repair or online repair has failed.
- Online repair is not active on the instance. If online repair is active, clear online repair first and confirm the instance has returned to `Ready`.
- If online repair failed, the instance labels have been updated to show the failure before release.

## Label Failed Online Repair Before Release

When full repair follows a failed online repair attempt, update the instance labels before release. This makes the failed online repair status visible in tenant and operator tooling while the release is being processed.

First fetch the instance and preserve any labels that should remain:

```bash
restish nico-stg get-instance <tenant-org-id> <instance-id>
```

Instance label updates replace the full label map. Labels not included in the update request are removed. Labels are limited to 16 key/value pairs, so use the minimum failure labels if the instance is already near that limit.

Example `online-repair-failed-labels.json`:

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

Use the `update-instance` operation:

```bash
restish nico-stg update-instance <tenant-org-id> <instance-id> < online-repair-failed-labels.json
```

Use at least `onlineRepair.status: Failed`. Add `onlineRepair.escalation` and `onlineRepair.failureReason` when label capacity permits.

## Release the Instance with a Repair Issue

Create `release-for-repair.json`:

```json
{
  "machineHealthIssue": {
    "category": "Hardware",
    "summary": "GPU diagnostics show intermittent ECC errors",
    "details": "Tenant observed intermittent GPU ECC errors during workload execution. Online repair did not resolve the issue, so the instance is being released for full repair."
  }
}
```

Run:

```bash
restish nico-stg delete-instance <tenant-org-id> <instance-id> < release-for-repair.json
```

Expected result:

- The API returns `202 Accepted`.
- The instance moves to a terminating or deleting state.
- The assigned machine is released from the tenant after cleanup.
- The reported machine issue is recorded for repair handling.
- The machine is prevented from normal tenant allocation until the issue is resolved.

## Issue Fields

`machineHealthIssue` is optional for a normal instance release, but it should be provided when releasing for repair.

| Field | Requirement |
|---|---|
| `category` | Required when `machineHealthIssue` is present. Must be one of `Hardware`, `Network`, `Performance`, or `Other`. |
| `summary` | Required when `machineHealthIssue` is present. Maximum 1024 characters. |
| `details` | Recommended. Maximum 1024 characters. |

Use the issue details to include observed symptoms, workload impact, timestamps, component identifiers, and any online repair attempt that already occurred.

## Repair Tenant Flag

Normal tenants should omit `isRepairTenant` or set it to `false`.

```json
{
  "machineHealthIssue": {
    "category": "Hardware",
    "summary": "GPU diagnostics show intermittent ECC errors",
    "details": "Tenant observed intermittent GPU ECC errors during workload execution."
  },
  "isRepairTenant": false
}
```

`isRepairTenant: true` is for repair tenants that are releasing a machine after investigation or repair. It requires effective targeted instance creation for the tenant, which a Provider Admin grants on the tenant account; see [Granting Targeted Instance Creation](../../configuration/tenant_management.md#granting-targeted-instance-creation) for what makes it effective. Do not set this flag for the original tenant's release-for-repair request.

## After Release

After the release request is accepted:

- The tenant should stop using the instance and expect it to terminate.
- The tenant should create or request a replacement instance according to the tenant's normal capacity workflow.
- The repair system or provider operations team handles diagnostics and repair. Platform admins and repair tenants should use [Repair Tenant Workflow](repair_tenant_workflow.md) to claim the machine with targeted instance creation, repair it, set `repair_status`, and release it back to NICo.
- A successfully repaired machine returns to the available pool after repair completion and validation.

The original tenant does not move this instance back to `Ready`. Returning the same assignment to `Ready` is the online repair workflow, not the full repair release workflow.

## Verification

Use the deployment's normal instance and machine inspection commands. With Restish, the common pattern is:

```bash
restish nico-stg get-instance <tenant-org-id> <instance-id>
```

Check that:

- The instance moves to terminating or is eventually removed.
- The machine is not available for normal allocation while the repair issue is active.
- The reported issue is visible to provider or repair operations.

Provider or repair operations can also inspect machine health overrides with the admin tooling described in [Repair System Integration](repair_integration.md).

## Troubleshooting

| Error | Meaning | Action |
|---|---|---|
| `403 Forbidden` | Caller is not a tenant admin for the owning tenant organization. | Use an account with the tenant admin role for the org that owns the instance. |
| `404 Not Found` | The instance ID does not exist or is not visible to the tenant. | Verify the instance UUID and tenant organization. |
| `Org specified in request does not match Org of Tenant associated with Instance` | The request used the wrong tenant organization. | Retry with the owning tenant org. |
| `Error validating Instance deletion request data` | The request body has an invalid issue category or field length. | Use one of the supported categories and keep summary/details within limits. |
| Instance is in online repair | The instance is still in `Repairing` because online repair is active. | Clear online repair first, confirm the instance is back in `Ready`, and then retry the full repair release. |
| `Site is not in Registered state` | The site cannot currently process the release workflow. | Escalate to provider operations. |
| `Instance delete workflow timed out` | The site workflow did not finish within the API timeout. | Check workflow status and site connectivity before retrying. |

If the tenant still needs to keep the instance assigned and the repair can be attempted without release, use [Online Repair](online_repair.md) instead.
