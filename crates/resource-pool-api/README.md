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

`PoolBinding` identifies the NICo pool and the provider pool backing it.
`OperationIdentity` pairs that binding with a request ID. `Recover` and `Cancel`
accept it in their request's `operation_identity` field and return it alongside
`operation_status`. The returned identity matches the request, including when
`Recover` reports `unknown`.

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

`Define` describes effective eligibility, including values added by explicit
growth. It is not a copy of the original TOML seed snapshot, which does not
record later growth. Each revision is immutable and must be acknowledged before
an allocation can use it.

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

## Lost replies and delayed requests

Within one authority, `(site_id, request_id)` identifies an allocation attempt.
The provider keeps enough immutable request information to reject changed inputs,
either the inputs themselves or an equivalent digest. Active allocations retain
the complete assignment. Replaying a request returns the same value, allocation
ID, and optional route target policy. Different slots let one entity own several
allocations without using its owner ID as a retry key.

`Recover` returning `unknown` only means the provider has not observed the
request. For example, an allocation request might still arrive after that
lookup. `Cancel` records that the attempt cannot allocate, and acknowledges
terminal cancellation only after any allocation and its owned provisioning
have been removed. If cleanup returns `pending`, the provider continues it for
the same request; NICo polls `Recover` until it sees `cancelled` or `released`.
A delayed allocation request then observes the terminal state.

`Release` also makes the original request terminal. Its allocation ID identifies
one reservation, so retrying an old release cannot free a later assignment of
the same value, even to the same owner. Version 1 keeps terminal request records
for the lifetime of the site and authority namespace; it has no expiry or
forget operation. Compacted terminal records still need the binding, input
conflict checks, and complete terminal result, including a saved failure's code
and message.

Replaying `Allocate` or `AllocateExact` after cancellation or release returns
`FailedPrecondition`; `Recover` identifies the terminal state. A definite
allocation failure is retained as `failed` and replayed until `Cancel` marks
the attempt `cancelled`.

Success from an allocation means provider-owned provisioning is ready. Timeouts
and unavailable services leave the result uncertain; they do not mean the pool
is empty. Unsupported statistics return `Unimplemented`, and unavailable
statistics return `Unavailable`, rather than a successful zero count.

## Conformance and implementation boundaries

The `test-support` feature exposes an in-memory `LocalProvider` and reusable
conformance scenarios against the generated service trait. The fixture uses a
finite inventory and retains its request history only while it is alive. The
suite uses it to check replay, conditional release, and both sequential arrival
orders for allocation and cancellation. Cleanup completes immediately in this
fixture. Running the suite requires a Tokio runtime with time enabled; testing
a remote client requires an adapter to the generated service trait.

P05 and P07 must add tests for concurrent allocation and cancellation, actual
provisioning, restart recovery, and cleanup that returns `pending` before remote
activation. Providers whose RT policy can change must also prove that policy
updates leave existing assignments intact.

Run the package tests, including the generated client/server loopback test:

```bash
cargo test -p carbide-resource-pool-api
```

Core's [pool conversions](https://github.com/dsx-ai-factory/infra-controller/issues/7634)
live in [`carbide_resource_pool::value`](../resource-pool/src/value.rs).
Core's transport adapter belongs to
[P05](https://github.com/dsx-ai-factory/infra-controller/issues/7134).
Saved operations belong to
[P06](https://github.com/dsx-ai-factory/infra-controller/issues/7135), and recovery
polling belongs to
[P07](https://github.com/dsx-ai-factory/infra-controller/issues/7136).
Neither this crate nor these conversions activate a new backend. See the
[execution and recovery design](../../designs/resource-pool/resource-pool-backend.md)
for the complete lifecycle and release boundaries.
