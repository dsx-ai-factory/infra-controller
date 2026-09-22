# Event Rules Usage Guide

This guide explains how to configure and operate Flow event rules through the
gRPC API. For processing, persistence, deduplication, and claim-recovery
details, see [Event-Rule Architecture](event-rule-architecture.md). For exact
protobuf definitions, see the [gRPC API reference](grpc-api.md).

## Supported event type

| Event type | Producer | Built-in response |
|---|---|---|
| `hardware.leak.detected` | NICo leakage polling for compute and NVSwitch components | Force-power-off affected components |

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
| `COMPONENT` | The component that produced the event; invalid for a rack event. |
| `RACK` | All currently known components in the event component's owning rack or in the event rack. |
| `AFFECTED_COMPONENTS` | The targets chosen by the resolver for the event type. Without an event-specific resolver, this selects the event component or all currently known components in the event rack. |

For `hardware.leak.detected`, `AFFECTED_COMPONENTS` selects the leaking
component and components in the same rack with valid, strictly lower slot IDs.
A rack event selects all currently known components in the rack. See
[Target resolution](event-rule-architecture.md#target-resolution) for resolver
selection, failure behavior, and the limitations of this slot-based model.

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

See the [`nico-flow` chart documentation](../../../helm/charts/nico-flow/README.md)
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
