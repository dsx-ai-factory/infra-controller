<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# External Validation Phase 1 gRPC Contract

External validators use Core gRPC to discover held machines, open an attempt,
allocate a targeted tenant instance, record a result, and release that instance.
NICo owns the allocation gate. It clears the gate only after external validation
passes and normal instance cleanup and local Machine Validation succeed.
Repair and other health sources remain independent.

This is the contract milestone for [design PR #6506](https://github.com/dsx-ai-factory/infra-controller/pull/6506)
and [requirement #5431](https://github.com/dsx-ai-factory/infra-controller/issues/5431).
The authoritative wire definitions are [external-validation messages](../../crates/rpc/proto/external_validation.proto)
and [Forge RPC declarations](../../crates/rpc/proto/forge.proto). The `carbide-rpc`
build generates Rust client/server types and the `ForgeApiClient` wrapper from
these sources.

## Availability

Milestone 1 publishes protobuf contracts and generated Rust interfaces. The six
new RPC handlers return `UNIMPLEMENTED`; existing method authorization may reject
calls before they reach a handler. Publishing declarations grants no additional
permissions and does not create holds or attempts.

All six methods have explicit empty static RBAC grants; runtime tenant/site
authorization and any service grants belong to their implementation milestones.

A present `InstanceAllocationRequest.external_validation_request_id` returns
`FAILED_PRECONDITION` until request-bound allocation is implemented, including
an empty string and requests within `AllocateInstances`. Omission preserves
existing allocation behavior. The guard prevents ordinary allocation from
silently accepting a request that expects a durable validation binding.

Durable records and workflow handlers arrive in Milestone 2, request-bound
`AllocateInstance` in Milestone 3, lifecycle and recovery in Milestone 4, and
repair coexistence in Milestone 5. Enable the site policy only after the complete
Phase 1 workflow passes integration testing. DSX Exchange event enrichment
belongs to Phase 2. The following sections define the later handlers' contracts.

## Scope and authorization

Connect to the Core endpoint for the machine's site using its existing
certificate authentication and gRPC method permissions. The site is implicit
in the connected Core service. `tenant_org_id` is the configured Core tenant
organization, not a newly generated tenant for each attempt.

Each handler must verify that the authenticated caller is authorized to manage
external validation for that tenant at the connected site. Method permission
alone does not permit selecting an unrelated tenant. The hold, attempt, machine,
and allocated instance must all match the configured tenant and current cycle.
An opaque request UUID identifies an attempt; it never grants permission.
Another authorized caller for the same tenant can resume an attempt.

Allocation retains existing tenant, OS, network, resource, ownership, and
managed-state checks. The validation service cannot create, replace, or remove
the NICo-owned hold. Normal removal belongs to NICo; administrator break-glass
recovery requires a reason, audit, and resolution of any outstanding allocation
and cleanup before normal allocation resumes.

The site policy names the validation tenant by Core organization ID. A REST
adapter translates its tenant UUID to that organization ID and back; Core never
compares REST tenant UUIDs. Site selection belongs to the adapter's connection
routing, rather than a caller-supplied Core site ID.

## RPC surface

All methods are on `forge.Forge`.

| RPC | Request | Response |
| --- | --- | --- |
| `FindExternalValidationHoldIds` | `ExternalValidationHoldSearchFilter` | `ExternalValidationHoldIdList` |
| `FindExternalValidationHoldsByIds` | `ExternalValidationHoldsByIdsRequest` | `ExternalValidationHoldList` |
| `StartExternalValidation` | `StartExternalValidationRequest` | `StartExternalValidationResponse` |
| `CancelExternalValidation` | `CancelExternalValidationRequest` | `ExternalValidationAttempt` |
| `CompleteExternalValidation` | `CompleteExternalValidationRequest` | `ExternalValidationAttempt` |
| `RemoveExternalValidationHold` | `RemoveExternalValidationHoldRequest` | `google.protobuf.Empty` |
| `AllocateInstance` | Existing `InstanceAllocationRequest`, with optional `external_validation_request_id` field 14. | Existing `Instance`; unresolved creation returns `UNAVAILABLE`. |
| `ReleaseInstance` | Existing `InstanceReleaseRequest`, using the bound instance ID. | Existing `InstanceReleaseResult`. |

The allocation extension is additive. Request-bound batch allocation is outside
this contract. OS and network configuration stay in the existing
`InstanceAllocationRequest.config`; this workflow does not choose a validator's
image or network.

## Discovery and pagination

Discover IDs on startup and periodically afterward, then retrieve their current
state. Both calls require `tenant_org_id` selecting an authorized validation
scope at the connected site. They follow the Core `FindIds`/`FindByIds` convention.

Call `FindExternalValidationHoldIds` with `ExternalValidationHoldSearchFilter`.
`page_size` is optional, defaults to 20, and accepts 1 through 100; explicit zero
is invalid. Omit `after` to begin a pass. Results contain active `hold_ids`
ordered by immutable `created_at ASC`, then `hold_id ASC` in UUID byte order.
An empty authorized scope succeeds with an empty ID list.

When more matches exist at the read, `next_cursor` contains the last returned
ID's creation timestamp and UUID; otherwise it is absent. Continue with that
cursor in `after` and the same tenant filter. A supplied cursor requires both
fields; malformed timestamps or UUIDs return `INVALID_ARGUMENT`. The next page
selects tuples strictly greater than the cursor without looking up its hold.
Removing or satisfying an earlier hold cannot shift the next page or repeat an
already returned ID. A deleted cursor hold does not invalidate traversal.

Call `FindExternalValidationHoldsByIds` with the tenant scope and 1 through 100
hold UUIDs from each page. Malformed UUIDs reject the entire request with
`INVALID_ARGUMENT`. Duplicates are accepted and returned at most once. Results
are ordered by the same creation/UUID tuple. Unknown, satisfied, removed, and
out-of-tenant IDs are omitted without disclosing their state. An empty result
never proves allocation or cleanup completed.

These calls read live state, not a frozen snapshot. Hold state may change
between discovery and retrieval. Newly visible holds behind the current cursor
are picked up by the next reconciliation pass, which restarts without a cursor.
Do not persist the end cursor as a permanent discovery checkpoint.

Active lists include Pending, AttemptOpen, AwaitingCleanup, and Recovery holds;
Satisfied holds are excluded. Repair or another health blocker does not hide a
Pending hold. Each hold includes its machine, tenant organization, stable hold
and cycle UUIDs, state, creation time, and current attempt. `attempt` is absent
in Pending. An attempt's `validation_instance_id` is absent before creation and
retained after deletion for recovery and replay. Timestamps are UTC.

`recovery_reason` is required and nonempty in Recovery and absent in other
states. It explains the condition Core observed, such as lost instance,
ownership mismatch, failed cleanup, or failed post-release local validation.
It must not contain credentials or unredacted plugin output. Use hold discovery
and retrieval to find pending work; an allocation-blocking health alert can make
a held machine appear unhealthy in other API representations.

## Opening an attempt

Persist a caller-generated UUID before calling Start with the exact
`machine_id`, `tenant_org_id`, and `caller_idempotency_key`. NICo scopes the key
to the connected site and configured tenant, locks the hold, and opens at most
one attempt. These response statuses are distinct from persisted attempt states:

| Status | Meaning |
| --- | --- |
| `Opened` | A new attempt (`replayed: false`) or replay of the original keyed attempt (`replayed: true`), including its terminal state and original hold. |
| `AlreadyOpen` | A different key found an open attempt; use the returned identity to resume it. |
| `NoActiveHold` | No eligible hold exists; no attempt is created, and `attempt` is absent. An existing active but ineligible hold always supplies `hold_id`; it is absent when no active hold exists. |

`replayed` is false for AlreadyOpen and NoActiveHold. An adapter can distinguish
new creation from replay without comparing timestamps or guessing from attempt
state. Replay returns the original hold ID even after that hold is satisfied.

Idempotent replay precedes current machine eligibility checks. Reusing a key for
another machine conflicts; a key never rebinds to a later hold or cycle. A new
attempt requires a Pending hold and a Ready, unassigned host with no
`RequestRepair` or unrelated blocking health alerts. AwaitingCleanup and
Recovery reject a new attempt until the previous allocation is resolved.

## Allocation and recovery

Persist the returned request UUID before calling `AllocateInstance`. Supply
that UUID in `external_validation_request_id`, the exact held `machine_id`,
`allow_unhealthy_machine: true`, and a config whose
`tenant.tenant_organization_id` matches the policy. The validator supplies its
normal OS and network configuration through the existing config fields.
`instance_type_id` must be omitted; request-bound allocation targets an exact
machine, and request-bound batch allocation is rejected.

NICo records Creating and accepted allocation inputs before dispatching
allocation, then commits the instance and its request binding together. An
identical retry returns the original `Instance`, including its generated ID
when `instance_id` was omitted. Changing accepted inputs returns
`ALREADY_EXISTS`. While allocation is Creating, return `UNAVAILABLE` with ASCII
trailing metadata `external-validation-allocation-state: creating`. This exact
marker identifies accepted allocation in progress without parsing error text;
ordinary transient failures do not include it. An adapter must capture and
propagate the marker through its gRPC boundary. Its absence never proves that
allocation was not accepted. The caller
retries the same request and inputs or fetches its hold by ID for allocation state
and the saved instance ID. It must not generate a new attempt to recover an
ambiguous outcome.

After any allocation rejection, reconcile the saved attempt's allocation state.
NotStarted permits correcting inputs or pre-allocation cancellation; Creating
or Created requires recovery using the same identity and accepted inputs.

The request permits bypass of this workflow's hold only. NICo rechecks unrelated
blocking health alerts, repair admission, ownership, and provisionability at
allocation commit. Labels cannot establish a binding. A closed or superseded
request cannot allocate another instance, even after its original instance was
released.

## Cancellation and completion

Cancel requires `request_id`, `tenant_org_id`, and a nonempty `details` reason.
Only an Open attempt with allocation state NotStarted can be cancelled. Record
Cancelled, close the attempt, and keep the hold Pending. Identical cancellation
replay returns the original Cancelled attempt, including after a newer attempt
opens, without modifying the newer attempt or its hold. Replay lookup precedes
current-attempt checks. Changed details or a stale request without a matching
recorded cancellation conflicts.
Creating and Created must be reconciled or completed and released instead.

Complete requires `request_id`, `tenant_org_id`, the bound Core
`validation_instance_id`, and outcome Passed, Failed, or Cancelled. Unspecified
or unknown outcomes are invalid. The request must match the active attempt,
current cycle, configured tenant, and saved instance binding in allocation
state Created. Identical terminal replay succeeds even after instance deletion;
a changed instance, result, or details conflicts.

Completion `details` is optional: omission is distinct from an explicitly empty
string and replay must preserve that presence. Both completion and cancellation
accept at most 4096 UTF-8 bytes of details; cancellation requires at least one
byte. These limits apply to multibyte text as well as ASCII.

For every terminal outcome, the validator must call `ReleaseInstance` using the
bound instance ID. A recorded result moves the hold to AwaitingCleanup. Passed
satisfies the hold only after successful cleanup and local validation, and only
if repair has not invalidated that attempt. Failed or Cancelled returns to
Pending after successful cleanup. Missing results, unexpected deletion,
ownership mismatch, or failed cleanup/local validation never satisfy the hold.
Deletion alone is not success.

Holds and attempts have no external-validation expiry. The validator owns its
execution deadline, including work lasting days, reports a result, and releases
its instance. Existing Core allocation and cleanup recovery still apply. A
missing service or result never releases capacity.

## Administrator recovery and hold protection

`RemoveExternalValidationHold` requires site-administrator break-glass
authorization, the exact host machine ID, the expected hold UUID, and a reason
of 1 through 4096 UTF-8 bytes. Validation-tenant permissions alone never permit
removal. Verify that outstanding allocation and cleanup are resolved; otherwise
return `FAILED_PRECONDITION`. Removal atomically closes or fences any attempt,
removes only the matching hold's allocation gate, and records the caller and
reason in the audit trail. It does not record a validation pass.

Identical replay of the recorded machine, hold, and reason succeeds, including
after a newer hold is created, without changing that newer hold. A changed
recorded reason returns `ALREADY_EXISTS`. An unknown hold returns `NOT_FOUND`;
a known hold belonging to another machine or superseded without a matching
removal receipt returns `ALREADY_EXISTS`. Hold IDs never rebind to another cycle.

Milestone 4 must reserve the configured hold health-report source against
`InsertMachineHealthReport` and `RemoveMachineHealthReport`, including generic
calls by privileged tenants or administrators. Reject explicit modifications
with `PERMISSION_DENIED`; normal workflow reconciliation and the audited
break-glass RPC own its changes. Match the configured source rather than a
hard-coded alert ID. These protections and runtime removal arrive with lifecycle
integration, before enabling the policy; Milestone 1 adds only the removal stub.

This contract grants no access to local Machine Validation run/results or
unredacted stdout/stderr. Any expansion of tenant access needs a separate
authorization and output-redaction decision.

## Status codes

| gRPC code | Meaning and recovery |
| --- | --- |
| `INVALID_ARGUMENT` | Required input is missing, a UUID is malformed, pagination/details bounds are exceeded, or an outcome is unspecified or unknown. Correct the request. |
| `UNAUTHENTICATED` | Authentication failed. Use valid credentials. |
| `PERMISSION_DENIED` | Caller lacks method or tenant/site authorization. Request identity is not sufficient. |
| `NOT_FOUND` | Machine or request does not exist in the authorized scope. Verify identity; absence of a hold instead returns `NoActiveHold`. |
| `ALREADY_EXISTS` | Idempotency key targets another machine, allocation inputs changed, a stale request or terminal result conflicts, or a break-glass identity/reason conflicts. Preserve the original operation identity and resolve the conflict. |
| `FAILED_PRECONDITION` | Allocation, cleanup, recovery, or attempt state blocks the requested mutation. Reconcile the existing work first. Milestone 1 also uses this code for unsupported request-bound allocation. |
| `UNAVAILABLE` | Site operation is transiently unavailable. With trailing metadata `external-validation-allocation-state: creating`, allocation is accepted and in progress. Replay only the same mutation identity and inputs. |
| `DEADLINE_EXCEEDED` | The call timed out; a mutation may have committed. Reconcile or replay the same identity. |
| `UNIMPLEMENTED` | The workflow handlers are contract declarations in Milestone 1. |

Do not infer that a timed-out allocation failed or that an empty inventory proves
no instance was committed. Resolve allocation and cleanup, then fence or close
the old attempt before opening a new one. Repair requested during an attempt
prevents that attempt from satisfying the hold; fresh validation follows
successful repair.
