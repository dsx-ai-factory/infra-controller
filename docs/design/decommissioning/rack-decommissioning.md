# Rack-Scale Decommissioning with NICo Flow

## Status

Draft — aligned with the merged Flow rack decommission workflow and the Core
resource workflows tracked under
[#1969](https://github.com/NVIDIA/infra-controller/issues/1969).

## Summary

This document defines how an operator starts decommissioning for an entire rack
through NICo Flow. Flow creates one durable task for the rack and drives the
resource-specific NICo Core decommissioning APIs in dependency order:

1. managed hosts on compute trays;
1. managed switches; and
1. managed power shelves.

Every component in one stage must reach `Decommissioned` before Flow starts the
next stage. Flow does not perform hardware cleanup itself. NICo Core owns each
resource state machine and the cleanup defined by the
[managed host](./managed-host-decommissioning.md),
[managed switch](./managed-switch-decommissioning.md), and
[managed power shelf](./managed-power-shelf-decommissioning.md) designs.

Final deletion is not part of the rack task. The task leaves the rack and its
components in their retained terminal states for operator verification.

## Architecture

The operation follows the existing rack-administration service path:

```mermaid
flowchart LR
    Operator["Operator or HW Lifecycle API"] --> Flow["NICo Flow"]
    Flow --> Temporal["Temporal rack task"]
    Temporal --> Core["NICo Core APIs"]
    Core --> Host["Managed-host controller"]
    Core --> Switch["Switch controller"]
    Core --> Shelf["Power-shelf controller"]
```

NICo Flow owns rack target resolution, task conflicts, stage ordering, durable
execution, retries, cancellation, and progress reporting. NICo Core remains the
source of truth for hardware state, eligibility, credentials, and terminal
completion.

## Preconditions

Rack-scale decommissioning requires:

- NICo Flow and its Temporal namespace to be deployed and healthy;
- current Flow inventory for the target rack, including compute, NVSwitch, and
  power-shelf component associations;
- a NICo Core resource ID for every Flow component selected in the rack;
- no active conflicting Flow task for the rack; and
- all resource-specific credentials, expected-resource records, artifacts, and
  reset capabilities required by the three component workflows.

Once accepted, the rack task is exclusive with allocation and other rack
lifecycle operations. New allocation, maintenance, power, firmware, bring-up,
and decommissioning work for the rack must be rejected or queued until the task
finishes or is cancelled.

Core still enforces per-resource eligibility when Flow dispatches each start
request. A switch or power-shelf start fails if any managed host on that rack
is assigned to an instance.

## Flow API

```protobuf
rpc DecommissionRack(DecommissionRackRequest)
    returns (SubmitTaskResponse);

message DecommissionRackRequest {
  OperationTargetSpec target_spec = 1;
  string description = 2;
  optional QueueOptions queue_options = 3;
  optional UUID rule_id = 4;
}
```

`target_spec` must contain rack targets. As with other Flow rack operations, a
request may name more than one rack, but Flow creates and returns one
independent task ID per rack.

The operation adds:

- task type `decommission`;
- operation code `decommission`;
- Temporal workflow name `Decommission`; and
- an exclusive rack-task conflict entry against allocation, power, firmware,
  bring-up, maintenance, and another decommissioning task.

### Start an operation

In production, the operator starts the operation through the authenticated HW
Lifecycle API. That service resolves the site and forwards the rack target to
NICo Flow. The caller supplies a rack target by rack ID or rack name and an
optional description, queue policy, or rule override. Flow returns the rack
task ID in `SubmitTaskResponse`.

### Monitor the operation

Use the existing task APIs with the returned task ID. The task report includes
per-component progress while Core persists the detailed resource substate. The
task reaches `TASK_STATUS_COMPLETED` only after every selected resource is
`Decommissioned`.

## Target resolution and frozen plan

When Flow accepts the request, it resolves the rack to a concrete component set
and persists that set in the task before dispatching stage 1. The frozen plan
contains:

- every compute component and its canonical managed-host ID;
- every NVSwitch component and its canonical switch ID; and
- every power-shelf component and its canonical power-shelf ID.

For each planned resource, these states are valid when a stage begins:

- `Ready`: call the resource's start-decommissioning API;
- `Decommissioning/*` other than terminal: observe and wait for the existing
  operation;
- `Decommissioned` / `Decommissioning/Decommissioned`: count it as already
  complete; or
- any other state: fail the stage without advancing to the next component type.

This makes a resubmitted rack task converge after a previous partial run while
preserving the one-operation-per-resource invariant. Core's start RPC is the
atomic claim from `Ready` into decommissioning; Flow must not treat a bare
state read followed by an unconditional start as sufficient idempotency.

## Default decommissioning rule

The hardcoded default Flow rule has three sequential stages. Components within
one stage may run in parallel. `max_parallel: 0` means unlimited concurrency
within the stage; stages never overlap.

```yaml
name: Hardcoded Default Decommission
description: Rack decommission: compute first, then NVSwitch, then PowerShelf
operation_type: decommission
operation: decommission
steps:
  - component_type: compute
    stage: 1
    max_parallel: 0
    timeout: 4h
    main_operation:
      name: DecommissionControl
    post_operation:
      - name: WaitDecommissioned
        timeout: 4h
        poll_interval: 30s

  - component_type: nvswitch
    stage: 2
    max_parallel: 0
    timeout: 4h
    main_operation:
      name: DecommissionControl
    post_operation:
      - name: WaitDecommissioned
        timeout: 4h
        poll_interval: 30s

  - component_type: powershelf
    stage: 3
    max_parallel: 0
    timeout: 4h
    main_operation:
      name: DecommissionControl
    post_operation:
      - name: WaitDecommissioned
        timeout: 4h
        poll_interval: 30s
```

`DecommissionControl` dispatches by component type:

| Flow component type | NICo Core operation | Completion state |
| --- | --- | --- |
| `compute` | `DecommissionManagedHost` | Managed host reaches `Decommissioning/Decommissioned` |
| `nvswitch` | `DecommissionSwitch` | Switch reaches `Decommissioning/Decommissioned` |
| `powershelf` | `DecommissionPowerShelf` | Power shelf reaches `Decommissioning/Decommissioned` |

`WaitDecommissioned` polls NICo Core. States that begin with
`Decommissioning/` are in progress. The terminal `Decommissioned` value
completes the wait. A permanent status-read failure aborts within a bounded
consecutive-failure budget.

## Execution sequence

```mermaid
sequenceDiagram
    participant O as Operator
    participant F as NICo Flow
    participant C as NICo Core

    O->>F: DecommissionRack(rack)
    F->>C: Resolve inventory and begin stage 1

    par Every managed host
        F->>C: DecommissionManagedHost(host_id)
        F->>C: Poll until Decommissioned
    end

    par Every managed switch
        F->>C: DecommissionSwitch(switch_id)
        F->>C: Poll until Decommissioned
    end

    par Every managed power shelf
        F->>C: DecommissionPowerShelf(power_shelf_id)
        F->>C: Poll until Decommissioned
    end

    F-->>O: Task completed with report
```

The switch stage is never dispatched until every managed host has completed.
The power-shelf stage is never dispatched until every managed switch has
completed.

## Failure, retry, and cancellation

A failure stays in its current stage:

- a compute failure prevents all switch and power-shelf starts;
- a switch failure prevents all power-shelf starts; and
- a power-shelf failure leaves the task incomplete but does not undo completed
  hosts or switches.

Temporal retries transient activities according to the selected rule. A retry
re-reads Core state before acting: terminal `Decommissioned` is success,
in-progress `Decommissioning/*` is observed without submitting another start
request, and `Ready` receives the Core start request that atomically claims the
resource.

After retry exhaustion, an operator corrects the underlying resource problem
and submits `DecommissionRack` again. The new task freezes the rack membership
again and converges from persisted Core state; it does not repeat cleanup on
resources already in `Decommissioned`.

Cancelling the Flow task prevents new stages and stops Flow polling. It does
not roll back hardware changes or force a Core resource out of its persisted
decommissioning state. A Core controller may therefore finish work that Flow
started before cancellation. The operator must inspect Core state before
resubmitting or taking manual action.

## Completion and final deletion

Successful task completion means every planned managed host, switch, and power
shelf reached `Decommissioned` and remains protected from rediscovery by its
BMC suppressions.

The operator then chooses one of two follow-up paths:

- **Physical removal:** remove the rack's expected-resource records when the
  site should no longer ingest that hardware, then use the resource-specific
  final-deletion APIs.
- **Fresh ingestion:** leave the expected-resource records in place and use the
  resource-specific final-deletion APIs. Final deletion removes suppressions,
  making connected hardware eligible for discovery and ingestion again.

Deleting or purging the Flow inventory rack is a separate inventory operation.
`DeleteRack` and `PurgeRack` do not substitute for Core decommissioning or prove
that hardware cleanup succeeded.

## Verification plan

Unit and integration coverage must verify:

- one Flow task and one frozen component plan are created per rack;
- missing or cross-rack inventory fails before any start request;
- compute start requests fan out before any switch request;
- all compute resources must reach `Decommissioned` before stage 2 starts;
- all switches must reach `Decommissioned` before stage 3 starts;
- retries observe active or completed Core states without duplicating starts;
- a failed stage never dispatches a later component type;
- cancellation prevents later stages without claiming rollback;
- the task report exposes per-resource state and redacted failures;
- successful completion leaves expected-resource records intact; and
- Flow rack deletion is not treated as successful hardware decommissioning.

End-to-end qualification must cover empty component groups, multiple resources
per type, a partial failure in each stage, Flow and Temporal restarts, Core
restart during polling, task cancellation, resubmission after failure, and
multi-rack requests producing independent per-rack tasks.
