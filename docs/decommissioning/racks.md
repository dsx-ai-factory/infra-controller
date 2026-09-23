# Rack-Scale Decommissioning

## Overview

Rack-scale decommissioning allows an operator to remove an entire rack from NICo
inventory in a single API call. Flow drives the sequence as a Temporal workflow,
decommissioning component types in dependency order and polling Core for completion
at each stage.

The `DecommissionRack` RPC is currently gated behind an `Unimplemented` guard.
`DecommissionMachine` (compute) is already implemented in Core; the guard comes
off once `DecommissionSwitch` and `DecommissionPowerShelf` are also available.

---

## API

```protobuf
rpc DecommissionRack(DecommissionRackRequest) returns (SubmitTaskResponse);
```

**`DecommissionRackRequest`**

| Field | Type | Description |
|---|---|---|
| `target_spec` | `OperationTargetSpec` | Target racks (rack IDs). Component targets are rejected. |
| `description` | `string` | Optional task description. |
| `queue_options` | `QueueOptions` | Optional queue-conflict policy override. |
| `rule_id` | `UUID` | Optional override to use a specific operation rule instead of the default. |

While the `Unimplemented` guard is in place, the RPC returns immediately with
that error and creates no tasks. After the guard is removed, one independent
Flow task is created per rack. The call returns immediately with the task IDs;
progress is tracked via `ListTasks` / `GetTasksByIDs`.

---

## Workflow

The default decommission rule drives three sequential stages. A failure in any
stage aborts the task; earlier stages are not rolled back.

```text
Stage 1 — Compute
  MainOperation:  DecommissionControl   (instructs Core to begin decommissioning)
  PostOperation:  WaitDecommissioned    (polls until all compute nodes are terminal)

Stage 2 — NVSwitch
  MainOperation:  DecommissionControl
  PostOperation:  WaitDecommissioned

Stage 3 — PowerShelf
  MainOperation:  DecommissionControl
  PostOperation:  WaitDecommissioned
```

**Stage parameters (default rule)**

| Parameter | Value |
|---|---|
| Per-stage timeout | 4 hours |
| Poll interval | 30 seconds |
| `DecommissionControl` retries | 1 Temporal attempt; 5 minute activity timeout |
| `GetDecommissionStatus` retries | 1 Temporal attempt per poll; poll loop retries until stage timeout |
| Consecutive status-poll error budget | 5 minutes |
| Max parallel components | unlimited (0) |

---

## Poll Loop State Machine

`WaitDecommissioned` calls `GetDecommissionStatus` every 30 seconds and
classifies each component's Core state:

| Core state | Classification |
|---|---|
| `Decommissioning/Decommissioned` | Terminal success |
| `Decommissioned` | Terminal success (legacy fallback) |
| `Decommissioning/<anything else>` | In progress — keep polling |
| `Ready` | In progress — Core has accepted the request but the machine controller has not yet begun the transition |
| `Maintenance(<operation>)` | In progress — a pending maintenance operation has priority; decommissioning will resume when it completes |
| `NotFound` (absent from Core response) | Hard failure — fails closed rather than inferring completion from absence |
| Any other state | Hard failure — workflow aborts immediately |

**Failure budget**: consecutive `GetDecommissionStatus` errors are tracked by
elapsed time. If failures span more than 5 minutes continuously the poll loop
aborts rather than spinning until the 4-hour stage deadline.

**Deadline enforcement**: the poll sleep is capped to `min(PollInterval, remaining_deadline)`
so a large configured poll interval cannot push execution past the stage timeout.

---

## Conflict Rules

A decommission task uses a rack-level wildcard conflict rule: it blocks — and is
blocked by — every other active task type on the same rack, including power,
firmware, bring-up, and a concurrent decommission. The wildcard (`*` on the B
side) means any future task type is also automatically blocked without requiring
a rule update.

The coarse-grained schedule-conflict check (`HasScheduleConflict`) enforces the
same rule when evaluating task schedules before they fire.

---

## Proto Mirror

The `DecommissionRack` RPC is exposed to the REST API and site-workflow layers
via the Flow client proto mirror at `rest-api/proto/flow/`. The mirror is kept in
sync by running:

```sh
make -C rest-api flow-proto
```

This copies the canonical source from `rest-api/flow/proto/v1/flow.proto`, adds
the Go package option, and regenerates `proto/flow/gen/v1/` via `buf generate`.
Do not manually patch the generated files — the binary descriptor tables and
`msgTypes` slots must be regenerated together or marshaling will panic.

---

## Known Open Items

The following are tracked as follow-ups and are non-blocking while the
`Unimplemented` guard is in place.

1. **Remaining Core RPCs** — `DecommissionSwitch` and `DecommissionPowerShelf`
   are still stubs. The guard on `DecommissionRack` comes off once both land.

2. **Core idempotency** — A repeated `DecommissionControl` call for a host
   already in `Decommissioning/*` or `Decommissioning/Decommissioned` currently
   returns `FailedPrecondition` instead of succeeding as a no-op. This must be
   fixed before the guard is removed so that retries and re-runs are safe.

3. **State divergence recovery** — Flow can time out or fail while Core retains
   `decommission_requested`. There is currently no recovery path; an operator
   must re-run the task manually.

4. **Retry semantics** — `DecommissionControl` hardcodes `MaximumAttempts: 1`
   and `StartToCloseTimeout: 5m`, overriding the stage-level retry policy.
   Operator-authored custom rules cannot change this behaviour. The retry policy
   should be expressed at the rule level or the override should be documented.

5. **Partial-application recoverability** — `Manager.Decommission` returns on
   the first component error. Combined with fire-once retry, a failure partway
   through a stage leaves earlier components commanded with no reconciliation.
   The fix is to re-read Core state before issuing a command (already-decommissioned
   components are skipped), which also makes retries safe.

6. **Failure-budget sleep cap** — The poll sleep is capped by the remaining
   action deadline but not yet by the remaining failure-budget window. With
   `PollInterval > 5m` the 5-minute consecutive-failure budget can be delayed
   by one extra poll cycle. Non-blocking given the default interval is 30 seconds.
