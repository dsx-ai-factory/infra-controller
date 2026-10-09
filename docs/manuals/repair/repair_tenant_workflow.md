# Repair Tenant Workflow

This runbook is for platform admins, repair tenant admins, and repair automation owners who pick up a machine after a tenant has released it for full repair. The repair tenant claims the exact machine with targeted instance creation, performs diagnostics and repair, records the repair outcome on the machine, and releases the repair instance so NICo can decide whether the machine returns to the ready pool or stays quarantined for failed repair handling.

For the original tenant release path, see [Release Instance for Full Repair](release_instance_for_repair.md). For the lower-level health override behavior, see [Repair System Integration](repair_integration.md).

## Audience and Access

The caller needs tenant admin access for the dedicated repair tenant, and the repair tenant must have effective targeted instance creation at the site being repaired. A Provider Admin grants that on the repair tenant's tenant account; see [Granting Targeted Instance Creation](../../configuration/tenant_management.md#granting-targeted-instance-creation) for what makes it effective. The repair tenant also needs access to the site, VPC, operating system, and network resources used for repair instances.

The repair tenant release path uses `isRepairTenant: true`. The REST API only accepts this flag from tenants with targeted instance creation capability.

## Workflow Summary

The platform repair workflow is:

1. Identify a released machine that has repair signals such as `tenant-reported-issue` and `repair-request`.
2. Create a repair instance in the dedicated repair tenant by targeting the exact `machineId`.
3. Use `allowUnhealthyMachine: true` when the machine is repair-eligible but blocked from normal allocation by status or health.
4. Set the machine label `repair_status: InProgress` so any stale status from an older repair cannot be reused accidentally.
5. Diagnose and fix the machine.
6. Update the machine labels with the final repair outcome, especially `repair_status`.
7. Release the repair tenant instance with `isRepairTenant: true`.
8. NICo clears or reapplies repair health overrides based on the machine label and release payload.

## REST and Restish

The main REST operations are:

| Step | REST operation | Restish operation |
|---|---|---|
| Create targeted repair instance | `POST /v2/org/{org}/nico/instance` | `create-instance` |
| Inspect machine | `GET /v2/org/{org}/nico/machine/{machineId}` | `get-machine` |
| Update machine labels | `PATCH /v2/org/{org}/nico/machine/{machineId}` | `update-machine` |
| Release repair instance | `DELETE /v2/org/{org}/nico/instance/{instanceId}` | `delete-instance` |

Restish exposes OpenAPI operation IDs as commands. The commands use the path arguments from the REST operation and accept JSON bodies through shell redirection:

```bash
restish <api-profile> create-instance <repair-tenant-org-id> < repair-instance.json
restish <api-profile> update-machine <repair-tenant-org-id> <machine-id> < repair-status.json
restish <api-profile> delete-instance <repair-tenant-org-id> <repair-instance-id> < repair-release.json
```

For example, in staging:

```bash
restish nico-stg create-instance <repair-tenant-org-id> < repair-instance.json
```

`nico-stg` is the Restish API profile or environment. Replace it with the profile for your deployment.

Use Restish help to confirm operation signatures in the target environment:

```bash
restish nico-stg create-instance --help
restish nico-stg update-machine --help
restish nico-stg delete-instance --help
```

Restish prints the HTTP status and JSON error body when a request fails. Use that response when troubleshooting validation, permission, or workflow errors.

## Before You Start

Collect the following values:

| Value | Description |
|---|---|
| `<api-profile>` | Restish profile, for example `nico-stg`. |
| `<repair-tenant-org-id>` | Organization identifier for the dedicated repair tenant. |
| `<repair-tenant-id>` | Tenant UUID used in the create instance request body. |
| `<machine-id>` | Machine ID being repaired. |
| `<repair-vpc-id>` | VPC UUID used by repair instances. |
| `<repair-vpc-prefix-id>` | VPC prefix UUID for the repair interface. |
| `<repair-os-id>` | Operating system UUID or an approved iPXE script for repair work. |
| `<repair-instance-id>` | Repair instance UUID returned by targeted instance creation. |

Confirm these preconditions:

- The original tenant has released the instance for full repair.
- Online repair is not active for the original tenant instance.
- The machine is not assigned to another instance.
- The machine has `tenant-reported-issue` and `repair-request` overrides if the repair tenant release is expected to route the machine automatically back to the ready pool or repair-failed handling.
- The repair tenant has targeted instance creation capability.
- The repair tenant has site access and can see the target machine.
- The machine controller state is still provisionable. `allowUnhealthyMachine: true` can target machines that are not normal-allocation ready, but it does not bypass missing machines, already-assigned machines, or controller states that cannot provision an instance.

When auto-repair is disabled, the original tenant release applies `tenant-reported-issue` but not `repair-request`. In that case, provider operations must either manually add `repair-request` before the repair tenant workflow or manually clear the resolved `tenant-reported-issue` after a successful repair. Without `repair-request`, a successful repair tenant release with no new issue does not clear `tenant-reported-issue`.

## Claim the Machine

Create `repair-instance.json`:

```json
{
  "name": "repair-<machine-id>",
  "description": "Repair instance for machine <machine-id>",
  "tenantId": "<repair-tenant-id>",
  "machineId": "<machine-id>",
  "vpcId": "<repair-vpc-id>",
  "operatingSystemId": "<repair-os-id>",
  "allowUnhealthyMachine": true,
  "interfaces": [
    {
      "vpcPrefixId": "<repair-vpc-prefix-id>",
      "isPhysical": true
    }
  ],
  "labels": {
    "repair.workflow": "MachineRepair",
    "repair.machineId": "<machine-id>",
    "repair.ticket": "INC-12345"
  }
}
```

Run:

```bash
restish nico-stg create-instance <repair-tenant-org-id> < repair-instance.json
```

Expected result:

- The API returns the repair instance.
- The repair instance is assigned to the requested machine.
- The machine remains blocked from normal tenant allocation while repair health overrides are active.

Use the repair instance only for diagnostics, firmware work, component validation, and other repair activity. Do not use the repair tenant as a normal workload tenant.

## Mark Repair In Progress

After the repair instance is created, mark the machine as actively being repaired. This avoids a stale `repair_status: Completed` label from an older repair being interpreted as the outcome for the current repair attempt.

First inspect the machine and preserve any labels that should remain:

```bash
restish nico-stg get-machine <repair-tenant-org-id> <machine-id>
```

Create `repair-status-in-progress-labels.json` using the existing labels plus `repair_status: InProgress`:

```json
{
  "labels": {
    "RackIdentifier": "GVX11F01C02",
    "repair_status": "InProgress",
    "repair.ticket": "INC-12345",
    "repair.summary": "Repair tenant diagnostics started"
  }
}
```

Run:

```bash
restish nico-stg update-machine <repair-tenant-org-id> <machine-id> < repair-status-in-progress-labels.json
```

## Perform and Validate Repair

Run the repair procedure required by the issue. Use the repair ticket or tenant-reported issue details to preserve the failure context. Before release, validate the machine enough to decide whether it is safe to return to tenant allocation.

The final release decision is controlled by the machine label `repair_status`, not by the repair instance label. Set this label on the machine before releasing the repair instance.

Supported values are case-insensitive:

| Machine label | Meaning | NICo release result |
|---|---|---|
| `repair_status: Completed` | Repair succeeded and validation passed. | If the release has no new issue, NICo removes `repair-request` and `tenant-reported-issue`, allowing the machine to return to the ready pool. |
| `repair_status: Failed` | Repair did not resolve the issue. | NICo removes `repair-request` and applies or keeps `tenant-reported-issue`, keeping the machine in repair or failed handling. |
| `repair_status: InProgress` | Repair work is not complete. | NICo treats the release as incomplete and keeps the machine out of the ready pool. |
| Missing or unknown value | No trusted completion signal. | NICo creates a fallback incomplete-repair issue and keeps the machine out of the ready pool. |

## Set Repair Outcome Labels

Before releasing the repair instance, inspect the machine again and preserve any labels that should remain:

```bash
restish nico-stg get-machine <repair-tenant-org-id> <machine-id>
```

Machine label updates replace the full label map. Labels not included in the update request are removed. Labels are limited to 16 key/value pairs, so keep repair labels short and preserve required placement labels such as rack, site, or pool hints.

When repair succeeds, create `repair-status-completed-labels.json`:

```json
{
  "labels": {
    "RackIdentifier": "GVX11F01C02",
    "repair_status": "Completed",
    "repair.ticket": "INC-12345",
    "repair.summary": "GPU riser replaced and validation passed"
  }
}
```

Run:

```bash
restish nico-stg update-machine <repair-tenant-org-id> <machine-id> < repair-status-completed-labels.json
```

When repair fails or the machine must not return to the ready pool, create `repair-status-failed-labels.json`:

```json
{
  "labels": {
    "RackIdentifier": "GVX11F01C02",
    "repair_status": "Failed",
    "repair.ticket": "INC-12345",
    "repair.summary": "GPU ECC errors persist after riser replacement"
  }
}
```

Run:

```bash
restish nico-stg update-machine <repair-tenant-org-id> <machine-id> < repair-status-failed-labels.json
```

Use `repair_status: Completed` only after the repair team has validated that the machine can safely re-enter normal allocation. Use `repair_status: Failed` when the machine should move to repair-failed or manual intervention handling.

## Release a Successfully Repaired Machine

After setting `repair_status: Completed`, create `repair-release-completed.json`:

```json
{
  "isRepairTenant": true
}
```

Run:

```bash
restish nico-stg delete-instance <repair-tenant-org-id> <repair-instance-id> < repair-release-completed.json
```

Expected result:

- The API returns `202 Accepted`.
- NICo releases the repair instance.
- NICo removes `repair-request`.
- NICo removes `tenant-reported-issue` because no new issue was reported.
- The machine becomes eligible for the normal ready pool after cleanup and validation.

## Release a Machine That Still Needs Repair

If repair failed, validation failed, or the machine should not return to normal allocation, set `repair_status: Failed` and include a machine health issue in the repair release.

Create `repair-release-failed.json`:

```json
{
  "isRepairTenant": true,
  "machineHealthIssue": {
    "category": "Hardware",
    "summary": "Repair failed: GPU ECC errors persist",
    "details": "Repair tenant replaced the GPU riser, but validation still reports ECC errors. Keep the machine out of the ready pool for provider intervention."
  }
}
```

Run:

```bash
restish nico-stg delete-instance <repair-tenant-org-id> <repair-instance-id> < repair-release-failed.json
```

Expected result:

- The API returns `202 Accepted`.
- NICo releases the repair instance.
- NICo removes `repair-request` so automated repair does not loop on the same machine.
- NICo applies or keeps `tenant-reported-issue`.
- The machine stays out of the normal ready pool and is routed to repair-failed or manual intervention handling.

If the repair tenant releases the instance with `repair_status: Failed`, `InProgress`, missing, or unknown and does not provide `machineHealthIssue`, NICo creates a fallback issue with summary `RepairSystem processing incomplete`.

If a repair tenant releases a machine that no longer has a `repair-request` override, NICo does not create a new automated repair loop. A release with a new `machineHealthIssue` applies `tenant-reported-issue`; a release with no issue takes no health override action and does not clear an existing `tenant-reported-issue`.

## Completion Matrix

For machines that still have a `repair-request` override, NICo uses the following release behavior:

| Machine label before release | Release issue provided | Result |
|---|---|---|
| `repair_status: Completed` | No | Machine can return to the ready pool. |
| `repair_status: Completed` | Yes | Machine remains blocked by the new `tenant-reported-issue`. |
| `repair_status: Failed` | Optional | Machine remains blocked by `tenant-reported-issue`. |
| `repair_status: InProgress` | Optional | Machine remains blocked as incomplete repair. |
| Missing or unknown `repair_status` | Optional | Machine remains blocked as incomplete repair. |

## Verification

After releasing the repair instance, inspect the machine and health overrides:

```bash
restish nico-stg get-machine <repair-tenant-org-id> <machine-id>
```

Check that:

- The repair instance is terminating or deleted.
- `repair-request` is removed after the repair tenant release.
- `tenant-reported-issue` is removed only for successful repair completion with no new issue.
- A failed, incomplete, or unknown repair still keeps the machine unavailable for normal tenant allocation.

Provider tooling can also inspect the lower-level health overrides described in [Repair System Integration](repair_integration.md).

## Troubleshooting

| Error | Meaning | Action |
|---|---|---|
| `Tenant does not have capability to create Instances using specific Machine ID` | Targeted instance creation is not effective for the repair tenant at this site: the capability is unset, the tenant account is not `Ready`, or a site override disables it. | Have a Provider Admin [grant the capability](../../configuration/tenant_management.md#granting-targeted-instance-creation) on the repair tenant's account, or use the correct repair tenant. |
| `Machine is not in Ready state, but it can be provisioned by setting allowUnhealthyMachine to true` | The machine can be targeted for repair but the create request omitted `allowUnhealthyMachine`. | Add `allowUnhealthyMachine: true` and retry. |
| `Machine is assigned to an Instance` | The machine is still assigned elsewhere. | Confirm the original tenant release completed before claiming the machine. |
| `Tenant does not have capability to set IsRepairTenant` | The release caller is not a targeted-instance-capable tenant. | Release from the repair tenant or use a properly privileged tenant. |
| Successful repair release does not return the machine to ready | The machine did not have `repair-request` when the repair tenant released it. | Manually clear the resolved `tenant-reported-issue`. For future attempts, add `repair-request` before the repair tenant release if automatic routing is expected. |
| Machine returns to failed handling after a successful repair | `repair_status` was missing, not `Completed`, or the release included a new `machineHealthIssue`. | Inspect machine labels, fix `repair_status`, and verify the release payload. |

Do not clear repair health overrides manually unless the repair outcome is known. The repair tenant release path exists so NICo can make the ready-pool decision from the recorded repair outcome.
