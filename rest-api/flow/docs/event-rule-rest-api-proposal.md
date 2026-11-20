# Event Rule REST API Proposal

This document records the proposed generic event-rule REST API design. The
implemented contract is defined by the [OpenAPI specification](../../openapi/spec.yaml) and
[REST routes](../../api/pkg/api/routes.go).

The [default-policy migration plan](event-rule-default-policy-migration-plan.md)
defines the planned replacement of the built-in fallback with an ordinary
persisted Site default. Its selection and initialization rules supersede the
built-in behavior described below when that migration is implemented.

All paths below are relative to `/v2/org/{org}/nico`. The `/event` namespace
provides shared policy, binding, handling, and pause/resume operations across
event types. The initial supported `eventType` is `hardware.leak.detected`.
The policy, binding, handling, and Task-list operations are implemented in this
branch. Pause/resume operations below remain proposed and are outside this
implementation. Additional event types require defined payloads, conditions, actions, and
supported binding scopes before they can be accepted by the API.

`eventType` uses Flow's registered event-type identifier. Its placement and
requiredness depend on the operation:

| Operation | `eventType` contract |
| --- | --- |
| Create policy | Required in the JSON body; immutable after creation |
| Read/update/delete a policy by UUID or list its bindings | Inferred from the policy; no separate selector |
| List policies, handlings, or Tasks | Optional query filter; omission includes all supported event types |
| Read/delete a Site or Rack binding | Required query parameter |
| Assign a Site or Rack binding | Required in the JSON body |
| Pause/resume handling or Tasks | Required in the JSON body, together with an explicit scope; applies only to that event type |

Missing required or unsupported `eventType` values return `400`. Binding changes
also reject a policy whose event type differs from the requested `eventType`.
There is no implicit all-event-types binding or pause operation.

Task actions use a shared operation definition with exactly one power or
firmware operation. Optional `submitTask.operation.ruleId` selects an Operation
Rule UUID, distinct from the event-policy UUID. Omitted or null uses normal
operation-rule resolution. Flow retains the reference in the captured action
and execution plan. It validates the UUID at policy creation and checks the
referenced rule during Task submission and execution. An explicit invalid
reference does not fall back. Referencing a rule does not freeze its definition.

Policy definitions are reusable and immutable; only their enabled state can
change. A policy's `id` is Flow's event-rule UUID. Policies can be shared across
bindings within the selected Site for the same event type.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/event/policy` | Create a policy, enabled by default, and return its UUID |
| GET | `/event/policy` | List policies, including disabled, unbound, and built-in policies |
| GET | `/event/policy/{policyId}` | Read the full policy and its enabled state |
| PATCH | `/event/policy/{policyId}` | Set `enabled: true` or `false` |
| DELETE | `/event/policy/{policyId}` | Delete an unbound persisted policy |

GET `/event/policy` accepts an optional `bound` filter:

- Omitted: return all policies.
- `bound=true`: return policies with at least one explicit binding.
- `bound=false`: return policies with no explicit bindings.

Each policy appears once, regardless of the number of scopes referencing it.
Binding presence is independent of `enabled`, so disabled policies can still
match `bound=true`. Built-in fallback use does not count as a binding.
The optional `enabled` filter selects policies by enabled state and can be
combined with `bound` and `eventType`. Filters apply before pagination.

The catalog exposes `id`, `name`, `eventType`, `enabled`, and `readOnly`, allowing
callers to select a policy and inspect its actions and conditions before binding
it. PATCH accepts only `siteId` and `enabled`. Changes to the event type, name, description,
actions, or conditions require a new policy UUID. Built-in policies remain
read-only and cannot be explicitly bound. Deleting a policy that still has
bindings returns `409`.

POST accepts an optional `enabled` field that defaults to `true`. Callers can
provide `enabled: false` for staging workflows. An enabled persisted policy
without a binding does not participate in policy selection, so creation alone has
no operational effect. Flow must validate and persist the policy with its initial
enabled state atomically in one operation.

The normal activation workflow has two steps:

1. POST `/event/policy` with `eventType` to create an enabled policy.
2. PUT the desired binding to assign that policy to a Site or Rack scope.

To replace an assigned policy, create the new enabled policy, then atomically
switch the desired bindings. The old policy can be retired when it is no longer
needed. If a replacement was created disabled, enable it while unbound before
switching the bindings.

Bindings associate an existing policy UUID with an event type and scope. Each
Site or Rack has at most one explicit binding per event type. Different event
types have independent assignments. The scope paths below represent explicit
assignments for event types supporting those scopes. Leakage supports both Site
and Rack bindings; unsupported event-type/scope combinations return `400`.

| Method | Path | Purpose |
| --- | --- | --- |
| GET, PUT, DELETE | `/event/policy/binding/site` | Read, assign/reassign, or remove the Site binding for an event type |
| GET, PUT, DELETE | `/event/policy/binding/rack/{rackId}` | Read, assign/reassign, or remove a Rack binding for an event type |
| GET | `/event/policy/binding` | List explicit bindings with optional event-type, scope, Rack, and policy filters |

PUT takes `siteId`, `eventType`, and `policyId` and creates or replaces a binding
atomically. It covers both initial assignment and reassignment, with atomic
replacement per scope and event type. Repeating the same assignment is
idempotent. DELETE removes the binding while preserving the policy definition.
A binding remains visible when its policy is disabled.

Scope GETs return the explicit assignment, `eventType`, and referenced `policyId`.
An absent assignment returns `404`; these reads do not resolve inherited or built-in
policies.

The binding collection accepts optional `eventType`, `scopeType`
(`Site` or `Rack`), `rackId`, and `policyId` query filters, combined with AND.
Omitted filters include all assignments in the required `siteId`, including
bindings referencing disabled policies. `rackId` accepts an external inventory
Rack ID, including UUID-shaped external IDs, and cannot be combined with
`scopeType=Site`. Results use
`SCOPE_ASC`: Site first, then canonical Rack UUID, event type, and binding UUID
ascending, with ordering applied before pagination. No matches, including an
unknown policy UUID, return `200` with an empty list. Binding UUIDs remain internal.

Scoped GET and DELETE require `siteId` and `eventType` in the query. Scoped PUT
requires `siteId`, `eventType`, and `policyId` in the JSON body. PUT returns `201`
when creating an assignment and `200` when replacing or repeating an existing
assignment. Reassignment preserves the binding identity and referenced policy
definitions. No POST or PATCH binding operation is exposed.

For leakage, Flow resolves enabled Rack policy, enabled Site policy, then
built-in policy when processing an event. Resolution is confined to the event's
type. Each additional event type must define its supported scope precedence and
fallback behavior. Disabling a policy preserves its bindings but skips it during
selection, permitting the defined fallback. Effective-policy resolution remains
internal to Flow.

`NoAction` remains a supported leakage-policy behavior. An enabled `NoAction`
policy selects an explicit no-remediation response and prevents policy fallback:

1. Create an enabled `NoAction` policy, or select an existing one.
2. PUT that policy UUID to the desired Site or Rack assignment.
3. Resume remediation by explicitly assigning the desired response policy again.

A Site `NoAction` assignment affects Racks inheriting the Site default. Enabled
explicit Rack assignments still take precedence.

Pause and resume use one endpoint per operation, with scope and action selection
in a structured JSON body. An all-actions pause stops all new policy actions for
the selected event type. A Task-specific pause stops only new Task creation,
allowing other policy actions to continue. Both preserve policy definitions,
enabled states, and bindings.

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/event/handling/pause` | Set a pause for the selected event type, scope, and action selection |
| POST | `/event/handling/resume` | Clear the pause for the selected event type, scope, and action selection |

Both endpoints accept the same request shape. For example, this body selects
Task creation for Rack `rack-17`:

```json
{
  "siteId": "550e8400-e29b-41d4-a716-446655440000",
  "eventType": "hardware.leak.detected",
  "scope": {
    "type": "Rack",
    "rackId": "rack-17"
  },
  "actionType": "SubmitTask"
}
```

`siteId`, `eventType`, and `scope` are required. `siteId` is the Site routing
UUID. `scope` must select exactly one supported scope:

| `scope.type` | Scope fields | Meaning |
| --- | --- | --- |
| `Site` | `rackId` is forbidden | All sources of the event type within the selected Site |
| `Rack` | Nonempty `rackId` is required | Rack and component sources within that Rack in the selected Site |

`scope.rackId` accepts the same external inventory Rack ID as the
binding endpoints. Omission of `scope` does not default to the entire Site.
Missing, empty, unknown, or incompatible scope fields return `400`, as does a
Rack that cannot be resolved in the selected Site.

`actionType` is optional. Omission selects the all-actions control;
`actionType: "SubmitTask"` selects the independent Task-creation control. Empty,
null, or unsupported action selectors return `400`. Task-specific controls
require an event type supporting Task creation; Rack controls require support
for Rack context. Unsupported combinations return `400`.

These operations set the desired pause state explicitly, are idempotent, and
return `200` with the resulting pause state, including when the selected pause
was already set or cleared. Responses identify the selected scope and control
and expose the applicable all-actions and Task-specific pause states and their
effective result. Rack responses include the Rack's own states and the Site
states that contribute to the effective result. Pause state is keyed by Site,
event type, scope, and action selection, so changing one event type does not
affect another.

A Site pause covers all sources of the selected event type, including Racks with
explicit policy bindings and components without Rack context. A Rack pause covers
Rack-origin events and component-origin events in that Rack, using the handling's captured
Rack context. This scope applies to both all-actions and Task-specific controls.

A policy action is paused when either the Site or its source Rack has an all-actions
pause. Task creation is additionally paused when either scope has a Task-specific
pause. Resuming clears only the selected control: a resume with
`actionType: "SubmitTask"` cannot override an all-actions pause, and a resume
with `actionType` omitted clears only the all-actions pause while preserving
Task-specific pauses. Resuming a Site control preserves Rack controls; resuming
a Rack control cannot override a Site pause.

Flow must check all-actions pause state before starting each policy action and
check both all-actions and Task-specific pause states immediately before creating
each Task. These checks include previously prepared actions and retries. A
successful pause response means subsequent action starts or Task creation,
respectively, are blocked for that scope. Existing Tasks and other actions
already started continue; subsequent Task creation remains subject to the pause
checks. Tasks can be cancelled individually. Handling results identify actions
suppressed by a pause. Policy selection continues under either control.

Pause state persists until explicitly resumed. Resume permits subsequent actions
subject to the remaining pause controls and does not automatically replay
suppressed actions. Policy changes affect subsequent handling decisions
independently of pause state.

Handling reporting describes policy processing triggered by events.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/event/handling` | Report handlings, optionally filtered by event type |

The query accepts optional `eventType`, `rackId`, and `policyId` filters, combined
with AND when supplied. Omitting `eventType` includes all supported event types.
`rackId` matches the captured Rack context, including component-origin events in
that Rack. `policyId` matches the captured applied
policy UUID. Configuration changes and component moves do not change those
associations. `sourceType` and `sourceId` select an exact event source.

Each returned handling aggregates the event's policy actions and Tasks. It
includes the captured event with its `eventType` and the applied policy UUID
and matched actions,
Flow-computed handling status and diagnostics, and Task IDs and statuses.
Individual action executions and retry attempts remain Flow implementation details.

Flow retains the durable processing state required for deduplication, retries,
and Task dispatch. This endpoint provides operational handling reporting; the
proposal does not establish a general event-history service.

Task inspection uses the recorded policy that triggered the work, independently
of its subsequent binding or enabled state. Users list matching Tasks and cancel
them individually through the existing Task API.

| Method | Path | Purpose |
| --- | --- | --- |
| GET | `/event/handling/task` | List event-triggered Tasks, optionally filtered by `eventType`, `policyId`, and `rackId` |

The event-triggered Task list reuses the existing Task response model and includes
terminal Tasks by default. Its `eventType`, `policyId`, and `rackId` filters use
the originating handling's captured associations and combine with AND when
supplied. Omitting `eventType` includes Tasks from all supported event types.
`activeOnly=true` restricts the list to Flow's waiting, pending, and running Tasks.
The REST model represents both waiting and pending Tasks as `Pending`. Results
use stable newest-first ordering.

The following conventions apply across the proposed surface:

- `siteId` goes in POST/PUT/PATCH JSON bodies and GET/DELETE query parameters.
- Lists use `pageNumber`, `pageSize`, stable ordering, and `X-Pagination`.
- Full policy definitions and captured event metadata use structured schemas
  with event-specific validation. Raw source payloads are not persisted or
  returned; future payload fields require their own structured contract. Each event type defines its supported actions,
  conditions, source types, and binding scopes; arbitrary JSON is not exposed.
- Policy reads expose `enabled`; PATCH sets the desired state explicitly and is
  safe to repeat.
- Flow owns policy selection, durable pause state, action and Task-creation
  gating, and handling results; REST proxies these operations.
- Static routes such as `binding` must precede
  `/{policyId}` routes that could capture them.

The proposed surface requires Flow support for atomic creation with an initial
enabled state, binding-presence filtering, filtered binding listing, atomic
binding replacement, policy lifecycle enforcement, and handling/Task queries
filtered by captured Rack and policy associations. It also requires independent,
durable Site and Rack controls for all-actions pauses and Task-specific pauses,
coordinated with action starts and Task creation, including retry paths. Queries,
bindings, pause controls, and response models must preserve event-type isolation
and support typed event payloads. The initial implementation accepts only
`hardware.leak.detected`; the shared lifecycle can support additional event types
once their contracts are defined.
See the [Flow gRPC definitions](../proto/v1/flow.proto),
[event-rule architecture](event-rule-architecture.md), and
[event rules usage guide](event-rules-guide.md) for implementation context.

One behavior remains open: whether an explicit re-evaluation operation is needed
for ongoing events first observed under `NoAction` or while paused. Flow's event
deduplication does not automatically replay those occurrences.
