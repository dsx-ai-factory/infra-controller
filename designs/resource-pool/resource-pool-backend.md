# ResourcePoolBackend Execution and Recovery Contract

## Status and Scope

This design specifies the implementation contract for [P01: execution, ownership, and recovery](https://github.com/dsx-ai-factory/infra-controller/issues/7130), part of [ResourcePoolBackend foundations](https://github.com/dsx-ai-factory/infra-controller/issues/7129). It addresses the external VNI and route target requirements in [issue #6249](https://github.com/dsx-ai-factory/infra-controller/issues/6249).

The contracts below describe proposed behavior. This document does not enable a backend, add configuration, change an API, or migrate allocations. Source observations refer to revision `b4075867e725d272e41710adac394bce0e469e35`.

The release boundaries are:

| Milestone | Deliverable | Activation |
| --- | --- | --- |
| v2.4 | Integrated adapter, shared protocol, durable recovery, and a tested VPC path | Reject remote pool selection before provider calls or binding, seed, and growth writes |
| v2.5 | Complete gRPC lifecycle support for every existing pool, caller recovery, and external route targets | Explicit per-pool opt-in after qualification |
| v2.6 | Standalone PostgreSQL and REST backends | The same lifecycle and conformance contract |

Every release preserves Integrated behavior for unchanged deployments. No new configuration, credentials, required API field, or manual allocation conversion is needed. Merely defining a named backend does not move any pool.

Live allocation migration between authorities, expiring reservations, vendor-specific provisioning, and a public asynchronous creation API are separate work. Synchronous creation retains existing later resource and fabric readiness stages.

## Existing Boundaries

The following source paths establish the compatibility baseline:

| Boundary | Source | Behavior to Preserve |
| --- | --- | --- |
| Typed pool descriptor | [Model](../../crates/api-model/src/resource_pool/mod.rs) | `ResourcePool<T>` holds a name and runtime value type; it does not own a connection |
| Allocation and release | [Database allocator](../../crates/api-db/src/resource_pool.rs) | Writes borrow `PgConnection`; Integrated success does not need a separate connection or commit |
| Seed definitions | [Definition model](../../crates/api-model/src/resource_pool/define.rs) | Stored definitions record the seed or backfill declaration; they are not a complete history of growth |
| InfiniBand PKeys | [Startup](../../crates/api-core/src/setup.rs), [fabric configuration](../../crates/ib-fabric/src/config.rs) | Fabric `pkeys` ranges add missing values on every startup; they bypass ordinary pool snapshot reconciliation |
| VPC creation | [VPC handler](../../crates/api-core/src/handlers/vpc.rs) | VNI allocation and entity insertion share one transaction |
| Multiple allocations per owner | [VPC-DPU loopbacks](../../crates/api-db/src/vpc_dpu_loopback.rs) | One DPU owns distinct loopbacks for different VPCs in the same pool |
| Controller cleanup | [Network segment controller](../../crates/network-segment-controller/src/handler.rs) | Drain precedes local release and deletion |
| Cross-process work | [Work locks](../../crates/api-db/src/work_lock_manager.rs) | A renewable work lease can fence a short database transaction; it cannot fence an external effect |
| Conditional writes | [Database contracts](../../crates/api-db/src/lib.rs) | A failed ownership condition returns an explicit result |

A transport adapter cannot extend the entity transaction into another authority. Remote reservation can commit while the Core transaction fails or the reply disappears. Durable intent and conditional attachment therefore belong to the shared service.

## Crate and Execution Boundaries

Keep `ResourcePool<T>` in `api-model`. Add the service and backend selection in `crates/resource-pool`, below `api-core` and the controller crates. Keep SQL functions in `api-db`.

The dependency direction is:

```text
api-core and controller crates
    -> resource-pool
        -> api-db -> api-model
        -> api-model
        -> rpc (provider protocol and gRPC transport)
```

Neither `api-db` nor `api-model` depends on `resource-pool` or `api-core`. Backend-neutral values and persistent operation models belong in `api-model`; SQL persistence belongs in `api-db`. The service owns client reuse, dispatch, recovery, and semantic error translation. `api-core` maps those errors onto its public API.

Existing composite database helpers cannot call the new service without creating a dependency cycle. Move backend orchestration to their caller, or split the helper into its SQL mutations and the caller's lifecycle work. Preserve Integrated transaction ownership when making that split.

### ResourcePoolBackend

Use `ResourcePoolBackend` as an explicit execution choice with Integrated and Remote variants. Integrated contains the adapter for the existing SQL functions. Remote holds a transport-neutral provider interface implemented first by gRPC. Standalone PostgreSQL and REST use the Remote execution contract because they commit independently of the entity transaction.

The operations have different execution contexts:

| Execution Path | Inputs | Transaction Ownership |
| --- | --- | --- |
| Integrated allocate, exact allocate, and release | Typed handle, caller's mutable `PgConnection`, existing owner identity, selection | Caller begins and commits; adapter forwards to existing SQL |
| Remote reserve, recover, cancel, lookup, and release | Immutable durable operation or allocation identity, effective policy, provider client | No entity transaction or borrowed database connection crosses the provider call |
| Remote attachment | Validated assignment and durable operation identity | Short local transaction changes entity and operation atomically |
| Pool inspection | Resolved binding and capability | Local read or bounded provider call; unavailable is distinct from empty |

Do not use one method with an optional transaction argument. Choose the pool and resolve its binding under the caller's required locks. Integrated work can continue in that transaction. For Remote work, persist the selected binding and intent, then end the transaction before dispatch. Attachment revalidates the selection against current tenant and routing state; a changed selection requires cancellation and a new attempt. A Remote method must not accept a database executor as an unused parameter.

Keep generic conversion at the typed pool boundary. The provider interface handles a closed value enum, so one client supports every pool without a VNI-specific transport. Validate the response against the authoritative configured or persisted pool value kind and the Rust consumer type. The legacy `IpAddr` handles for `lo-ip` and `vpc-dpu-lo` have an `Ipv4` descriptor tag but accept either address family; that tag must not reject a valid IPv6 pool. An `Ipv6Addr` consumer remains IPv6-only. Apply narrower domain checks, such as VNI width or ASN validity, at the consumer boundary.

The v2.4 gate runs during configuration resolution, before the first ordinary seed reconciliation in `setup.rs`, registry construction, or the separate PKey seeding path. It rejects remote selection before provider calls or binding, seed, and growth writes. Administrative growth validates the complete request against the same activation boundary before mutation.

Startup constructs one registry and the clients it owns, then injects shared handles into API services and controllers. Shutdown joins background recovery and polling tasks. A caller does not construct a connection or credential loader for each allocation.

### Integrated Preservation

The Integrated adapter must retain the existing SQL success path, owner strings, partition selection, errors, rollback, and conditional-release results. Adding the service must not create a second transaction or make successful allocation depend on remote operation records.

Old Integrated allocations remain valid without acquiring remote handles. Integrated cleanup continues using its existing ownership checks. New remote generations do not retroactively change the same-owner semantics of Integrated exact allocation.

## Binding and Authority

Configuration names select a binding; they do not identify the authority that issued an allocation.

The binding contract distinguishes these identities:

| Identity | Meaning |
| --- | --- |
| Site ID | Persisted identity shared by replicas of one deployment; not a pod name or process UUID |
| Backend name | Operator-facing configuration reference |
| Authority ID | Stable provider identity verified during connection establishment; independent of URL and credentials |
| Provider pool ID | Stable identity of the provider's pool within that authority |
| Logical pool name | Existing NICo pool identity used by typed handles |
| Binding generation | Immutable association of site, logical pool, authority, and provider pool |
| Policy revision | Immutable effective eligibility used by one operation |

An omitted pool `backend` selects Integrated. An unknown reference is an error, never a fallback. A named Integrated backend still uses the caller's database transaction. An omitted `remote_pool` resolves to the logical pool name; an explicit alias only changes provider lookup. Empty names are invalid.

Resolve an alias to a stable provider pool ID and persist that identity before allocation. Reconnects must verify the same authority and pool identity. An endpoint or credential update can retain a binding only if this check succeeds. Provider identity checks supplement authenticated TLS; a self-reported ID alone does not authenticate a service.

Retain issuing authority, provider pool, and binding generation on every remote operation and allocation. Lookup and release use those records, even if configuration changes. Removal or redirection of a binding with unresolved operations, attached reservations, retained allocations, or unfinished cleanup is rejected.

[P03: named backends and bindings](https://github.com/dsx-ai-factory/infra-controller/issues/7132) owns exact TOML keys beyond the agreed selectors, precedence, credential delivery, and startup validation. It also owns the explicit first-selection procedure for an upgraded deployment and whether a previously seeded, drained binding can be reassigned. Until that procedure is implemented and tested, changing an existing binding is rejected. An empty allocation list alone is insufficient because a request can still be in flight.

Parse selectors separately from the legacy `ResourcePoolDef` seed payload and compare only that payload for definition drift. Keep selectors out of its stored JSON so old binaries continue decoding the snapshot and a selected backend does not cause a false drift warning. Persist binding metadata separately and reconstruct it on every replica, including replicas that do not perform seeding.

Ordinary pool declarations gain optional `backend` and `remote_pool` selectors beside their existing definition fields. PKey selectors belong beside the existing required `pkeys` array under `[ib_fabrics.<fabric>]`; omission selects Integrated and the logical name `ib_fabrics.<fabric>.pkey`. Preserve the fabric configuration and its separate startup path; do not require a duplicate `pools` declaration.

## Effective Eligibility

TOML defines NICo's initial eligibility. The backend determines which eligible values are available and reserves them. This does not make edited TOML a live allocation filter.

For ordinary `pools` declarations, the existing lifecycle remains:

| Operation | Result |
| --- | --- |
| Seed a new pool | Expand its declared values and assignment modes; store the seed definition |
| Restart with the same declaration | Preserve inventory and allocations |
| Edit ranges and restart | Warn about drift; preserve effective inventory and the stored seed snapshot |
| Remove a declaration | Warn; do not implicitly release or delete its values |
| Explicitly grow | Add missing eligible values; preserve ownership and assignment mode of existing entries |

PKey pools are an existing exception: each startup additively applies the fabric's `pkeys` ranges without ordinary snapshot drift handling. Removing values does not shrink the pool. Integrated keeps this behavior. Remote PKey startup adds missing eligibility through the durable successor-policy procedure below before serving allocations; replay reuses the prepared revision. P03 propagates the selectors through this path, P08 implements its policy growth, and the v2.5 PKey task preserves the consumer lifecycle.

Growth does not rewrite the original seed snapshot. Therefore, effective eligibility cannot be reconstructed from that snapshot alone. Integrated persisted value rows are authoritative for upgrading an existing pool, including values added by growth and their assignment modes.

### Values and Selection

The shared contract preserves these value domains and expansion rules:

| Value Kind | Required Representation and Semantics |
| --- | --- |
| Integer | Signed `i64`; explicit range start included and end excluded |
| IPv4 address | Typed IPv4 address; explicit address ranges exclude the end; prefix expansion includes the network address and excludes broadcast |
| IPv6 address | Typed IPv6 address; explicit address ranges exclude the end; prefix expansion includes all addresses |
| IPv6 delegated prefix | Typed network and prefix length; distinct from a single IPv6 address; preserve existing prefix enumeration |

Wire values use a discriminated value kind, not an unsigned VNI field or a string that every consumer reparses differently. Prefix validation rejects malformed or noncanonical provider values before attachment. Transport encoding must round-trip every supported value without narrowing it.

Selection is explicit:

| Selection | Eligible Partition | Conflict Semantics |
| --- | --- | --- |
| Automatic | `auto_assign = true` | Select a free eligible value |
| Requested create | `auto_assign = false` | Reserve the requested free value in the manual partition |
| Exact transition | Either partition | Reserve precisely the requested value; no implicit ownership transfer |

Integrated `allocate_exact` still conflicts when the value is already allocated, even to the same owner. Remote idempotency replays one operation; it does not make a different operation by the same owner succeed on an occupied value.

For a new pool, require exactly one of a prefix or nonempty ranges, preserving existing definition validation. Omitted ranges default to empty; explicitly empty ranges have the same meaning. Neither permits unrestricted allocation or import of the provider's whole inventory. Empty ranges without a prefix are invalid. Eligibility follows the accepted declaration and retains assignment mode per value; an existing overlap or growth preserves the mode already established for that value.

### Policy Revisions and Growth

Persist an immutable effective policy revision before remote dispatch. It contains value kind, eligible intervals or prefixes, and assignment modes. Every reservation binds to the exact revision and content digest. Provider policy registration is idempotent; reusing a revision ID with different content is an error.

The provider acknowledges that it can enforce the policy, without claiming that it owns or has provisioned every value in it. Required reservation capability includes all value kinds and selection modes. Provider inventory creation is a separate optional capability.

Explicit growth prepares a durable successor policy, obtains provider acknowledgement outside a database transaction, then conditionally activates that revision locally. Calls already dispatched keep their previous revision. A reply lost during registration is recovered or replayed with the same revision identity. No local success is reported before acknowledgement and activation.

Growth adds eligibility; it does not move existing values between assignment partitions or promise new provider capacity. Reject unsupported operations and invalid mixed-pool batches before mutations. Multi-authority growth cannot promise a distributed atomic commit; its administrative contract must report recoverable progress or reject that batch before dispatch.

Validate every returned assignment against the operation's policy and the consumer's domain. On a violation, retain its operation and handle, block attachment, and request cancellation. Do not discard the identity or repeatedly allocate unconstrained values until one fits.

### Presence, Capacity, and Membership

Remote bindings do not seed synthetic rows into Integrated inventory. Their binding and policy records establish configuration presence; provider inspection establishes availability. Seed reconciliation must branch on the binding before applying the local-row New, Backfill, or Anomaly classification.

The existing gates need explicit backend-aware equivalents:

| Existing Gate | Remote Contract | Owner |
| --- | --- | --- |
| Mandatory pool startup checks | Validate the binding and query usable capacity; known zero remains a failed check. Unavailable or unsupported capacity is an explicit failed check, not zero or success | P03, P10 |
| Optional IPv6 allocation and backfill | An absent binding means unconfigured; a configured exhausted pool remains an allocation error | v2.5 loopback enablement |
| ASN backfill | Preserve absent/full skipping using backend status; unknown capacity requires a visible retry or unsupported result | v2.5 ASN enablement |
| VPC routing-profile transition | Check destination presence and complete effective internal/external VNI membership for overlap, including manual and allocated values | P08, P12 |
| Metrics, list, and IP finder | Inspect the selected backend or durable assignment records appropriate to the query; preserve unknown and unavailable states | P09, P10 |

The [common-pool constructor](../../crates/api-db/src/resource_pool.rs), [machine allocation and backfill helpers](../../crates/api-db/src/machine.rs), and [VPC transition handler](../../crates/api-core/src/handlers/vpc.rs) contain these gates. Remote overlap checks use full effective eligibility, including any retained assignments, rather than only allocations observed locally. A provider's inventory outside NICo's eligibility does not make those values NICo pool members.

## Durable Remote Records

Reservations persist until explicit release. There are no expiry or renewal semantics in the first provider contract.

The durable records must satisfy these requirements; [P06: operation and allocation persistence](https://github.com/dsx-ai-factory/infra-controller/issues/7135) owns the concrete schema:

| Record | Required Contents |
| --- | --- |
| Binding | Stable site, logical pool, authority, provider pool, binding generation, and effective policy revision |
| Operation | Unique operation ID, immutable request body or canonical digest plus recoverable input, binding, policy, owner, slot, generation, completion authority, and lifecycle state |
| Assignment | Issuing identity, allocation handle, typed value, associated immutable policy payload, and provider result identity |
| Attachment | Entity and slot reference to the assignment; retained until the entity's lifecycle permits release |
| Terminal record | Operation identity and final state sufficient to reject replay and diagnose cleanup |

An owner includes its resource type and stable ID. The slot identifies the allocation's purpose within that owner. For a VPC-DPU loopback, the slot includes the VPC identity; `(pool, machine_id)` alone is not unique. A new allocation attempt after terminal cleanup receives a new generation and operation ID.

Persist the operation before any provider call. Its identity and request never change on retries or worker takeover. Uniqueness and conditional transitions permit at most one operation for an owner, slot, and generation, and at most one unresolved unattached attempt for that owner and slot across generations. Retained Attached assignments from earlier generations remain independent. A fresh generation cannot bypass Pending or CancelRequested work. A request digest includes scope, selection, exact value if any, policy revision, and requested assignment options.

Do not use a transaction rollback, an elapsed work lease, or absence of an entity as proof that the provider did nothing.

### Local Lifecycle

Use these semantic states, with transition guards implemented in short database transactions:

| State | Meaning | Permitted Successor |
| --- | --- | --- |
| Pending | Intent committed; dispatch can be absent, running, or have an unknown result | Attached or CancelRequested |
| Attached | Entity and validated assignment committed together | ReleaseRequested after consumer-specific drain or retention checks |
| CancelRequested | Attachment forbidden; provider reservation must be cancelled, including a request not yet received | Cancelled after terminal provider acknowledgement |
| ReleaseRequested | Entity no longer uses the assignment; cleanup retains original handle and issuing authority | Released after provider acknowledgement |
| Cancelled | Provider guarantees this operation cannot reserve later | Terminal |
| Released | Original allocation is gone; replay cannot create another | Terminal |

A provider result can be persisted while the operation remains Pending. Result availability does not grant attachment permission. The transaction attaching the entity also records the assignment and moves the operation to Attached; it must verify the current completion authority, slot generation, policy, and Pending state.

Cancellation first conditionally moves Pending to CancelRequested. Attachment and cancellation contend on the same durable state, so exactly one can win. If attachment already committed, cancellation must follow the entity's normal deletion or retention lifecycle rather than release its live assignment.

A routing change that retains an inactive VNI must retain that assignment's handle and policy independently of the new active generation. Explicit inactive-VNI cleanup releases only the retained assignment after its existing version and expected-value checks.

The operation record survives a failed entity transaction. Cleanup records survive entity deletion. If a workflow allocates several resources, persist each reservation and an aggregate completion intent. Failure of the second allocation cancels the first unless the entity transaction attached the complete set.

### Work Ownership

Use `WorkLockManager` to coordinate long-running work across replicas. Call `fence_transaction` before protected writes, keep those writes in that short transaction, and retain the work guard until protected work stops.

A lease can expire while its former worker still runs. The provider therefore must make overlapping requests safe independently: same-operation reserve is idempotent, cancel is terminal, and release targets one handle. A stale worker cannot attach after losing its database fence or after CancelRequested wins.

Reacquiring a work lease does not generate a new reservation identity. Reconcile the original operation first. Every writer of the operation lifecycle must participate in the same conditional state contract, including handlers, recovery workers, deletion, and administrative repair.

## Provider Protocol

[P04: provider protocol and conformance](https://github.com/dsx-ai-factory/infra-controller/issues/7133) translates these semantics into the versioned `resource_pool.proto`. The following names identify operations, not shipped RPC declarations:

| Operation | Contract |
| --- | --- |
| Describe | Authenticate authority and pool identity; report protocol version, required capabilities, and optional inventory capabilities |
| RegisterPolicy | Idempotently acknowledge one immutable eligibility revision |
| Reserve | Reserve for one immutable operation; return the original assignment on replay; success includes required provider provisioning |
| Recover | Return Unknown, InProgress, Reserved, Cancelled, or Released for that operation |
| Cancel | Durably prohibit this operation from creating or retaining a reservation, even when Reserve has not arrived |
| Lookup | Inspect one issued handle and its ownership; never infer identity from a value alone |
| Release | Conditionally release the exact issuing identity, handle, owner, slot, and generation; replay cannot free a later allocation |

Provider operations must be linearizable per operation identity. Cancel and Reserve share the same durable serialization point. When Cancel arrives first, a later Reserve observes its terminal tombstone. When Reserve wins first, Cancel removes that reservation and its exclusively owned provisioning before acknowledging terminal cancellation.

Cancel can report InProgress while cleanup runs. Core keeps CancelRequested and retries recovery. A lost Cancel or Release reply is ambiguous; Core keeps the durable cleanup intent and repeats the same conditional operation.

A successful Release also leaves the original reserve operation terminal. Otherwise, a delayed duplicate Reserve could recreate the released allocation.

### Retention and Reuse

Version 1 requires durable terminal tombstones for the lifetime of the site and authority namespace. There is no time-based tombstone eviction, Forget operation, or automatic namespace reuse. This deliberately avoids relying on an unproved maximum network delay or retry horizon.

The provider can compact terminal records to identity, immutable request digest, and terminal state, provided it still rejects conflicting requests and cannot resurrect work. Core retains the corresponding terminal identity and audit linkage. Bound active work and page terminal history; do not load all historical operations into memory.

A future retirement protocol can permit garbage collection only after fencing the namespace against all old requests, credentials, and restored clients. Provider disaster recovery must preserve reservation and tombstone durability or stop allocation until reconciliation establishes it.

Ordinary process restart uses the current durable database. Restoring an older database is a separate offline recovery procedure owned by P06 and P07 and required before remote activation. Stop and fence every original writer, invalidate its provider access, and establish that accepted in-flight requests can no longer change the reconciled state. A provider that cannot establish that boundary keeps the site offline. Starting a live clone with the original site identity and credentials is unsupported; a database-local ID cannot distinguish that clone from a legitimate replica.

Before resuming provider dispatch, entity use, or DPU configuration publication, reconcile all Pending, Attached, retained, and cleanup records with their original provider identities. Matching handles can resume; missing, released, reissued, or conflicting handles quarantine their consumers until repaired and drained as needed. Never render a stale Attached VNI merely because the backup contains it. Provider reservations absent from the backup require an authoritative ownership audit before adoption or cleanup; absence from restored Core state is not cancellation authority. P06/P07 must document and prove this fence and reconciliation procedure. A clone used as a new site needs a new identity and may not replay or claim the original site's operations or handles.

Handles identify allocation instances, not values. If value 101 is released and reissued under a new handle, releasing the old handle is an idempotent terminal result or an explicit ownership conflict. It must never release the new reservation, even if the owner string is identical.

### Outcomes and Retry Policy

Keep exhaustion, exact conflict, invalid policy or value, request-identity conflict, authentication failure, unsupported capability, unavailable service, and unknown outcome distinct. Preserve diagnostic sources internally and map stable semantic errors at public boundaries.

A timeout is an unknown outcome, not exhaustion. Recover returning Unknown is only an observation: an earlier Reserve can still arrive. Only terminal Cancel acknowledgement authorizes declaring an unattached reservation cleaned up.

Do not fall back to Integrated when a remote authority is unavailable. Retry only the same immutable operation after an ambiguous outcome. Invalid requests, authentication failures, and capability mismatches require correction rather than blind allocation retries.

[P05: bounded gRPC calls](https://github.com/dsx-ai-factory/infra-controller/issues/7134) owns verified TLS, credential refresh, connection reuse, whole-attempt deadlines including capacity waits, bounded foreground retries, and cancellation. [P07: recovery](https://github.com/dsx-ai-factory/infra-controller/issues/7136) owns bounded durable reconciliation and observable cleanup backlog. These deadlines bound individual work; they do not expire allocations or tombstones.

## Foreground Completion and Caller Recovery

A successful create response requires a validated, usable provider assignment and a committed entity attachment. Provider-owned provisioning must be ready before Reserve succeeds. This does not require every DPU to acknowledge its later network configuration.

For an API request creating a new Core entity, only the active foreground create attempt can attach it. Persist its completion authority and an attachment deadline before dispatch. The attachment transaction verifies that authority and deadline using database time, then repeats the entity's tenant, authorization, security-group, routing, and overlap checks against current state. Persisted intent does not bypass admission. Foreground failure or cancellation requests CancelRequested; process death eventually expires the attachment authority. A recovery worker can recover and cancel the reservation, but cannot create a previously absent entity from an allocator operation alone.

For a controller adding a slot to an existing entity, a persisted desired generation can authorize recovery to complete that slot. Revalidate the entity, desired generation, and deletion state under the attachment transaction. A removed or replaced desire requires cancellation. This authority must be explicit; it cannot be inferred from the existence of a Pending operation.

Apply those authority rules to every allocation caller:

| Caller | Completion Authority |
| --- | --- |
| API create or VPC routing-profile mutation | Active foreground attempt with deadline; mutation also fences the existing entity version and selected pool |
| Site-explorer, new-machine discovery, or configured initial VPC creation | Persisted creation desire from the discovery workflow or configuration, with explicit ownership and current admission checks; preserve atomic attachment of the allocation set and cancel abandoned or removed desire |
| ASN or IPv6 backfill, or discovery filling an existing machine slot | Persisted desire for the existing machine slot; revalidate machine existence, generation, and deletion state |
| VPC-DPU loopback | Persisted VPC/DPU slot desire; [P20](https://github.com/dsx-ai-factory/infra-controller/issues/7141) and v2.5 P21 prepare the assignment before network-config rendering |

A durable discovery or configured-entity creation desire can authorize its workflow to finish after a worker restart. This is distinct from an abandoned API create: neither an allocator Pending record nor an absent entity supplies that authority.

A polled DPU configuration read must not dispatch a remote reservation. It reads a prepared assignment or reports that the dependent configuration is not ready. Retries continue the same durable desired slot; provider unavailability remains visible and cannot justify a new reservation or Integrated fallback.

A handler can commit just before its connection disappears. Cancellation that loses to Attached must preserve the entity and assignment. Return ambiguity to the caller where possible, and reconcile by stable entity identity. A lost reply cannot undo a committed transaction.

A public caller-supplied retry key remains optional. Server-generated operation IDs protect internal retries and recovery without changing existing request fields. Two unkeyed POST requests are distinct requests; matching payloads do not prove they represent one intent. A new request for an entity ID and slot with an unresolved unattached attempt returns a retryable conflict without dispatch. It does not take over the earlier foreground authority. Internal recovery uses the original operation; the caller can start a new attempt only after terminal cancellation, or reconcile the already committed entity using existing create-conflict semantics.

### REST Ownership

The planned v2.5 caller-recovery task, P13, must persist caller intent and the Core entity ID before dispatch. It must retain tenant, metadata, request attribution, and the intended completion state through a lost reply or cloud transaction rollback. If a request omits an ID, generate and durably record one before making the Core call.

REST success requires both Core attachment and its local completion contract. Recovery can reconstruct the local record for a Core entity already committed by the authorized foreground attempt. If Core has no attached entity, recovery cancels the remote operation; a fresh create is a separate attempt. Do not replay an ambiguous unkeyed create with a new identity.

The [REST VPC handler](../../rest-api/api/pkg/api/handler/vpc.go) holds a cloud transaction across its Core workflow, and the [site create workflow](../../rest-api/site-workflow/pkg/workflow/vpc.go) disables automatic create retries. The [inventory activity](../../rest-api/workflow/pkg/activity/vpc/vpc.go) can reconstruct committed Core VPCs. P13 must end the cloud transaction after persisting caller intent and before dispatching remote-enabled Core work, then complete locally in a new transaction. No outer caller may retain a database transaction across provider I/O.

VPC and subnet inventory reconstruction is not sufficient request recovery. Other inventory paths can skip unknown entities, and request metadata is not necessarily recoverable from inventory. An internal lookup by durable caller identity must distinguish a still-running attempt from a terminal absent entity before abandoning its dependencies.

REST subnet creation also reserves an IPAM prefix before Core creation. The v2.5 P26 task must preserve that hold across an uncertain Core result. Release it only after Core terminally confirms no attached segment and cancellation, or after the attached segment completes normal deletion. Cloud rollback alone does not make the prefix available.

The [REST workflow bounds](../../rest-api/common/pkg/util/workflow.go) at the source revision are 40 seconds for the activity, 45 seconds for workflow execution, and 50 seconds for the handler wait. Fit provider attempts, capacity waits, backoff, local attachment, and response handling inside the remaining activity budget. P05 and P13 must choose and test shorter per-attempt limits together; a provider timeout cannot consume the entire outer budget. Recovery does not keep a timed-out HTTP request alive or expose a new public pending allocation state. Existing REST Pending and Provisioning states retain their meanings.

## VNI and Route Target Assignment

A VPC reservation returns one immutable assignment containing its VNI and optional route target (RT) policy. Recover returns the same pair. Persist and attach them together; a later lookup must not silently replace policy for an existing reservation.

The generic value remains an integer. RT policy is associated assignment metadata, not another integer pool or a globally unique allocation. Shared RTs are valid. Non-VPC consumers reject unexpected VPC policy metadata.

### Presence and Generated Policy

Preserve existing per-VPC override precedence: an omitted direction inherits its named-profile list, while a present empty override suppresses that inherited list.

Use an optional policy message with independently present import and export collections. A present collection can be empty; a repeated field alone cannot preserve that distinction. Keep presence through protobuf, Rust, persistence, and DPU configuration.

The composition rules are:

| Provider Policy | Generated RTs | Configured Profile and Site RTs | Provider Lists |
| --- | --- | --- | --- |
| Absent | Preserve existing behavior | Preserve existing resolution | No addition |
| Present, preserve mode | Preserve existing behavior | Preserve existing resolution | Add each present list; an empty list adds nothing |
| Present, replace mode with explicit operator opt-in | Suppress generated own import and export RTs, including automatic template generation | Preserve explicitly configured profile and site additions | Both lists required; an empty list deliberately contributes none |

Omitted mode means preserve. Replace without the operator's explicit opt-in is an error. An absent direction in preserve mode leaves that direction unchanged; an absent direction in replace mode is invalid. Explicit empty replacement does not erase separately configured site or profile policy. The effective own import and export sets must share at least one RT so DPUs in the same VPC can exchange routes. Empty provider lists are valid when configured policy supplies that common RT; otherwise reject replacement during admission.

Do not interpret the mere presence of provider RTs as replacement. A separate future policy update cannot mutate an active reservation implicitly; it needs an explicit versioned entity operation and the same admission checks.

### Peering and Admission

Resolve effective own and peer routing policy once in Core and use that result for tenant prefix-overlap admission and DPU configuration. Do not independently reconstruct a peer's import identity from its VNI when the peer replaces generated RTs.

The provider must identify which export RTs safely identify this VPC for explicit peering. Require those peer identity RTs to be a subset of the VPC's effective exports, reserved for that VPC within the routing domain. Importing every shared export tag could accidentally connect unrelated VPCs. If a replaced VPC lacks a safe peer identity, reject a peering operation that needs it. Ordinary shared policy RTs remain valid for intentional shared routing.

The [FNN template](../../crates/agent/templates/nvue_startup_fnn.conf), [peer VNI construction](../../crates/api-core/src/ethernet_virtualization.rs), and [prefix-overlap admission](../../crates/api-core/src/handlers/tenant_prefix_overlap.rs) are the existing consumers to align.

Replacement suppresses both the explicit `<ASN>:<VNI>` emission and any `auto` entry that would recreate the removed routing membership. Preserve the existing renderer output when policy is absent. P14 and P15 own central resolution, wire presence, own/peer rendering, and device qualification.

An older DPU can ignore newly added protobuf fields. Require an explicit capability and placement check before accepting replacement or placing that VPC on a DPU. Unknown capability is unsupported. This admission requirement does not add an all-DPU acknowledgement barrier to create success.

## Failure and Recovery Table

These cases are required conformance and production-path proofs. Local transaction fences and provider terminal states have separate responsibilities:

| Failure or Interleaving | Required Recovery | Forbidden Outcome |
| --- | --- | --- |
| Crash after intent commit, before dispatch | Recover same ID; cancel if foreground authority expired | New reservation ID solely because of restart |
| Provider commits, reply is lost | Recover original assignment or cancel same operation | Duplicate allocation or provisioning |
| Recover says Unknown, delayed Reserve arrives | Cancel establishes a tombstone before local cleanup is terminal | Treat Unknown as proof of no future reservation |
| Cancel arrives before Reserve | Reserve observes terminal cancellation | Late reservation after cleanup |
| Reserve arrives before Cancel | Cancel cleans the original reservation before terminal acknowledgement | Acknowledged cancellation with live provisioning |
| Provider returns an invalid value or policy | Retain identity, prevent attachment, cancel | Forgetting an allocated resource because validation failed |
| Local attachment fails or rolls back | Operation remains recoverable; active foreground can retry, otherwise cancel | Lost remote allocation identity |
| Attach races with cancellation | One local Pending transition wins; Attached follows entity lifecycle | Cleanup releasing an attached allocation |
| Work lease expires and another worker takes over | Same operation ID; stale database fence fails | Stale worker attaching or generating another reservation |
| Entity deletion commits, release reply is lost | Retain ReleaseRequested and original handle; retry | Lost cleanup after row deletion |
| Old release follows value reuse | Release only original handle and generation | Releasing the new holder of that value |
| Released Reserve is replayed | Return terminal Released | Recreating a reservation after deletion |
| Core commits, client loses reply | Preserve attachment; reconcile durable caller identity | Releasing a live reservation based on client timeout |
| Foreground disappears before attachment | Expire completion authority and cancel | Background creation of an unrequested new entity |
| One of several required allocations fails | Cancel unattached reservations by original identities | Partial entity success or leaked first reservation |
| REST rolls back after Core commits | Reconcile local entity and retain dependent IPAM hold | Reuse of a prefix still attached in Core |

## Compatibility and Activation

Integrated-only mixed-version upgrades retain the old snapshot format, existing rows, transaction behavior, generated RTs, API fields, and metric contracts. New metadata must be additive and readable without making old binaries allocate through another authority.

Before remote activation, every participating API, controller, and startup writer must understand binding and operation records. An old writer that ignores a remote binding can allocate Integrated values incorrectly. Therefore remote activation requires excluding incompatible writers, not merely deploying one new replica. P03 must implement and test the deployment admission boundary; a failed check leaves selection unavailable.

After remote activation, rollback is supported only to a binary that understands the active binding, recovery, and policy schema. Returning to an older Integrated-only binary requires a separately verified drain and configuration rollback procedure. Do not claim that additive protobuf or schema changes alone make remote downgrade safe.

[P11: gated VPC integration](https://github.com/dsx-ai-factory/infra-controller/issues/7140) exercises the real remote service below the v2.4 startup gate. There is no operator-facing bypass. The v2.5 activation task remains blocked until allocation, lookup, recovery, retained ownership, and cleanup work for every pool and caller, plus RT and deployment qualification.

Pool statistics distinguish known capacity, unsupported capacity, unavailable provider, and stale observation. Do not encode unknown as zero. Integrated metric names and meaning remain unchanged. Legacy numeric-only inspection must return an explicit unsupported or unavailable result when it cannot represent a remote answer; new status-aware fields can report partial information deliberately.

## Verification and Implementation Handoff

P01 uses disposable probes to check the Rust dependency/borrowing boundary and the protocol's race assumptions. They are design evidence, not a production recovery implementation or proof of real provider durability.

The minimum evidence and later implementation owners are:

| Proof | P01 Boundary | Implementation Follow-Up |
| --- | --- | --- |
| Integrated call through shared service | Compile actual model, allocator, and transaction types without `api-core`; caller retains commit | [P02](https://github.com/dsx-ai-factory/infra-controller/issues/7131) adds production VPC wiring and rollback tests |
| Delayed Reserve after Unknown and Cancel | Exercise both provider orderings and retained tombstone | [P04](https://github.com/dsx-ai-factory/infra-controller/issues/7133) tests the real protocol provider |
| Attachment versus cancellation | Assert Attached with an entity when attachment wins, and CancelRequested without an entity when cancellation wins | [P06](https://github.com/dsx-ai-factory/infra-controller/issues/7135) proves SQL races and persistence |
| Stale release after reuse | Preserve the new allocation under an old handle | [P07](https://github.com/dsx-ai-factory/infra-controller/issues/7136) proves restart and lost replies |
| All-pool policy and compatibility | Trace existing seed, growth, typed values, and consumers | [P08](https://github.com/dsx-ai-factory/infra-controller/issues/7137) proves predecessor-data reconstruction |
| Synchronous caller completion | Trace VPC and REST ownership boundaries | P11 and v2.5 P12, P13, and P26 prove real cross-service failures |
| Effective RT policy | Identify default emission, peer imports, and admission | v2.5 P14 and P15 prove rendered output, old-DPU rejection, and device behavior |

Later PRs must inventory retained tests before adding cases. Put each proof at the narrowest layer that exercises its distinct failure boundary. An in-memory protocol probe does not substitute for SQL fencing, restart persistence, caller rollback, or a real gRPC provider.

The next implementation is [P02](https://github.com/dsx-ai-factory/infra-controller/issues/7131): introduce the shared boundary and preserve the first Integrated VPC allocation/release path. P03, P04, and P06 then implement configuration, protocol, and persistence against this contract. Revisit estimates after the P07 and P11 production-path proofs, before parallel consumer work depends on untested assumptions.

The v2.5 task identifiers used here are planning identifiers, not GitHub issue numbers:

| Plan ID | Required Consumer Work |
| --- | --- |
| P12 | Complete VPC VNI lifecycle, retained assignments, and admission |
| P13 | Core and REST create identity, lost-reply recovery, and synchronous completion |
| P14 | Central effective RT resolution with unchanged Integrated behavior |
| P15 | Provider RT replacement, peer imports, DPU compatibility, and device qualification |
| P21 | Enable VPC-DPU loopback assignments before configuration rendering |
| P26 | REST subnet IPAM retention across uncertain Core creation |

These tasks belong to the later enablement epic; the published v2.4 epic does not claim customer-ready remote allocation.
