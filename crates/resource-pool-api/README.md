# Resource pool provider API

This crate defines how an external allocator serves NICo resource pools. The
operations follow the existing pool API: `Allocate`, `AllocateExact`, and
`Release`. The extra request and allocation IDs let NICo recover a lost reply
and release the original allocation safely, even if its value has since been
assigned to someone else.

The contract lives in
[`resource_pool.proto`](../rpc/proto/resource_pool/v1/resource_pool.proto).
Cargo generates the Rust messages, client, and server into `OUT_DIR` and exposes
them under `carbide_resource_pool_api::v1`. These bindings are separate from
Core's public Forge service and reflection descriptor.

Keeping this crate separate from `resource-pool` lets provider implementations
use the API without pulling in NICo's database and model crates.

## Familiar operations

| Operation | What it does |
| --- | --- |
| `Describe` | Resolves a configured pool alias to a stable allocator and pool identity. |
| `Define` | Acknowledges the values NICo may allocate, independently of what the provider has available. |
| `Allocate` | Selects a free automatic value, or the requested free manual value. |
| `AllocateExact` | Allocates the requested value from either partition. |
| `Release` | Releases the original allocation only when its issuing identity and owner match. |
| `Lookup` | Reads an active assignment using its allocation ID. |
| `Stats` | Reports the existing six used/free counts, when the provider supports them. |
| `Recover` | Reads the result of an earlier allocation attempt. |
| `Cancel` | Stops an attempt from allocating, including one that has not arrived yet. |

Each RPC has its own request and response messages. For example, `Allocate`
accepts `AllocateRequest` and returns `AllocateResponse`, whose `allocation`
field contains the assignment. Even `DefineResponse` has its own message, so
we can add fields later without changing the RPC's response type.

`PoolBinding` identifies the NICo pool and the provider pool backing it. NICo assigns a random binding UUID. Its allocation callers generate a fresh random UUID for each new attempt, then save and reuse it on retries. Request IDs must not be derived from owners or slots, since those values can repeat across deployments sharing a provider. `OperationIdentity` pairs the binding with the request ID. `Recover` and `Cancel` accept it in their request's `operation_identity` field and return it alongside `operation_status`. The returned identity matches the request, including when `Recover` reports `unknown`.

Identifiers, aliases, owner fields, revision IDs, and failure messages must be nonempty UTF-8 without NUL (`U+0000`). NICo preserves their case, whitespace, and other Unicode characters exactly. There is no per-field length limit; transport message limits still apply.

`Allocate` preserves manual-partition exhaustion precedence: when no manual
values are free, a requested allocation returns `ResourceExhausted`. Otherwise,
an unavailable requested value returns `FailedPrecondition`. `AllocateExact`
always returns `FailedPrecondition` for an unavailable value, including one
already held by the same owner under a different request.

An omitted `requested_value` selects the automatic partition. An unset value
inside a present message is malformed. Values distinguish signed integers,
IPv4 addresses, IPv6 addresses, and IPv6 prefixes. Address pools can contain
both address families. `Value::validate` and `Eligibility::validate` check these
wire representations. `Eligibility::contains` checks whether a value belongs to
one declaration without expanding its range. The consuming pool still applies
limits such as VNI width.

## Definitions and growth

`Define` describes the values NICo may allocate, including values added by explicit growth. The original TOML seed snapshot does not record that later growth. Each definition revision is immutable and must be acknowledged before an allocation can use it. Defining a pool does not allocate a value.

Core's [`registration` module](../resource-pool/src/registration.rs) saves each effective definition in `resource_pool_registration`, together with its revision and whether the provider has acknowledged it. A lost reply reuses that saved definition; it does not create another revision. Accepted definitions stay available so an allocation recovered after growth can still be checked against the values allowed when it started.

In the gated startup path, an authoritative replica (`listen_only = false`) registers remote pools from their saved seed definitions. Interrupted registration retries the saved request, including its provider pool ID and revision. An edited TOML declaration produces the existing drift diagnostic; it does not replace that request or grow the pool. A listen-only replica requires an accepted definition and checks the provider identity without sending `Define`. Pending growth does not hide an earlier accepted revision.

Growth names the previous revision and appends declarations to its existing
ordered list. Earlier declarations determine the assignment mode when entries
overlap, so adding a manual range cannot change an existing automatic value.
Old revisions remain available for requests already using them. A definition
acknowledgement does not create provider inventory or promise capacity.

Ranges include their start and exclude their end. IPv4 network expansion
includes the network address and excludes broadcast; IPv6 address expansion
includes every address. Delegated IPv6 prefixes remain distinct from individual
addresses. The protobuf comments specify the endpoint and presence rules.

Wire prefixes require zero host bits. TOML parsing can preserve host bits in a
prefix such as `192.0.2.7/24`, so the
[`ResourcePoolEligibility` converter](../resource-pool/src/value.rs)
uses the network address (`192.0.2.0/24` here) for IPv4 and IPv6 address
prefixes and delegated-prefix parents. Prefix-range endpoints with host bits
remain invalid and are rejected. These
[pool conversions](https://github.com/dsx-ai-factory/infra-controller/issues/7634)
preserve the existing parsing, normalization, and eligibility rules, but do not
apply Integrated's `MAX_POOL_SIZE` inventory limit or enumerate values.

### Rejected Definitions

A provider error after NICo saves the `Define` request leaves the definition pending, including when the provider rejects it. The saved `resource_pool_registration` row identifies the binding and revision being retried. `accepted = false` means NICo has not recorded acknowledgement; it does not prove the provider has not accepted the request. An initial `Describe` failure occurs before NICo saves a request, so there is no registration to resume.

Registration errors identify the logical pool and binding. For an RPC failure, Core also reports the gRPC code. Inspect the provider's logs for diagnostic details; Core omits the raw provider message. After a provider, credential, or connection problem is corrected, a later authoritative startup retries the saved request with `registration::resume`. If no request was saved, startup uses `registration::define`.

Authoritative startup also stops when a pending growth revision fails, even if an older revision was accepted. Listen-only startup can use that earlier accepted revision without retrying growth.

A saved remote binding needs a configured provider connection even after its pool is removed from configuration, so recovery can finish work at the original provider.

If correction requires changing the saved definition itself, protocol v1 has no operation to replace or cancel that pending definition. No safe replacement procedure is available, and restarting with different TOML cannot resolve it. The planned v2.5 P25 task must define and verify that procedure before production activation. Refer to the [activation requirements](../../designs/resource-pool/resource-pool-backend.md#status-and-scope).

## Lost replies and delayed requests

Within one authority, `request_id` identifies an allocation attempt across all bindings. The provider saves the request inputs or an equivalent digest so it can reject changes, including a different binding. Active allocations retain the complete assignment. Replaying a request returns the same value, allocation ID, and optional route target policy. Different slots let one entity own several allocations without using its owner ID as a retry key.

`Recover` returning `unknown` only means the provider has not observed the request. For example, an allocation request might still arrive after that lookup. `Cancel` records that the attempt cannot allocate, and acknowledges terminal cancellation only after any allocation and its owned provisioning have been removed. If cleanup returns `pending`, the provider continues it for the same request. NICo repeats `Cancel` with the original identity and, when the reply is `pending`, checks `Recover` to see whether cleanup has finished. It stops only after observing `cancelled` or `released`. A delayed allocation request then observes the terminal state.

`Release` also makes the original request terminal. Its allocation ID identifies one reservation, so retrying an old release cannot free a later assignment of the same value, even to the same owner. Version 1 keeps terminal request records for the lifetime of the issuing authority; it has no expiry or forget operation. Compacted terminal records still need the binding, input conflict checks, and complete terminal result, including a saved failure's code and message.

Replaying `Allocate` or `AllocateExact` after cancellation or release returns
`FailedPrecondition`; `Recover` identifies the terminal state. A definite
allocation failure is retained as `failed` and replayed until `Cancel` marks
the attempt `cancelled`.

Success from an allocation means provider-owned provisioning is ready. Timeouts
and unavailable services leave the result uncertain; they do not mean the pool
is empty. Unsupported statistics return `Unimplemented`, and unavailable
statistics return `Unavailable`, rather than a successful zero count.

## Conformance and implementation boundaries

Production configuration rejects remote pool selection. [Shared operations and API startup](https://github.com/dsx-ai-factory/infra-controller/issues/8297) provide the generic lifecycle below that gate. VPC callers still require Integrated allocation; [VPC adoption](https://github.com/dsx-ai-factory/infra-controller/issues/7140) follows separately. Clients and providers must use the same identity contract before remote activation; reserving removed protobuf fields does not make a provider that requires a site ID compatible with this API.

The shared [`resource-pool` interface](../resource-pool/src/lib.rs) exposes `prepare_allocation` for either backend. Call it before starting the entity transaction: remote preparation saves the request and completes provider work. The returned `Allocation` reserves Integrated inventory or attaches the remote result through `allocate(&mut txn)`, alongside the entity's writes. If creation fails, roll back and request cleanup with `cancel`. Controllers can use `prepare_request` to get a `PreparedAllocationRequest`, commit their creation record with that request, then call `resolve` before the entity transaction. Consumer admission and value-policy checks remain with the caller.

Ownership lookup returns an `OwnedAllocation` whose `release` method either frees Integrated inventory or saves remote release intent in the entity transaction. The recovery task finishes provider cleanup after that transaction commits.

Controllers capture cleanup with `ResourcePoolRegistry::request_cancellation` in a transaction under the same creation lock used for preparation. Acquire that lock before allocation row locks in participating attachment and removal paths. The method requests cancellation for a Pending attempt and returns an `AllocationCleanup` identifying that attempt or its absence. Commit, then call `wait` outside the transaction. Before discarding creation IDs, reacquire the creation lock and recheck current identity, desire, and `is_complete`.

The [cleanup API contract](../resource-pool/src/cleanup.rs) documents capture states, errors, and the 30-second wait budget. The caller bounds the capture transaction. `wait` never selects a newer attempt, and a failed or interrupted wait leaves saved cleanup intent available for retry. Replacement outside the shared creation lock must use a new owner ID.

Older stored definitions remain readable and keep their original database bytes. Decoding ignores the removed field, so a retry encodes the remaining fields on the wire rather than reproducing those stored bytes.

The `test-support` feature exposes an in-memory `LocalProvider` and reusable conformance scenarios against the generated service trait. The fixture uses a finite inventory and retains its request history only while it is alive. The suite uses it to check replay, conditional release, and both sequential arrival orders for allocation and cancellation. Cleanup completes immediately by default. `LocalProvider::with_pending_cancellation` keeps the reservation and simulated provisioning until the test calls `complete_cleanup`. This lets recovery tests observe unfinished cleanup across coordinator restarts. Running the suite requires a Tokio runtime with time enabled; testing a remote client requires an adapter to the generated service trait.

The [gRPC client tests](../resource-pool/src/grpc/tests.rs) exercise delayed allocation and lost replies over a local server. `terminal_cancel_prevents_an_allocation_already_in_flight` holds an allocation at the server while cancellation finishes, then verifies that the delayed allocation cannot reserve the value. The [coordinator tests](../resource-pool/src/remote/tests.rs) add PostgreSQL persistence, restart recovery, and cleanup that returns `pending`. These tests keep the local provider alive while reconstructing Core's coordinator; they do not prove that an external provider preserves reservations across its own restart or completes real network provisioning. The planned v2.5 P25 activation task owns that provider qualification and offline database restore testing. Providers whose RT policy can change must also prove that policy updates leave existing assignments intact.

Run the package tests, including the generated client/server loopback test:

```bash
cargo test -p carbide-resource-pool-api
```

Core's [pool conversions](https://github.com/dsx-ai-factory/infra-controller/issues/7634) live in [`carbide_resource_pool::value`](../resource-pool/src/value.rs). Core's transport adapter belongs to [P05](https://github.com/dsx-ai-factory/infra-controller/issues/7134). Saved operations belong to [P06](https://github.com/dsx-ai-factory/infra-controller/issues/7135), and recovery polling belongs to [P07](https://github.com/dsx-ai-factory/infra-controller/issues/7136). The [shared startup integration](https://github.com/dsx-ai-factory/infra-controller/issues/8297) wires the coordinator and cleanup sampler into API startup and shutdown.

`carbide_resource_pool_cleanup_pending` counts saved cancellation and release requests awaiting confirmation. On replicas with `listen_only = false`, the API starts the sampler even when every pool uses Integrated, and the integration test checks its emitted series. Listen-only replicas start neither recovery nor the sampler. The sampler emits no value before its first successful query. It then retains the last count, with `fresh="true"` while that sample is less than 30 seconds old and `fresh="false"` afterward. Neither this crate nor these conversions activate a new backend. Refer to the [execution and recovery design](../../designs/resource-pool/resource-pool-backend.md) for the complete lifecycle, release boundaries, and metric sampling contract.
