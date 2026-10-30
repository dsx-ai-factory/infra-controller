# Event Rules Usage Guide

This guide explains how to configure and operate Flow event rules through the
gRPC API. For processing, persistence, deduplication, and claim-recovery
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
2. Create a persisted rule. It is always created disabled.
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
- persisted rules have `read_only = false` and are created with
  `enabled = false`.

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

The REST leakage value `LeakingSource` maps to gRPC `SOURCE` (number 1).
`LeakingRack` maps to `RACK` (number 2), selecting all known components in the
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

### REST leakage policy requests

`GET /nico/leakage/policy?siteId=<uuid>` lists the effective default once,
followed by enabled explicit Rack overrides. Disabled overrides and unbound
rules are excluded; the REST API does not expose enable/disable controls or an
`enabled` field. A policy bound to several Racks appears once for each Rack.
Each row includes `siteId`, `scope` (`Default` or `Rack`), nullable `rackId`,
`source` (`BuiltIn` or `Override`), and the nested `policy`. A Rack binding
without a public inventory identifier has `rackId: null` and remains in the
list. The list does not repeat inherited defaults for every Rack.

The list uses `pageNumber` (default 1) and `pageSize` (default 20, maximum 100),
with pagination metadata in `X-Pagination`. Its only ordering is `SCOPE_ASC`:
default first, then public Rack ID ascending, missing identifiers last, with
canonical Rack UUID as a tie-breaker. Out-of-range pages return `[]` with the
full matching total.

`GET /nico/leakage/policy/default?siteId=<uuid>` retrieves the effective default:
the enabled Site override, or the built-in policy when no enabled override exists.
`PUT /nico/leakage/policy/default/override` sets the Site override.
`DELETE /nico/leakage/policy/default/override?siteId=<uuid>` removes that override
and restores the built-in default; deleting an absent override returns 204.
Rack policy GET, PUT, and DELETE remain at `/nico/rack/{rackId}/leakage/policy`.
Rack GET resolves the enabled Rack override, then the enabled Site override,
then the built-in policy.

Both the default override and Rack policy PUT endpoints require `siteId` in the JSON body.
It selects the Site for authorization and routing. The Rack endpoint retains
`rackId` in the URL path. PUT requests require exactly `siteId`, `action`,
`targetStrategy`, and `onConflict`; query parameters are not accepted.

```json
{
  "siteId": "550e8400-e29b-41d4-a716-446655440000",
  "action": "ForcePowerOff",
  "targetStrategy": "LeakingSource",
  "onConflict": "Queue"
}
```

For `NoAction`, supply the Site UUID and set both `targetStrategy` and
`onConflict` to `null`. Policy GET and DELETE requests have no JSON body and
continue to require query `siteId`.

### REST leakage handling history

Each REST handling record includes the source, event summary, captured policy,
`handlingStatus`, nullable diagnostic `reason`, curated whole-handling `message`,
Tasks with their IDs and current statuses, and `handledAt`. Flow computes the
result from the captured actions, their executions, and their Tasks; REST only
maps the public representation. It exposes no execution object, attempt count,
or retry schedule. An empty `tasks` array does not imply successful handling;
the status and diagnostic explain whether work is starting, retrying, skipped,
failed, unknown, or intentionally absent.

The event's `detectedAt` is the
source observation time captured when Flow first accepted the occurrence; it
can precede `handledAt` and does not represent the actual leak start time.
Duplicate observations preserve this timestamp. It is `null` for older records
or sources without an observation timestamp, rather than inferred from
`handledAt`.

Event and execution UUIDs and polling observation counts remain internal to
Flow. Task IDs identify Tasks clients can retrieve. REST pagination uses
`pageNumber` (default 1) and `pageSize` (default 1, maximum 100). The body is an
array, with pagination metadata in `X-Pagination`: `pageNumber`, `pageSize`,
`total`, and `orderBy`. The only supported ordering is `HANDLED_AT_DESC`, with
event UUID descending breaking equal-time ties. Empty or out-of-range pages
return `[]` with the total matching count. Page numbers must be between 1 and
21474836, matching the shared REST pagination bounds. The cursor and limit query
parameters are not supported. Clients must not treat `detectedAt` as a unique
record identifier.

Flow's gRPC handling request uses nonnegative `offset` and `limit` (zero defaults
to one, maximum 100). Its response includes the total matching history count
before pagination. Ordering is applied in PostgreSQL before offset and limit.

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

REST maps Flow's `NoAction`, `Running`, and `Completed` to `None`,
`ShuttingDown`, and `Down` for leakage. These describe handling progress, not
the source's current leak or power state. Per-task statuses match the Task API:
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
racks, but one rule cannot mix site and rack bindings.

Binding APIs use `(event_type, scope)` for get and delete. The binding UUID is
returned as data but is not the deletion key. There is no binding update API;
delete the occupied scope before creating its replacement. Deleting a
persisted rule deletes all of its bindings atomically.

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
| `CreateEventRule` | Creates one disabled persisted rule. |
| `GetEventRule` | Gets a persisted or built-in rule by UUID. |
| `GetEffectiveEventRule` | Resolves the effective rule for an existing rack or component. |
| `ListEventRules` | Filters and paginates persisted and built-in rules. |
| `UpdateEventRule` | Replaces either metadata or the complete action list. |
| `EnableEventRule` / `DisableEventRule` | Changes persisted-rule participation in selection. |
| `DeleteEventRule` | Deletes a persisted rule and all of its bindings. |
| `CreateEventRuleBinding` | Occupies one site or rack selection scope. |
| `GetEventRuleBinding` | Gets the binding at an event-type and scope pair. |
| `DeleteEventRuleBinding` | Clears the binding at an event-type and scope pair. |

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
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"}
}' localhost:50051 v1.Flow/EnableEventRule
```

Call `GetEffectiveEventRule` again with the bound rack or one of its component
IDs. Verify that the returned rule UUID is the custom rule UUID before enabling
a producer that can cause hardware side effects.

### Update, disable, unbind, or delete

`UpdateEventRule` changes exactly one section per call:

```bash
grpcurl -plaintext -d '{
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"},
  "metadata": {
    "name": "Leakage audit",
    "description": "Updated description."
  }
}' localhost:50051 v1.Flow/UpdateEventRule
```

A metadata update replaces both the name and description. An actions update
replaces the complete action list. Changes affect only events planned after the
update; existing events retain their policy and execution-plan snapshots.

Disable a persisted rule without removing its binding:

```bash
grpcurl -plaintext -d '{
  "ruleId": {"id": "22222222-2222-4222-8222-222222222222"}
}' localhost:50051 v1.Flow/DisableEventRule
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

Deleting the rule removes all of its bindings. It does not rewrite or remove
events and executions that already snapshot that rule.

## Errors and concurrent mutations

| Code | Meaning |
|---|---|
| `InvalidArgument` | Malformed input, unsupported event type or action capability, invalid rule, or invalid rack scope. |
| `NotFound` | Rule, binding, or effective-rule target does not exist. |
| `FailedPrecondition` | Built-in mutation, binding conflict, or an unconfigured event-rule manager. |
| `Internal` | Unclassified storage or dependency failure. |

Rule mutations use last-write-wins semantics. The API has no etag or revision
precondition. Enabling validates a loaded rule snapshot and atomically applies
the desired state, but a concurrent valid definition update can win before or
after it. Clients that require serialized administration must coordinate it
outside this API.
