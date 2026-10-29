<!--
SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
SPDX-License-Identifier: Apache-2.0
-->

# External Validation Phase 1 gRPC Contract

Core gRPC defines the site contract used by REST adapters and authorized Core
callers to discover held machines, open an attempt, allocate a targeted tenant
instance, record a result, and release that instance. External validation clients
integrate through REST; publishing these protos unblocks the REST implementation.
NICo owns the allocation gate. It clears the gate only after external validation
passes and normal instance cleanup and local Machine Validation succeed.
Repair and other health sources remain independent.

This is the contract milestone for [design PR #6506](https://github.com/dsx-ai-factory/infra-controller/pull/6506)
and [requirement #5431](https://github.com/dsx-ai-factory/infra-controller/issues/5431).
The companion [REST surface PR #8147](https://github.com/dsx-ai-factory/infra-controller/pull/8147)
defines the end-user routes, OpenAPI schemas, and SDK types on top of this
Core contract.
The authoritative wire definitions are [external-validation messages](../../crates/rpc/proto/external_validation.proto)
and [Forge RPC declarations](../../crates/rpc/proto/forge.proto). The `carbide-rpc`
build generates Rust client/server types and the `ForgeApiClient` wrapper from
these sources.

## Availability

Milestone 1 publishes protobuf contracts and generated Rust interfaces. The six
new RPC handlers return `UNIMPLEMENTED`; existing method authorization may reject
calls before they reach a handler. Publishing declarations grants no additional
permissions and does not create holds or attempts.

All six methods have explicit empty static RBAC grants. With internal RBAC
enabled, callers with an `AuthContext` receive `PERMISSION_DENIED` before a
handler runs. Missing `AuthContext` is also rejected before dispatch; this
contract does not promise a gRPC wire status for that branch. With RBAC bypassed,
the stubs return `UNIMPLEMENTED`. Runtime resource/policy checks and service
grants belong to the implementation milestones. That implementation must grant
`SiteAgent` all six RPCs, including `RemoveExternalValidationHold`, for REST
proxy calls; Milestone 1 deliberately grants none.

A present `InstanceAllocationRequest.external_validation_request_id` returns
`FAILED_PRECONDITION` until request-bound allocation is implemented, including
a present default UUID and requests within `AllocateInstances`. Omission preserves
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

REST authenticates the end user, checks their role and site/Tenant access,
and records that user in REST audit entries before calling Core as `SiteAgent`.
Tenant calls supply the caller's organization; Provider Admin inventory may omit
the tenant filter to discover holds across the site. REST requires Provider
Admin authorization for break-glass removal. Core sees the service identity,
so it cannot infer the original user's role or enforce that REST role check.

Core authorizes and audits its authenticated service or direct operator caller
through explicit method/site permissions, then checks resource bindings and
the configured validation tenant for mutations. The hold, attempt, machine, and
allocated instance must match. New mutations require the current cycle;
identical replay refers to the original saved operation. Direct Core access is
restricted to authorized callers, not anonymous tenant-selected scope. An opaque
request UUID never grants permission. Another REST-authorized caller for the
same tenant may resume an attempt.

Allocation retains existing tenant, OS, network, resource, ownership, and
managed-state checks. The validation service cannot create, replace, or remove
the NICo-owned hold. Normal removal belongs to NICo; administrator break-glass
recovery requires a reason, audit, and resolution of any outstanding allocation
and cleanup before normal allocation resumes.

The site policy uses `validation_tenant_org_id`, the `org` field from
`GET /v2/org/{org}/nico/tenant/current`, also used as `{org}` in REST paths.
REST sends it as `organization_id` when creating the Core Tenant. The response's
`id` is a separate REST Tenant UUID used by REST Instance create as `tenantId`;
the adapter resolves that UUID to the organization ID. Core never compares a
REST Tenant UUID with its tenant organization ID. Site selection belongs to
connection routing, rather than a caller-supplied Core site ID.

When site policy is implemented in Milestone 4, publish its effective validation
tenant organization IDs through `RuntimeConfig` alongside policy configuration.
REST Site inventory must retain them for an early validation-Tenant check.
That check supplements REST authorization and Core binding checks. Missing
policy advertisement from an older build is unknown, not proof of authorization
or an authoritative empty policy. This contract milestone adds no policy field
or runtime advertisement before policy configuration exists.

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

## Resource identities

`common.ExternalValidationHoldId`, `common.ExternalValidationCycleId`, and
`common.ExternalValidationRequestId` are distinct UUID message types. They follow
the existing `common.InstanceId` convention: each contains a `value` UUID string,
and Rust uses the corresponding `carbide_uuid::external_validation` type.
All hold/cycle/request references, including the allocation binding, use these
types. The caller-generated `caller_idempotency_key` stays a
separate UUID string; it is not a NICo request ID.

For example, the protobuf JSON representation of a request reference is
`"requestId": {"value": "<request-uuid>"}`. The future REST create field below is
a scalar UUID string translated into this Core message by the REST adapter.

## Discovery and pagination

Discover IDs on startup and periodically afterward, then fetch their current
state in batches. Both calls accept optional `tenant_org_id`: a supplied value
filters exactly by Core organization ID; a present empty string is invalid.
Omission searches across tenants at the connected site for an authorized Core
caller. REST always supplies the caller's org for Tenant requests; Provider
Admin inventory may omit it. Omission never bypasses method/site authorization.

`FindExternalValidationHoldIds` returns all matching `hold_ids` in one response,
ordered by immutable `created_at ASC`, then `hold_id ASC` in UUID byte order.
This follows the existing Core `FindIds`/`FindByIds` batching convention. There
is no ID-page limit, offset, cursor, or continuation token. Optional `machine_id`
selects one exact host; omission includes all hosts in the selected scope.
Malformed or DPU IDs are invalid; an unknown host returns no matches. Optional
`state` selects one state, including Satisfied. Omission includes Pending,
AttemptOpen, AwaitingCleanup, and Recovery; UNSPECIFIED and unknown enum values
are invalid. All supplied filters combine with AND. An empty match set succeeds.

Split those IDs into nonempty `FindExternalValidationHoldsByIds` batches no
larger than the site's `RuntimeConfig.max_find_by_ids` setting, whose default
is 100. This configured limit, rather than a fixed contract limit, applies to
the submitted list including duplicates. Oversized batches or malformed UUIDs
reject the whole request with `INVALID_ARGUMENT`; duplicates are returned at
most once. Results use the same creation/UUID ordering. Satisfied holds remain
readable. Unknown, removed, and tenant-filter-mismatched IDs are omitted without
disclosing their state. Retrieval does not apply the discovery state filter:
a hold becoming Satisfied between the calls is returned with its new state.

The ID response and subsequent detail batches are separate live observations,
not a frozen snapshot. A disappearing hold cannot shift an offset or cursor in
the saved ID list, but its details may be omitted later. Newly matching holds
are discovered on the next full ID lookup. Repeat discovery periodically; an
empty result never proves allocation or cleanup completed.

Default discovery includes active holds only; explicit state filtering and
by-ID retrieval can also return Satisfied holds. Repair or another health
blocker does not hide a Pending hold. Each hold includes its machine, tenant organization, stable hold
and cycle UUIDs, state, creation time, and current attempt. Satisfied holds remain
available for the lifecycle of the retained hold record; this phase defines no
automatic hold expiry. By-ID retrieval does not expose a complete attempt history. `attempt` is absent
in Pending. An attempt's `validation_instance_id` is absent before creation and
retained after deletion for recovery and replay. Timestamps are UTC.
Every hold includes `state_changed_at`, initially equal to `created_at` and
advanced only on a hold-state transition. It measures time in AwaitingCleanup
or Recovery; allocation/attempt changes that preserve the hold state do not
advance it. A terminal attempt includes `closed_at`, set once when its result
is recorded; Open attempts omit it. Replay and instance deletion preserve it.

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
when `instance_id` was omitted. Accepted inputs include `machine_id`, config,
metadata, allocation flags, and `instance_id` when supplied. The supplied ID
and its presence are part of replay identity: the same request ID with a
different `instance_id` returns `ALREADY_EXISTS`. When it is omitted, Core saves
and reuses its generated identity; replay does not generate another instance.
Changing accepted inputs returns `ALREADY_EXISTS`.

Created means the Core database committed the instance and request binding
together. It does not mean a REST database row exists or that instance
provisioning has reached Ready. A successful bound `AllocateInstance` response
requires Created, including on replay; it never reports Creating as success.

While allocation is Creating, return `UNAVAILABLE` with ASCII
trailing metadata `external-validation-allocation-state: creating`. This exact
marker identifies accepted allocation in progress without parsing error text;
ordinary transient failures do not include it. An adapter must capture and
propagate the marker through its gRPC boundary. Its absence never proves that
allocation was not accepted. The caller
retries the same request and inputs or fetches its hold by ID for allocation state
and the saved instance ID. It must not generate a new attempt to recover an
ambiguous outcome.

Core owns autonomous reconciliation of every durable Creating record, including
after controller restart and when the caller has lost its original inputs.
It must persist the complete accepted inputs and chosen instance identity,
not only their fingerprint, before dispatch. Reconciliation uses those saved
inputs and the same request identity; caller retries are optional for progress.
With dependencies available and an authoritative outcome obtainable, Core
resolves the record to Created or confirmed NotStarted. NotStarted requires
proof that no allocation committed and fencing of the previous in-flight create.
An ambiguous absence, elapsed time, or lost caller cannot establish that proof.

There is no fixed convergence deadline and no unconditional guarantee that a
permanent fault resolves automatically to Created or NotStarted. Contradictory
ownership or an outcome that needs operator resolution moves the hold to
Recovery with a reason while preserving the unresolved allocation record.
An extended outage may leave Creating until authoritative checks resume. Neither
condition expires the hold, permits cancellation, or releases capacity.

After a rejection, read the saved attempt's allocation state. NotStarted permits
pre-allocation cancellation; inputs may be corrected only if Core never accepted
them. Accepted inputs remain immutable even after a confirmed no-allocation
outcome; cancel and open a new attempt to change them. Creating or Created
requires recovery using the saved identity and accepted inputs.

The request permits bypass of this workflow's hold only. NICo rechecks unrelated
blocking health alerts, repair admission, ownership, and provisionability at
allocation commit. Labels cannot establish a binding. A closed or superseded
request cannot allocate another instance, even after its original instance was
released.

## REST integration contract (implementation deferred)

External validation clients use the REST API. The companion
[PR #8147](https://github.com/dsx-ai-factory/infra-controller/pull/8147) adds hold
and attempt routes, OpenAPI/SDK types, and `externalValidationRequestId` on
single-instance create. Its handlers validate requests and access, then return
HTTP 501 until Core proxying and allocation binding are implemented. Publishing
either contract milestone does not make the workflow operational. The Site
workflow must still capture and propagate Core's allocation trailer.

The companion routes, relative to `/v2/org/{org}/nico`, map to Core as follows:

| REST operation | Route | Core operation |
| --- | --- | --- |
| Discover holds | `GET /machine/external-validation/hold?siteId=...` | `FindExternalValidationHoldIds`, then `FindExternalValidationHoldsByIds` |
| Start | `POST /machine/{machineId}/external-validation/attempt` | `StartExternalValidation` |
| Complete | `POST /machine/{machineId}/external-validation/attempt/{attemptId}/complete` | `CompleteExternalValidation` |
| Cancel | `POST /machine/{machineId}/external-validation/attempt/{attemptId}/cancel` | `CancelExternalValidation` |
| Remove | `DELETE /machine/{machineId}/external-validation/hold/{holdId}` | `RemoveExternalValidationHold` |

REST resolves the site from `siteId` or the route's machine and supplies that
machine as Core's expected-host check on Complete/Cancel. The route's
`attemptId` maps to Core `request_id`; `holdId` is the expected hold identity
for removal. Core `opened_at` maps to REST attempt `created`, and
`allocation_state` maps to the attempt's `allocationStatus` values NotStarted,
Creating, and Created. Core tenant organization IDs are resolved to REST Tenant
UUIDs; the companion permits a null `tenantId` if no REST Tenant matches the org.

REST's `pageNumber`, `pageSize`, and `orderBy` are adapter-level pagination over
the matching Core ID set, not Core cursor fields. Apply the documented global
ordering before slicing and preserve a deterministic hold-ID tie-breaker.
Fetch details in batches respecting Core's configured `max_find_by_ids`, even
when a REST page is larger than that limit. Neither layer promises a frozen
snapshot across discovery and retrieval.

Before operational integration, the companion REST schema still needs alignment
with the remaining Core guarantees:

- Core can discover/read Satisfied holds, while the companion hold enum and list
  describe active states only and offer no state filter or completed-hold read.
  Expose completed-state retrieval without dropping Core's retained outcomes.
- Core provides `state_changed_at` and `closed_at`; the companion response models
  expose creation time only. Define their REST representation for age monitoring.
- The structured create outcomes and HTTP 201 guarantee below need REST/workflow
  implementation and OpenAPI coverage; an attempt's `allocationStatus` alone is
  not a structured create-error outcome.

The companion does not add `externalValidationRequestId` to batch create and
ignores it there as an unknown JSON field. That is not request-bound allocation.
Callers must use single-instance create for this workflow; Core continues to
reject a present binding in its batch RPC. Do not describe the REST placeholder
as explicitly rejecting that unknown batch field.

The following is the proposed REST contract for the request-bound allocation
milestone; it is not implemented or added to OpenAPI by this contract-only PR:

- Add optional scalar UUID `externalValidationRequestId` to the single-instance
  create model and OpenAPI schema. Omission preserves ordinary allocation;
  an empty or malformed value is rejected. Translate it into Core's typed
  `external_validation_request_id` and preserve it through the Site workflow.
- Require an exact `machineId`, the configured validation tenant, and
  `allowUnhealthyMachine: true`; omit `instanceTypeId`. Do not add this binding to
  batch create. Existing tenant/site authorization and other blockers still apply.
- Persist the request identity, accepted create inputs, and REST-to-Core instance
  identity before dispatch. A replay of the same request and inputs must reuse
  that identity and return the original instance rather than generate a new one.
  Changed accepted inputs, including a different Core `instance_id`, conflict.
- Capture Core's allocation metadata at the gRPC boundary and carry the state
  through Temporal errors/results into a structured REST response. HTTP status,
  a generic retryable flag, or parsing an error message cannot substitute for it.

For a bound create response, propose structured fields
`externalValidationRequestId` (the submitted UUID) and
`externalValidationAllocationState` (`notStarted`, `creating`, `created`, or
`unknown`). Preserve these fields in the error representation as well as the
success response. The OpenAPI/REST implementation must establish the response
schema before enabling this path.

| Observed result | REST representation | Caller action |
| --- | --- | --- |
| Committed allocation or identical committed replay | HTTP 201 with the original instance and `created` | Continue using the saved instance identity and poll readiness. |
| Definite rejection, confirmed unaccepted | HTTP 4xx with `notStarted`; for an existing attempt this requires authoritative Core confirmation | Correct inputs or cancel only after confirming the attempt is NotStarted. |
| Accepted allocation still in progress | HTTP 503 with `creating`, preserving Core's `external-validation-allocation-state: creating` meaning | Retry the same request identity and inputs; never cancel. |
| Lost reply, timeout, or otherwise uncertain outcome | `unknown` if an HTTP response is possible; network failure has no response | Reconcile the saved request; never cancel or create a new attempt merely because the reply was lost. |

For a bound create, HTTP 201 must always carry
`externalValidationAllocationState: created`, never `creating`, `notStarted`,
or `unknown`. The REST handler must await successful Core allocation and the
REST transaction before returning it. The existing unbound create path awaits
the Site workflow's `AllocateInstance` call before returning 201; the bound
implementation must preserve that ordering and additionally verify the durable
request binding. A REST-row write alone is insufficient. This is a Core
allocation-commit guarantee, not an instance-readiness guarantee, so callers may
proceed directly from 201 to readiness polling without an extra hold lookup.

A 4xx from a changed-input conflict is not proof that the original create was
unaccepted. Missing structured state, an unrecognized state, and a bare 5xx are
unknown outcomes. `notStarted` permits cancellation only while the saved attempt
still satisfies Core's cancellation preconditions. The REST adapter must not
classify an accepted operation as a definite rejection after an ambiguous result.

REST model/handler/workflow changes, OpenAPI, SDK regeneration, and integration
tests are required together in the allocation milestone. These tests must cover
binding propagation, replay without a second instance, trailer propagation, and
unknown-outcome recovery. Until that work ships, REST callers cannot perform
request-bound external-validation allocation; this milestone publishes the
Core contract for the REST team's corresponding endpoints and OpenAPI work.

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

Both Cancel and Complete accept optional `machine_id` as an expected-host check.
REST must supply the route's machine ID when it uses that machine to select the
site. Core verifies it against the saved attempt before mutation or terminal
replay: a valid mismatch returns `NOT_FOUND`, while malformed or DPU IDs are
`INVALID_ARGUMENT`. Omission preserves lookup by request identity. Adding a
matching expected host on replay does not change the original result identity.

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

`RemoveExternalValidationHold` requires privileged method/site access, the exact
host machine ID, the expected hold UUID, and a reason of 1 through 4096 UTF-8
bytes. For REST calls, REST enforces Provider Admin authorization and audits
the end user before proxying as `SiteAgent`; Core authorizes and audits that
service caller, not an inferred end-user role. Validation-Tenant REST callers
cannot remove holds. Verify that outstanding allocation and cleanup are resolved; otherwise
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

This Core contract adds no access to local Machine Validation run/results or
unredacted stdout/stderr. The separate REST change in
[PR #8147](https://github.com/dsx-ai-factory/infra-controller/pull/8147) allows
privileged tenants with targeted-instance capability at the site to read
validation runs/results, returning empty `stdOut` and `stdErr` strings for
Tenant responses. That redaction and end-user authorization belong to REST;
this milestone does not change the existing Core result RPCs or their grants.

## Status codes

| gRPC code | Meaning and recovery |
| --- | --- |
| `INVALID_ARGUMENT` | Required input is missing, a supplied tenant filter is empty, a UUID is malformed, batch/details bounds are exceeded, or a state filter/outcome is unspecified or unknown. Correct the request. |
| `UNAUTHENTICATED` | Authentication failed. Use valid credentials. |
| `PERMISSION_DENIED` | Caller lacks method or tenant/site authorization. Request identity is not sufficient. |
| `NOT_FOUND` | Machine or request does not exist in the authorized scope, or Cancel/Complete's supplied machine does not match the attempt, including replay. Verify identity; absence of a hold instead returns `NoActiveHold`. |
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
