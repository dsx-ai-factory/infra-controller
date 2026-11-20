# Event Rules Usage Guide

This guide explains how to configure and operate Flow event rules through the
gRPC and REST APIs. For processing, persistence, deduplication, and claim-recovery
details, see [Event-Rule Architecture](event-rule-architecture.md). For exact
protobuf definitions, see the [gRPC API reference](grpc-api.md).

## Supported event type

| Event type | Producer | Built-in response |
|---|---|---|
| `hardware.leak.detected` | NICo leakage polling for racks, compute components, and NVSwitch components | Force-power-off affected components |

Flow rejects a syntactically valid but unregistered event type with
`InvalidArgument`. The v1 API does not provide a method to list supported event
types.

## Recommended workflow

1. Inspect the built-in rule for an inventory target.
2. Create a persisted rule. gRPC defaults to disabled when `enabled` is omitted;
   an explicit initial state is validated and persisted atomically.
3. Bind the rule to an existing rack or to the site.
4. Enable the rule.
5. Resolve the effective rule for the intended rack or component and verify
   its UUID.
6. Exercise the event producer only after that verification.

Rules and bindings can be prepared without changing event processing because
only enabled persisted rules participate in selection.

## Rule contract

A rule contains a UUID, name, description, enabled state, event type, and one
or more uniquely named actions.

Rule names are required, may contain spaces, must not have leading or trailing
whitespace, and are limited to 128 bytes. Action names are required
lower-snake-case identifiers such as `power_off_affected_components`.

The response field `read_only` identifies built-in rules:

- built-in rules have `read_only = true`, are always enabled, and cannot be
  mutated or bound; and
- persisted rules have `read_only = false`; gRPC creation defaults to disabled
  when `enabled` is omitted, while REST creation defaults to enabled.

Attempts to update, enable, disable, delete, or bind a built-in rule return
`FailedPrecondition`.

### Conditions

Each action can restrict matching by event severity and resolved component
type:

- values within `severities` use OR semantics;
- values within `component_types` use OR semantics;
- the two fields combine with AND semantics; and
- an omitted or empty protobuf list imposes no constraint.

A component-type condition does not match a rack event. If no action matches,
Flow records the event without creating an action execution.

### Actions

| Action | Behavior | Standard service availability |
|---|---|---|
| `submit_task` | Resolves targets and submits one idempotent task per rack | Available |
| `noop` | Completes without an external side effect | Available |
| `send_alert` | Sends one idempotent alert | Rule creation is rejected unless an alert sender is supplied when the event-rule manager is assembled |

A `submit_task` action requires exactly one typed `TaskOperation`. The protobuf
contract supports power-control and firmware-control operations. Targets must
not be embedded in the operation; Flow derives them from the event and target
strategy.

`TaskOperation.rule_id` optionally selects an Operation Rule UUID for either
power or firmware work. Omission uses normal rule resolution. The selected UUID
is retained in the policy snapshot and execution plan, and forwarded to Task
submission. Flow checks existence at submission and uses the explicit rule instead of
normal operation-rule matching when executing the Task. An unavailable explicit rule never
falls back to a default. Policy creation validates the UUID without requiring
the referenced Operation Rule to exist yet. The UUID pins identity, not a
version of the Operation Rule definition.

REST exposes the same selector as `submitTask.operation.ruleId`, alongside
`powerControl` or `firmwareControl`. Its shared models are `APITaskOperation`,
`APIPowerControl`, and `APIFirmwareControl` in
[operations.go](../../api/pkg/api/model/operations.go).

Task conflict strategies are:

- `QUEUE`: queue the task behind conflicting work.
- `REJECT`: reject conflicting work. Task-submission errors are retryable under
  the event-rule execution policy.

An event-action execution is complete when the task manager accepts its task;
it does not wait for the task's Temporal workflow. The task has its own status
lifecycle. Trigger linkage is stored in the database but is not exposed by the
v1 `Task` response.

### Target strategies

| Strategy | Resolution |
|---|---|
| `SOURCE` | The inventory resource that produced the event: that component for a component event, or all known components in that rack for a rack event. |
| `COMPONENT` | Deprecated gRPC name for `SOURCE`, with the same numeric value and source-aware behavior. |
| `RACK` | All currently known components in the event component's owning rack or in the event rack. |
| `AFFECTED_COMPONENTS` | The targets chosen by the resolver for the event type. Without an event-specific resolver, this selects the event component or all currently known components in the event rack. |

For `hardware.leak.detected`, `AFFECTED_COMPONENTS` selects the leaking
component and components in the same rack with valid, strictly lower slot IDs.
A rack event selects all currently known components in the rack. See
[Target resolution](event-rule-architecture.md#target-resolution) for resolver
selection, failure behavior, and the limitations of this slot-based model.

### Source strategy compatibility

`SOURCE` expresses source-based intent independently of a policy's binding
scope. A site or rack policy can receive both component and rack events; Flow
resolves the concrete target from each event's inventory source kind. Rack
targets expand to their known components. An empty target set produces a
skipped execution with reason `no_targets`.

The REST event-policy value `Source` maps to gRPC `SOURCE` (number 1).
`Rack` maps to `RACK` (number 2), selecting all known components in the
event rack or the reporting component's owning rack. `AffectedComponents` maps
to `AFFECTED_COMPONENTS` (number 3).

The deprecated gRPC name `COMPONENT` remains an alias for value 1, preserving
existing generated client symbols and binary messages. It is declared before
`SOURCE` so ProtoJSON output retains `EVENT_RULE_TARGET_STRATEGY_COMPONENT`,
which older clients recognize. ProtoJSON input accepts both names. Inside Flow,
value 1 is represented only as `TargetStrategySource`.

Version 1 policy persistence continues writing the `component` token for source
selection so existing readers can decode new writes. Reading either `component`
or `source` normalizes it to the internal source strategy. The `rack` and
`affected_components` encodings are unchanged, and previously materialized
execution plans retain their captured targets.

Value 1 intentionally gains rack-event support: a rack event now selects that
rack's known components instead of failing component-only target resolution.
Upgrade all event processors before relying on this behavior. Older processors
can read the compatible encoding but still interpret value 1 as component-only.
The built-in affected-components policy is unchanged.

### REST event policies and bindings

All paths in this section are relative to `/v2/org/{org}/nico`. Requests require
Provider Admin authorization, an owned Site, and Flow enabled on that Site.
The initial supported `eventType` is `hardware.leak.detected`; unsupported types
return 400. Site selection is required: `siteId` belongs in POST/PUT/PATCH bodies
and GET/DELETE query parameters.

| Method | Path | Contract |
|---|---|---|
| POST | `/event/policy` | Create a typed policy, enabled by default; return 201 with its UUID and definition. |
| GET | `/event/policy` | Catalog including built-in, disabled, and unbound policies. |
| GET | `/event/policy/{policyId}` | Read the complete definition and enabled state. |
| PATCH | `/event/policy/{policyId}` | Accept only `siteId` and required Boolean `enabled`. |
| DELETE | `/event/policy/{policyId}` | Delete an unbound persisted policy; bound policies return 409. |
| GET, PUT, DELETE | `/event/policy/binding/site` | Read, replace, or remove an explicit Site assignment. |
| GET, PUT, DELETE | `/event/policy/binding/rack/{rackId}` | Read, replace, or remove an explicit Rack assignment. |
| GET | `/event/policy/binding` | List explicit bindings with optional event-type, scope, Rack, and policy filters. |

An enabled unbound policy has no effect on selection. Create with
`enabled: false` when a staged workflow is needed. Each policy owns one event
type, and each Site or Rack has at most one assignment per event type.

Policy definitions cannot be edited through REST; create a new policy and
atomically replace the desired assignments. PUT takes `siteId`, `eventType`,
and `policyId`, returns the assignment, and preserves policy definitions and
enabled states. Repeating the same assignment is idempotent. A shared policy
may be referenced by the Site and multiple Racks. Built-in policies are
read-only and cannot be explicitly assigned.

Scoped binding GET and DELETE require query `eventType`; a missing assignment returns
404, even when Flow would select a fallback. DELETE removes only the assignment.
These reads do not resolve effective policies. Operations addressed by policy
UUID infer the event type and do not accept an additional selector.

The catalog accepts optional `eventType`, `enabled`, and `bound` filters,
combined with AND. `bound=true` selects policies with at least one explicit
assignment; `bound=false` selects none; omission includes both. Every policy
appears once, independent of reference count. Built-in fallback use does not
count as an assignment. Ordering is persisted policies by UUID ascending,
followed by built-ins by UUID ascending (`ID_ASC`). Binding lists use `SCOPE_ASC`:
Site first, then canonical Rack UUID, event type, and binding UUID ascending.
A Rack binding reports its external inventory ID, or `null` when that ID is
unavailable. Site bindings also report `rackId: null`.

The binding collection accepts optional `eventType`, `scopeType` (`Site` or
`Rack`), `rackId`, and `policyId` filters, combined with AND. Omitted filters
include all explicit assignments, including disabled policies. A Rack filter
accepts its external inventory ID, including UUID-shaped external IDs, and
cannot be combined with Site scope. No matches return `200` with an empty list.
Scoped PUT returns `201` on creation and `200` on replacement or an unchanged
assignment. Binding UUIDs
remain internal; scoped reads do not resolve inheritance. All REST Rack path
selectors and Rack filters use external inventory IDs; Flow resolves them to
internal UUIDs.

Each action has a lower-snake-case name, an optional typed condition, and exactly
one non-null `submitTask`, `sendAlert`, or `noAction` specification. Names are
unique within the policy. Unselected specifications are serialized as null.
`submitTask` contains `targetStrategy`, `onConflict`, an optional description,
and a typed power-control or firmware-control operation. Alert creation requires
a configured Flow alert sender. The [OpenAPI specification](../../openapi/spec.yaml)
defines all supported values, required fields, and action exclusivity constraints.

For example, the following body creates an enabled no-op policy without changing
selection until a binding references its returned UUID:

```json
{
  "siteId": "550e8400-e29b-41d4-a716-446655440000",
  "name": "Observe leakage",
  "eventType": "hardware.leak.detected",
  "actions": [
    {"name": "record", "noAction": {"reason": "Explicit no-op response"}}
  ]
}
```

Selecting an enabled no-op policy prevents fallback for that scope. Disabling a
policy instead permits fallback and does not cancel existing Tasks. This REST
surface has no pause/resume or bulk-cancellation endpoints.

### REST event handling and Tasks

`GET /event/handling` returns operational policy-handling results. Optional
`eventType`, `policyId`, and `rackId` filters combine with AND. Omitted event type
includes all supported types. `sourceType` (`Component` or `Rack`) and `sourceId`
must be supplied together to select an exact source. No source selector is
required when listing handlings more broadly.

Each record includes the source and `eventType`, captured policy UUID and matched
actions, `handlingStatus`, nullable diagnostic `reason`, curated whole-handling
`message`, Tasks with their IDs and statuses, and `handledAt`. Flow computes the
result; REST does not reconstruct it from Task counts. An empty `tasks` array
can represent a failure, retry, skip, or no action.

`detectedAt` is the first accepted source observation time; it can precede
`handledAt` and is not necessarily the condition start time. Duplicate
observations preserve it. Older records or sources without this timestamp
return null. Event and execution UUIDs, observation counts, and the original raw
event payload remain internal or are not retained.

Rack filtering uses the captured source Rack UUID, including component-origin
events. Moving a component later does not change the result. Predecessor records
without captured Rack context are excluded from Rack-filtered results; their
original Rack is never guessed from current inventory. Policy filtering uses
`events.applied_rule_id`, preserving membership after policy changes or deletion.

`GET /event/handling/task` lists event-triggered Tasks using the same captured
`eventType`, `policyId`, and `rackId` associations. It reuses the Task response
model without report expansion and includes terminal Tasks by default.
`activeOnly=true` restricts results to Flow's Waiting, Pending, and Running
states. Clients can cancel listed Tasks individually. A Task's own operation-rule
UUID is distinct from the event-policy UUID used by these filters.

All REST lists use `pageNumber` (default 1), `pageSize` (default 20, maximum 100),
and `X-Pagination`, containing `pageNumber`, `pageSize`, `total`, and `orderBy`.
Handling order is `HANDLED_AT_DESC` (handling time, then event UUID descending).
Task order is `CREATED_DESC` (creation time, then Task UUID descending). Filtering
and ordering precede slicing; empty and out-of-range pages return `[]` with the
matching total. Other orderings are rejected.

The corresponding Flow reads are `ListEventHandlings` and `ListEventTasks`, with
optional typed filters and `Pagination`. To retrieve handling history for one
source, set `eventType`, `resourceType`, and `resourceId` in the handling filter.

### Handling result aggregation

Flow's overall status follows this precedence across all applicable actions:
`Failed` > `Retrying` > `Starting` > `Unknown` > `Running` > `Skipped` >
`Completed` > `NoAction`. Any failed or terminated Task makes the handling
`Failed`. Otherwise, failed actions dominate retries, and retries dominate
actions still being dispatched. Missing or duplicate matching executions,
unrecognized states, and completed submissions without their own Tasks produce
`Unknown`. Tasks from another action cannot satisfy a submission. While any
known action is still active, it takes precedence over skipped or completed
work. A partial skip becomes `Skipped` after other work completes; its message
states that one or more actions were skipped. No applicable actions, or only
completed no-op actions, produce `NoAction`. Alert actions can complete without
Tasks. When several actions are retrying, `AttemptFailed` takes precedence over
`AttemptInterrupted`, then `RetryScheduled`, independently of action ordering.

REST preserves Flow's generic `NoAction`, `Running`, and `Completed` names.
These describe handling progress, not the source's current condition or power state. Per-task statuses match the Task API:
`Waiting` maps to `Pending`, `Completed` maps to `Succeeded`, and unrecognized
values map to `Unknown`. Task ordering remains creation time then Task UUID
ascending, and an empty list is serialized as `[]`.

The gRPC handling response exposes aggregate status, reason, and message,
alongside the captured event, policy, and Tasks. Individual executions, attempt
counts, and retry schedules are internal processing details. Unspecified or
unrecognized aggregate statuses map to `Unknown` in REST without reconstructing
progress from Tasks. Human-readable messages are curated in Flow and never
include raw backend diagnostics.

## Binding and selection contract

A binding assigns a persisted rule to one scope:

- `SITE` must omit its ID.
- `RACK` requires the UUID of an existing Flow inventory rack.

The binding event type is derived from the rule; clients cannot provide a
different type. For each event type, only one rule can occupy the site scope
and only one rule can occupy each rack scope. A rule may be bound to several
racks. `PutEventRuleBinding` also permits sharing the same rule across Site
and Rack scopes; `CreateEventRuleBinding` retains its original mixed-scope
restriction for callers using that operation.

Binding APIs use `(event_type, scope)` for get and delete. The binding UUID is
returned as data but is not the deletion key. `PutEventRuleBinding` atomically
assigns or replaces an existing policy for a scope. `ListEventRuleBindings`
returns its explicit references. `DeleteEventRule` always rejects bound policies
atomically with binding writes, even when the policy is disabled. Remove or
replace every binding before deleting its policy.

Flow selects the first enabled rule in this order:

1. rack binding;
2. site binding; and
3. built-in fallback.

A binding to a disabled rule remains stored but is skipped. Consequently,
`GetEventRuleBinding` can return a custom rule while
`GetEffectiveEventRule` returns the site rule or built-in fallback. Enabling
the custom rule makes that existing binding effective.

`GetEffectiveEventRule` requires an existing rack UUID or component UUID. A
component is resolved to its owning rack before rule selection.

## Leakage runtime configuration

The leakage job requires a registered NICo provider. Without one, Flow does not
create the job.

| Setting | Default | Contract |
|---|---|---|
| `disable_leak_detection` | `false` | `true` disables the leakage job entirely. |
| `leak_detection_interval` | `1m` | Positive Go duration between scheduled collections. |

When leakage detection is not disabled,
`FLOW_EVENT_RULE_LEAK_DETECTION_ENABLED` selects the implementation at service
startup:

- unset or a false value selects the legacy direct leakage handler;
- a true value selects event-rule ingestion; and
- an invalid value fails leakage-job setup.

Accepted true values are `1`, `t`, `T`, `TRUE`, `true`, and `True`. Accepted
false values are `0`, `f`, `F`, `FALSE`, `false`, and `False`. Changing the
value requires a Flow restart.

```bash
export FLOW_EVENT_RULE_LEAK_DETECTION_ENABLED=true
./flow serve --dev-mode
```

### Helm deployment

The `nico-flow` chart maps `flowConfig.disableLeakDetection` and
`flowConfig.leakDetectionInterval` to the corresponding settings in
`/etc/flow/flowconfig.yaml`. Their chart defaults are `false` and `1m`,
respectively. Use `extraEnv.flow` to set the implementation selector on the
Flow container:

```yaml
flowConfig:
  disableLeakDetection: false
  leakDetectionInterval: 1m

extraEnv:
  flow:
    - name: FLOW_EVENT_RULE_LEAK_DETECTION_ENABLED
      value: "true"
```

`flowConfig.disableLeakDetection: true` prevents the leakage job from being
created, regardless of the environment variable. When leak detection is not
disabled, `FLOW_EVENT_RULE_LEAK_DETECTION_ENABLED` selects the legacy or
event-rule implementation, and `flowConfig.leakDetectionInterval` controls how
often that implementation runs. A Helm upgrade that changes these values rolls
the Flow pod so the file and environment changes take effect.

See the [`nico-flow` chart documentation](../../../helm/nico-flow/README.md)
for the complete chart configuration contract.

## gRPC API

| Method | Contract |
|---|---|
| `CreateEventRule` | Atomically creates a persisted rule with its initial state; omission defaults to disabled. |
| `GetEventRule` | Gets a persisted or built-in rule by UUID. |
| `GetEffectiveEventRule` | Resolves the effective rule for an existing rack or component. |
| `ListEventRules` | Filters and paginates persisted and built-in rules. |
| `SetEventRuleEnabled` | Sets persisted-rule participation in selection; requires an explicit `enabled` value. |
| `DeleteEventRule` | Deletes an unbound persisted rule; rejects any remaining bindings. |
| `CreateEventRuleBinding` | Occupies one site or rack selection scope. |
| `PutEventRuleBinding` | Atomically creates or replaces a Site or Rack assignment to an existing policy, preserving its definition and enabled state. |
| `GetEventRuleBinding` | Gets the binding at an event-type and scope pair. |
| `ListEventRuleBindings` | Filters and paginates explicit assignments, including bindings to disabled policies. |
| `DeleteEventRuleBinding` | Clears the binding at an event-type and scope pair. |
| `ListEventHandlings` | Filters and paginates handling results using captured event, policy, Rack, and exact-source associations. |
| `ListEventTasks` | Filters and paginates event-triggered Tasks; includes terminal Tasks unless `active_only` is true. |

The examples use gRPC reflection, which requires `--dev-mode`, and assume Flow
is listening without TLS on `localhost:50051`. Replace the example UUIDs with
IDs from your environment.

### Inspect the built-in fallback

```bash
grpcurl -plaintext -d '{
  "eventType": "hardware.leak.detected",
  "rackId": {"id": "11111111-1111-4111-8111-111111111111"}
}' localhost:50051 v1.Flow/GetEffectiveEventRule
```

With no enabled persisted override, the response is the built-in leakage rule
and has `readOnly: true`.

List rules with optional event-type and enabled filters:

```bash
grpcurl -plaintext -d '{
  "eventType": "hardware.leak.detected",
  "pagination": {"offset": 0, "limit": 25}
}' localhost:50051 v1.Flow/ListEventRules
```

Filters apply before pagination. Results contain persisted rules first and
built-in rules second; each group is ordered by ascending rule UUID. If
pagination is omitted, offset defaults to 0 and limit defaults to 100. The
response `total` is the matching count before pagination.

### Create a disabled custom rule

This no-op example exercises selection without a hardware side effect:

```bash
grpcurl -plaintext -d '{
  "name": "Leakage audit only",
  "description": "Exercise event selection without a hardware side effect.",
  "eventType": "hardware.leak.detected",
  "actions": [
    {
      "name": "record_critical_leak",
      "condition": {
        "severities": ["EVENT_RULE_SEVERITY_CRITICAL"]
      },
      "noop": {
        "reason": "End-to-end event-rule validation"
      }
    }
  ]
}' localhost:50051 v1.Flow/CreateEventRule
```

Save the returned rule UUID. The response has `enabled: false` and
`readOnly: false`.

A task-submission action uses this shape:

```json
{
  "name": "power_off_affected_components",
  "condition": {
    "severities": ["EVENT_RULE_SEVERITY_CRITICAL"]
  },
  "submitTask": {
    "targetStrategy": "EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS",
    "conflictStrategy": "EVENT_RULE_CONFLICT_STRATEGY_QUEUE",
    "description": "Leakage response",
    "operation": {
      "powerControl": {
        "operation": "POWER_CONTROL_OPERATION_FORCE_POWER_OFF"
      }
    }
  }
}
```

### Bind the rule

```bash
grpcurl -plaintext -d '{
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"},
  "scope": {
    "type": "EVENT_RULE_SCOPE_TYPE_RACK",
    "id": {"id": "11111111-1111-4111-8111-111111111111"}
  }
}' localhost:50051 v1.Flow/CreateEventRuleBinding
```

For a site binding, omit the ID:

```json
{
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"},
  "scope": {"type": "EVENT_RULE_SCOPE_TYPE_SITE"}
}
```

Creating a rack binding with an unknown rack UUID returns `InvalidArgument`.

### Enable and verify the rule

```bash
grpcurl -plaintext -d '{
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"},
  "enabled": true
}' localhost:50051 v1.Flow/SetEventRuleEnabled
```

Call `GetEffectiveEventRule` again with the bound rack or one of its component
IDs. Verify that the returned rule UUID is the custom rule UUID before enabling
a producer that can cause hardware side effects.

### Replace, disable, unbind, or delete

Rule definitions are immutable after creation. To change metadata, event type,
or actions, create a new rule and replace the affected bindings with
`PutEventRuleBinding`. Delete the old rule once it has no remaining bindings.
Existing events retain their captured policy and execution-plan snapshots.

To disable a rule without removing its bindings or canceling existing Tasks:

```bash
grpcurl -plaintext -d '{
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"},
  "enabled": false
}' localhost:50051 v1.Flow/SetEventRuleEnabled
```

Delete a rack binding by its resolution slot:

```bash
grpcurl -plaintext -d '{
  "eventType": "hardware.leak.detected",
  "scope": {
    "type": "EVENT_RULE_SCOPE_TYPE_RACK",
    "id": {"id": "11111111-1111-4111-8111-111111111111"}
  }
}' localhost:50051 v1.Flow/DeleteEventRuleBinding
```

A rule can be deleted only after all of its bindings have been removed or
replaced. Deletion does not rewrite or remove events and executions that already
snapshot that rule.

## Errors and concurrent mutations

| Code | Meaning |
|---|---|
| `InvalidArgument` | Malformed input, unsupported event type or action capability, invalid rule, or invalid rack scope. |
| `NotFound` | Rule, binding, or effective-rule target does not exist. |
| `AlreadyExists` | Policy deletion is blocked by remaining bindings, regardless of policy enabled state. |
| `FailedPrecondition` | Built-in mutation, binding conflict, or an unconfigured event-rule manager. |
| `Internal` | Unclassified storage or dependency failure. |

Enabled-state changes use last-write-wins semantics without an etag or revision
precondition. Enabling validates the immutable definition before the desired
state is applied atomically. Repeating the current state preserves `updated_at`.
Omitting `enabled` returns `InvalidArgument`; explicit `false` disables the rule.
Each binding replacement is atomic for its event-type and scope slot. Clients
that require serialized administration must coordinate it outside this API.
