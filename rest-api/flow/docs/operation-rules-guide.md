# Operation Rules Guide

Operation rules define how Flow executes power control, firmware, ingestion, and
bring-up operations. Each rule specifies a sequence of steps that determine
component ordering, parallelism, verification, and retry behavior.

## Table of Contents

- [Concepts](#concepts)
- [Rule Schema](#rule-schema)
- [Actions Reference](#actions-reference)
- [Examples](#examples)
- [Temporal Execution Model](#temporal-execution-model)
- [CLI Usage](#cli-usage)
- [Reference YAML](#reference-yaml)

---

## Concepts

### Rules and Operations

Each rule is bound to a single **operation** (e.g., `power_on`, `power_off`).
When an operation is triggered, Flow resolves the applicable rule for the target
rack and executes it.

### Steps and Stages

A rule contains **steps**. Each step targets one component type and belongs to
a numbered **stage**. Execution proceeds stage by stage in ascending order.
Within a stage, all steps run in parallel.

```text
Stage 1: [powershelf step]
Stage 2: [nvswitch step]
Stage 3: [compute step]        ← stages are sequential
         [nvswitch step]      ← steps within a stage are parallel
```

### Action Sequences

Each step defines three optional action lists:

- `pre_operation` — runs before the main operation (e.g., Sleep to settle)
- `main_operation` — the primary work (PowerControl, FirmwareControl, or a
  verification action when used as the entire step's purpose)
- `post_operation` — runs after the main operation (e.g., verify status)

All three phases execute inside a single child workflow per component. The
step-level `timeout` applies to the entire sequence (pre + main + post).

---

## Rule Schema

### Top-level fields

Rules are submitted as JSON via the `--rule-file` flag or embedded in a YAML
batch file.

```json
{
  "version": "v1",
  "steps": [ ... ]
}
```

| Field     | Type     | Required | Description                        |
|-----------|----------|----------|------------------------------------|
| `version` | string   | no       | Schema version. Defaults to `"v1"` |
| `steps`   | array    | yes      | One or more step definitions       |

### Step fields

| Field           | Type     | Required | Description |
|-----------------|----------|----------|-------------|
| `component_type`| string   | yes      | Component this step targets: `"compute"`, `"nvswitch"`, `"powershelf"` |
| `stage`         | integer  | yes      | Execution order. Steps with the same stage run in parallel. Must be ≥ 1 |
| `max_parallel`  | integer  | yes      | Max concurrent components. `0` = unlimited, `1` = sequential |
| `timeout`       | duration | no       | Timeout for the entire child workflow (pre + main + post). E.g. `"10m"` |
| `retry`         | object   | no       | Retry policy for the child workflow |
| `pre_operation` | array    | no       | Actions to run before `main_operation` |
| `main_operation`| object   | yes      | The primary action |
| `post_operation`| array    | no       | Actions to run after `main_operation` |

### Retry policy fields

| Field                | Type    | Required | Description |
|----------------------|---------|----------|-------------|
| `max_attempts`       | integer | yes      | Total attempts including the first. Must be ≥ 1 |
| `initial_interval`   | duration| yes      | Wait before first retry. E.g. `"5s"` |
| `backoff_coefficient`| float   | yes      | Multiplier for each subsequent interval. Must be ≥ 1.0 |
| `max_interval`       | duration| no       | Cap on retry interval. E.g. `"1m"` |

### Duration format

All duration fields accept Go duration strings: `"5s"`, `"30s"`, `"2m"`,
`"1m30s"`, `"10m"`, `"1h"`.

---

## Actions Reference

### PowerControl

Executes a power operation (on/off/restart) for the component.

When used from the power workflow, the operation is inherited from the task
context — no parameters required. When used cross-workflow (e.g., firmware
power recycle, bring-up), specify `operation` explicitly:

```yaml
# Within a power workflow (inherits from task):
main_operation:
  name: PowerControl

# Cross-workflow usage (explicit operation):
main_operation:
  name: PowerControl
  parameters:
    operation: "force_power_off"
```

| Field     | Required | Description |
|-----------|----------|-------------|
| `timeout` | no       | Overrides step timeout for this action only |

| Parameter   | Required | Description |
|-------------|----------|-------------|
| `operation` | no*      | Power operation code. Required when used outside a power workflow. Valid values: `power_on`, `force_power_on`, `power_off`, `force_power_off`, `restart`, `force_restart`, `warm_reset`, `cold_reset` |

---

### FirmwareControl

Starts a firmware update and polls for completion (async start + poll pattern).
Calls `FirmwareControl` to initiate, then repeatedly calls
`GetFirmwareStatus` until all components complete or the poll timeout
expires.

```yaml
main_operation:
  name: FirmwareControl
  parameters:
    poll_interval: 2m    # time between status polls (default: 2m)
    poll_timeout: 30m    # max wait for completion (default: 30m)
```

| Field     | Required | Description |
|-----------|----------|-------------|
| `timeout` | no       | Overrides step timeout for this action only |

| Parameter       | Required | Description |
|-----------------|----------|-------------|
| `poll_interval` | no       | Time between status polls (default `2m`) |
| `poll_timeout`  | no       | Max time to wait for completion (default `30m`) |

---

### BringUpControl

Opens the power-on gate for target components. No parameters required.

```yaml
main_operation:
  name: BringUpControl
```

---

### WaitBringUp

Polls `GetBringUpStatus` until all components reach the `MachineCreated` state
(ingestion complete). Used as a post-operation after `BringUpControl`.

```yaml
post_operation:
  - name: WaitBringUp
    timeout: 15m
    poll_interval: 30s
```

| Field           | Required | Description |
|-----------------|----------|-------------|
| `timeout`       | yes      | Max time to wait for bring-up |
| `poll_interval` | yes      | Time between polls |

---

### VerifyPowerStatus

Polls the component until its power status matches `expected_status`. Typically
used in `post_operation` to confirm the result of `PowerControl`.

```json
{
  "name": "VerifyPowerStatus",
  "timeout": "30s",
  "poll_interval": "5s",
  "parameters": {
    "expected_status": "on"
  }
}
```

| Field           | Required | Description |
|-----------------|----------|-------------|
| `timeout`       | yes      | Maximum time to wait for status to match |
| `poll_interval` | yes      | How often to check status |
| `expected_status` (param) | yes | `"on"` or `"off"` |

When used as `main_operation`, the step performs only verification (no power
command is sent). This is the pattern for forceful operation final-verification
stages.

---

### VerifyReachability

Polls until all components of the specified types in the rack become reachable
over the network. Used after powering on a powershelf to confirm downstream
components have booted, or before bring-up to wait for all PMCs.

By default, a component type is considered reachable when the `GetPowerStatus`
API call succeeds. With `require_all: true`, every individual component within
the type must respond (i.e., the returned status map must contain all target
component IDs).

```yaml
# Basic reachability (API call succeeds):
- name: VerifyReachability
  timeout: 3m
  poll_interval: 10s
  parameters:
    component_types: ["compute", "nvswitch"]

# Strict mode (every individual component must respond):
- name: VerifyReachability
  timeout: 10m
  poll_interval: 30s
  parameters:
    component_types: ["powershelf"]
    require_all: true
```

| Field              | Required | Description |
|--------------------|----------|-------------|
| `timeout`          | yes      | Maximum time to wait |
| `poll_interval`    | yes      | How often to probe |

| Parameter          | Required | Description |
|--------------------|----------|-------------|
| `component_types`  | yes      | Array of component type strings to check |
| `require_all`      | no       | When `true`, every individual component must respond (default `false`) |

---

### Sleep

Pauses execution for a fixed duration. Implemented as a durable workflow timer
(survives worker restarts). Useful for hardware settle time.

```json
{
  "name": "Sleep",
  "parameters": {
    "duration": "30s"
  }
}
```

| Field        | Required | Description |
|--------------|----------|-------------|
| `duration` (param) | yes | How long to sleep. E.g. `"30s"`, `"2m"` |

---

### GetPowerStatus

Queries the current power status of components and returns a status map.

```json
{
  "name": "GetPowerStatus",
  "timeout": "30s"
}
```

| Field     | Required | Description |
|-----------|----------|-------------|
| `timeout` | yes      | Maximum time for the query |

---

### InjectExpectation

Registers expected component configurations with their respective component
manager services. Used as the initial ingestion stage in full bring-up rules,
and as the sole action in ingestion-only rules.

The `IngestRack` gRPC API is a convenience wrapper that triggers a BringUp
workflow with an ingestion-only rule (all component types run
`InjectExpectation` in parallel).

```yaml
main_operation:
  name: InjectExpectation
```

No parameters or timeout overrides required — the action uses the step-level
timeout and the component `Target` to call the `InjectExpectation` activity.

---

## Examples

### Graceful power on

Powers components in dependency order (powershelf → nvswitch → compute) and
verifies status at each stage before proceeding.

```json
{
  "version": "v1",
  "steps": [
    {
      "component_type": "powershelf",
      "stage": 1,
      "max_parallel": 1,
      "timeout": "10m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "5s",
        "backoff_coefficient": 2.0,
        "max_interval": "1m"
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        {
          "name": "VerifyPowerStatus",
          "timeout": "30s",
          "poll_interval": "5s",
          "parameters": { "expected_status": "on" }
        },
        {
          "name": "VerifyReachability",
          "timeout": "3m",
          "poll_interval": "10s",
          "parameters": { "component_types": ["compute", "nvswitch"] }
        },
        {
          "name": "Sleep",
          "parameters": { "duration": "30s" }
        }
      ]
    },
    {
      "component_type": "nvswitch",
      "stage": 2,
      "max_parallel": 4,
      "timeout": "15m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "5s",
        "backoff_coefficient": 2.0,
        "max_interval": "1m"
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        {
          "name": "VerifyPowerStatus",
          "timeout": "30s",
          "poll_interval": "5s",
          "parameters": { "expected_status": "on" }
        }
      ]
    },
    {
      "component_type": "compute",
      "stage": 3,
      "max_parallel": 8,
      "timeout": "20m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "5s",
        "backoff_coefficient": 2.0,
        "max_interval": "1m"
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        {
          "name": "VerifyPowerStatus",
          "timeout": "30s",
          "poll_interval": "5s",
          "parameters": { "expected_status": "on" }
        }
      ]
    }
  ]
}
```

---

### Graceful power off

Reverse dependency order (compute → nvswitch → powershelf). A `Sleep` in the
powershelf `pre_operation` allows downstream components to finish shutting down
before cutting power.

```json
{
  "version": "v1",
  "steps": [
    {
      "component_type": "compute",
      "stage": 1,
      "max_parallel": 8,
      "timeout": "20m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "5s",
        "backoff_coefficient": 2.0
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        {
          "name": "VerifyPowerStatus",
          "timeout": "30s",
          "poll_interval": "5s",
          "parameters": { "expected_status": "off" }
        }
      ]
    },
    {
      "component_type": "nvswitch",
      "stage": 2,
      "max_parallel": 4,
      "timeout": "15m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "5s",
        "backoff_coefficient": 2.0
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        {
          "name": "VerifyPowerStatus",
          "timeout": "30s",
          "poll_interval": "5s",
          "parameters": { "expected_status": "off" }
        }
      ]
    },
    {
      "component_type": "powershelf",
      "stage": 3,
      "max_parallel": 1,
      "timeout": "10m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "5s",
        "backoff_coefficient": 2.0
      },
      "pre_operation": [
        {
          "name": "Sleep",
          "parameters": { "duration": "30s" }
        }
      ],
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        {
          "name": "VerifyPowerStatus",
          "timeout": "30s",
          "poll_interval": "5s",
          "parameters": { "expected_status": "off" }
        }
      ]
    }
  ]
}
```

---

### Forceful power on

Skips per-stage verification for maximum speed. All power commands are issued
first; a dedicated final stage (4) verifies all component types simultaneously.

```json
{
  "version": "v1",
  "steps": [
    {
      "component_type": "powershelf",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "1s",
        "backoff_coefficient": 2.0
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        { "name": "Sleep", "parameters": { "duration": "5s" } }
      ]
    },
    {
      "component_type": "nvswitch",
      "stage": 2,
      "max_parallel": 0,
      "timeout": "15m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "1s",
        "backoff_coefficient": 2.0
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        { "name": "Sleep", "parameters": { "duration": "5s" } }
      ]
    },
    {
      "component_type": "compute",
      "stage": 3,
      "max_parallel": 0,
      "timeout": "20m",
      "retry": {
        "max_attempts": 3,
        "initial_interval": "1s",
        "backoff_coefficient": 2.0
      },
      "main_operation": { "name": "PowerControl" },
      "post_operation": [
        { "name": "Sleep", "parameters": { "duration": "5s" } }
      ]
    },
    {
      "component_type": "powershelf",
      "stage": 4,
      "max_parallel": 0,
      "timeout": "2m",
      "retry": {
        "max_attempts": 2,
        "initial_interval": "5s",
        "backoff_coefficient": 1.5
      },
      "main_operation": {
        "name": "VerifyPowerStatus",
        "timeout": "1m",
        "poll_interval": "5s",
        "parameters": { "expected_status": "on" }
      }
    },
    {
      "component_type": "nvswitch",
      "stage": 4,
      "max_parallel": 0,
      "timeout": "2m",
      "retry": {
        "max_attempts": 2,
        "initial_interval": "5s",
        "backoff_coefficient": 1.5
      },
      "main_operation": {
        "name": "VerifyPowerStatus",
        "timeout": "1m",
        "poll_interval": "5s",
        "parameters": { "expected_status": "on" }
      }
    },
    {
      "component_type": "compute",
      "stage": 4,
      "max_parallel": 0,
      "timeout": "2m",
      "retry": {
        "max_attempts": 2,
        "initial_interval": "5s",
        "backoff_coefficient": 1.5
      },
      "main_operation": {
        "name": "VerifyPowerStatus",
        "timeout": "1m",
        "poll_interval": "5s",
        "parameters": { "expected_status": "on" }
      }
    }
  ]
}
```

### Ingestion only

Registers components with their respective component manager services without performing power or firmware
operations. All component types are ingested in parallel in a single stage.
The `IngestRack` gRPC API triggers this rule automatically.

```json
{
  "version": "v1",
  "steps": [
    {
      "component_type": "powershelf",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "main_operation": { "name": "InjectExpectation" }
    },
    {
      "component_type": "compute",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "main_operation": { "name": "InjectExpectation" }
    },
    {
      "component_type": "nvswitch",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "main_operation": { "name": "InjectExpectation" }
    }
  ]
}
```

### Full bring-up (with ingestion)

A complete bring-up sequence that first ingests components, then powers on
powershelves, and finally brings up compute nodes. The `BringUpRack` gRPC API
uses this as its default rule.

```json
{
  "version": "v1",
  "steps": [
    {
      "component_type": "powershelf",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "main_operation": { "name": "InjectExpectation" }
    },
    {
      "component_type": "compute",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "main_operation": { "name": "InjectExpectation" }
    },
    {
      "component_type": "nvswitch",
      "stage": 1,
      "max_parallel": 0,
      "timeout": "10m",
      "main_operation": { "name": "InjectExpectation" }
    },
    {
      "component_type": "powershelf",
      "stage": 2,
      "max_parallel": 0,
      "timeout": "20m",
      "pre_operation": [
        {
          "name": "VerifyReachability",
          "timeout": "10m",
          "poll_interval": "30s",
          "parameters": { "component_types": ["powershelf"], "require_all": true }
        }
      ],
      "main_operation": {
        "name": "PowerControl",
        "parameters": { "operation": "power_on" }
      },
      "post_operation": [
        { "name": "VerifyPowerStatus", "timeout": "5m", "poll_interval": "10s", "parameters": { "expected_status": "on" } }
      ]
    },
    {
      "component_type": "compute",
      "stage": 3,
      "max_parallel": 0,
      "timeout": "30m",
      "pre_operation": [
        { "name": "BringUpControl" },
        { "name": "WaitBringUp", "timeout": "15m", "poll_interval": "30s" },
        { "name": "VerifyPowerStatus", "timeout": "10m", "poll_interval": "15s", "parameters": { "expected_status": "on" } }
      ],
      "main_operation": {
        "name": "PowerControl",
        "parameters": { "operation": "force_restart" }
      },
      "post_operation": [
        { "name": "Sleep", "parameters": { "duration": "30s" } },
        { "name": "VerifyPowerStatus", "timeout": "10m", "poll_interval": "15s", "parameters": { "expected_status": "on" } }
      ]
    }
  ]
}
```

---

## Temporal Execution Model

This section describes what actually happens inside the Temporal executor when
a rule is applied to a task.

### 1. Rule resolution

Before any workflow starts, the task manager resolves the applicable rule for
the operation and rack using this priority order:

```text
1. Explicit rule ID           (ruleId on the request)
2. Rack-specific association  (rack_rule_associations table for this rack)
3. Global default rule        (is_default = true for this operation)
4. Hardcoded fallback         (built into the binary)
```

Rule resolution does not widen the request target. For a component-scoped
request, Flow runs only steps whose `component_type` occurs in the selected
components; other steps remain `skipped` in the task report. The resolved rule
must have at least one applicable step. If it has none, Flow returns
`FAILED_PRECONDITION` and does not fall through to a lower-priority rule. This
same check runs when a waiting task is promoted, using the component UUIDs and
types persisted on the task, so promotion cannot expand the original scope.

The resolved `RuleDefinition` is embedded in the `ExecutionInfo` passed to the
parent workflow. The workflow never queries the database. Flow records the ID
of a resolved database rule on the task as `applied_rule_id`; hardcoded fallback
rules have no database ID.

### 2. Parent workflow — sequential stages

`PowerControl` (or `FirmwareControl`) is the parent Temporal workflow. It does
pure orchestration:

```text
for each stage in ascending stage number:
    executeGenericStageParallel(stage.steps)   ← waits before advancing
```

If any stage fails, the workflow stops and the task is marked failed. Stages do
not roll back.

The parent workflow has no retry policy of its own (`MaxAttempts = 1`). Retries
are configured at the child workflow (step) level.

The parent workflow's execution timeout is auto-calculated from the sum of each
stage's maximum step timeout, plus a 10 % overhead buffer. You do not set it
directly.

### 3. Stage execution — parallel child workflows

Within a stage, each step spawns one `GenericComponentStepWorkflow` child
workflow. All child workflows in the stage are launched simultaneously and the
parent waits for all of them to finish before advancing.

A stage with no step applicable to the task target is recorded as `skipped` and
launches no child workflow. As a defensive check, the parent workflow fails if
the entire rule has no applicable step, even though the task manager normally
rejects that condition before scheduling Temporal.

```text
Stage N:
  ┌─────────────────────────┐  ┌─────────────────────────┐
  │ child: powershelf step  │  │ child: nvswitch step   │  ← in parallel
  └─────────────────────────┘  └─────────────────────────┘
           both must complete before Stage N+1 begins
```

Steps whose component type is not present in the task target are recorded as
`skipped` and do not launch a child workflow.

The child workflow's `WorkflowExecutionTimeout` is set to the step's `timeout`
field (defaulting to 30 minutes if unset). This timeout covers the entire
pre + main + post sequence.

### 4. Child workflow — action sequence

`GenericComponentStepWorkflow` runs the three action phases in order:

```text
pre_operation actions  (sequential)
       ↓
main_operation action
       ↓
post_operation actions (sequential)
```

The step's `retry` policy applies to the **entire child workflow**. If any
action fails and retries are exhausted, the child workflow fails, which fails
the stage, which fails the parent workflow.

Activity options (timeout, retry) for individual Temporal activities are derived
from the step configuration via `buildActivityOptions`. If the step has no
`retry` block, a default of 3 attempts with 1 s initial interval and 2× backoff
is used.

### 5. Action dispatch

Each action name is looked up in a registry that maps it to an executor
function:

| Action | Executor | Temporal primitive |
|--------|----------|--------------------|
| `Sleep` | `executeSleepAction` | `workflow.Sleep()` — durable timer |
| `PowerControl` | `executePowerControlAction` | `workflow.ExecuteActivity("PowerControl")` |
| `FirmwareControl` | `executeFirmwareControlAction` | `FirmwareControl` + poll `GetFirmwareStatus` |
| `GetPowerStatus` | `executeGetPowerStatusAction` | `workflow.ExecuteActivity("GetPowerStatus")` |
| `VerifyPowerStatus` | `executeVerifyPowerStatusAction` | polling loop (see below) |
| `VerifyReachability` | `executeVerifyReachabilityAction` | polling loop (see below) |
| `BringUpControl` | `executeBringUpControlAction` | `workflow.ExecuteActivity("BringUpControl")` |
| `WaitBringUp` | `executeWaitBringUpAction` | polling loop on `GetBringUpStatus` |

`Sleep` is a workflow timer, not an activity. It survives worker restarts and
does not consume an activity slot.

### 6. Polling actions

`VerifyPowerStatus` and `VerifyReachability` are implemented as workflow-level
polling loops — not as single activities. Each iteration calls
`GetPowerStatus` (a Temporal activity) and then `workflow.Sleep` for
`poll_interval` before trying again. The loop exits when the condition is
satisfied or `timeout` is exceeded.

```text
loop:
    call GetPowerStatus activity
    if condition met → return success
    if elapsed > timeout → return error
    workflow.Sleep(poll_interval)
```

Because the sleep inside the loop is a durable workflow timer, the worker can
restart mid-poll without losing progress.

`VerifyReachability` uses the same pattern but checks multiple component types,
remembering which types have already become reachable across iterations.

### 7. Cross-component verification

`VerifyReachability` needs to check component types other than the one the
current step targets (e.g., a powershelf step checking whether compute and
nvswitch are reachable). The parent workflow passes the full
`map[ComponentType]Target` to every child workflow, which in turn passes it to
every action executor. `VerifyReachability` uses this map to probe the correct
targets regardless of the step's own component type.

### Execution flow diagram

```text
gRPC request
    │
    ▼
Task Manager ── resolves rule ──► RuleDefinition embedded in ExecutionInfo
    │
    ▼
PowerControl workflow (parent)
    │
    ├─ Stage 1 ──────────────────────────────────────────┐
    │   ├─ child: GenericComponentStepWorkflow (powershelf)│
    │   │     pre_operation → main_operation → post_op    │ parallel
    │   └─ child: GenericComponentStepWorkflow (nvswitch) │
    │         pre_operation → main_operation → post_op    │
    │                                                    ◄┘ (wait all)
    │
    ├─ Stage 2 ──────────────────────────────────────────┐
    │   └─ child: GenericComponentStepWorkflow (compute)  │
    │         pre_operation → main_operation → post_op   ◄┘
    │
    ▼
UpdateTaskStatus (completed / failed)
```

---

## CLI Usage

### Create a single rule

```bash
flow rule create \
  --name "Graceful Power On" \
  --description "Power-on with verification" \
  --operation-type power_control \
  --operation power_on \
  --rule-file my-rule.json \
  --is-default
```

### Load rules from a YAML batch file

```bash
# Create (skip rules that already exist by name)
flow rule create --from-yaml examples/operation-rules-example.yaml

# Create or overwrite existing rules
flow rule create --from-yaml examples/operation-rules-example.yaml --overwrite

# Validate without writing to the database
flow rule create --from-yaml examples/operation-rules-example.yaml --dry-run
```

### Manage rules

```bash
# List all rules
flow rule list

# Set a rule as the default for its operation
flow rule set-default --id <rule-id>

# Associate a rule with a specific rack
flow rule associate --rack-id R1 --rule-id <rule-id>
```

### YAML batch file format

```yaml
version: v1

rules:
  - name: "My Power On Rule"
    description: "..."
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: powershelf
        stage: 1
        max_parallel: 1
        timeout: 10m
        main_operation:
          name: PowerControl
        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"
```

---

## Reference YAML

The file `examples/operation-rules-example.yaml` is the canonical loadable reference
covering all built-in rule patterns. Load it with:

```bash
flow rule create --from-yaml examples/operation-rules-example.yaml
```

```yaml
version: v1

rules:
  # ===========================================
  # Power Control - Action-Based with Verification
  # ===========================================

  - name: "Graceful Power On with Verification"
    description: "Power-on with status verification and reachability checks"
    operation_type: power_control
    operation: power_on
    steps:
      # Stage 1: Power shelves first
      - component_type: powershelf
        stage: 1
        max_parallel: 1
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"

          # Wait for downstream components to become reachable
          - name: VerifyReachability
            timeout: 3m
            poll_interval: 10s
            parameters:
              component_types: ["compute", "nvswitch"]

          - name: Sleep
            parameters:
              duration: 30s

      # Stage 2: NVL switches (parallel batches)
      - component_type: nvswitch
        stage: 2
        max_parallel: 4
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"

          - name: Sleep
            parameters:
              duration: 15s

      # Stage 3: Compute nodes (larger parallel batches)
      - component_type: compute
        stage: 3
        max_parallel: 8
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"

  - name: "Graceful Power Off with Verification"
    description: "Power-off with status verification (reverse order)"
    operation_type: power_control
    operation: power_off
    steps:
      # Stage 1: Compute nodes first
      - component_type: compute
        stage: 1
        max_parallel: 8
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "off"

          - name: Sleep
            parameters:
              duration: 10s

      # Stage 2: NVL switches
      - component_type: nvswitch
        stage: 2
        max_parallel: 4
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "off"

          - name: Sleep
            parameters:
              duration: 5s

      # Stage 3: Power shelves last
      - component_type: powershelf
        stage: 3
        max_parallel: 1
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        # Allow time for downstream to fully shut down
        pre_operation:
          - name: Sleep
            parameters:
              duration: 30s

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "off"

  # ===========================================
  # Forceful Operations - Fast with Final Verification
  # ===========================================

  - name: "Force Power On with Final Verification"
    description: "Fast power-on, verify all components at the end"
    operation_type: power_control
    operation: force_power_on
    steps:
      # Stage 1: Power shelves (no verification)
      - component_type: powershelf
        stage: 1
        max_parallel: 0
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: Sleep
            parameters:
              duration: 30s

      # Stage 2: NVL switches (no verification)
      - component_type: nvswitch
        stage: 2
        max_parallel: 0
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: Sleep
            parameters:
              duration: 15s

      # Stage 3: Compute nodes (no verification)
      - component_type: compute
        stage: 3
        max_parallel: 0
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: Sleep
            parameters:
              duration: 10s

      # Stage 4: Final verification (all component types in parallel)
      - component_type: powershelf
        stage: 4
        max_parallel: 0
        timeout: 2m
        retry:
          max_attempts: 2
          initial_interval: 5s
          backoff_coefficient: 1.5

        main_operation:
          name: VerifyPowerStatus
          timeout: 1m
          poll_interval: 5s
          parameters:
            expected_status: "on"

      - component_type: nvswitch
        stage: 4
        max_parallel: 0
        timeout: 2m
        retry:
          max_attempts: 2
          initial_interval: 5s
          backoff_coefficient: 1.5

        main_operation:
          name: VerifyPowerStatus
          timeout: 1m
          poll_interval: 5s
          parameters:
            expected_status: "on"

      - component_type: compute
        stage: 4
        max_parallel: 0
        timeout: 2m
        retry:
          max_attempts: 2
          initial_interval: 5s
          backoff_coefficient: 1.5

        main_operation:
          name: VerifyPowerStatus
          timeout: 1m
          poll_interval: 5s
          parameters:
            expected_status: "on"

  - name: "Force Power Off with Final Verification"
    description: "Fast power-off, verify all components at the end"
    operation_type: power_control
    operation: force_power_off
    steps:
      # Stage 1: Compute nodes (no verification)
      - component_type: compute
        stage: 1
        max_parallel: 0
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: Sleep
            parameters:
              duration: 10s

      # Stage 2: NVL switches (no verification)
      - component_type: nvswitch
        stage: 2
        max_parallel: 0
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: Sleep
            parameters:
              duration: 5s

      # Stage 3: Power shelves (no verification)
      - component_type: powershelf
        stage: 3
        max_parallel: 0
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: Sleep
            parameters:
              duration: 10s

      # Stage 4: Final verification (all component types in parallel)
      - component_type: powershelf
        stage: 4
        max_parallel: 0
        timeout: 2m
        retry:
          max_attempts: 2
          initial_interval: 5s
          backoff_coefficient: 1.5

        main_operation:
          name: VerifyPowerStatus
          timeout: 1m
          poll_interval: 5s
          parameters:
            expected_status: "off"

      - component_type: nvswitch
        stage: 4
        max_parallel: 0
        timeout: 2m
        retry:
          max_attempts: 2
          initial_interval: 5s
          backoff_coefficient: 1.5

        main_operation:
          name: VerifyPowerStatus
          timeout: 1m
          poll_interval: 5s
          parameters:
            expected_status: "off"

      - component_type: compute
        stage: 4
        max_parallel: 0
        timeout: 2m
        retry:
          max_attempts: 2
          initial_interval: 5s
          backoff_coefficient: 1.5

        main_operation:
          name: VerifyPowerStatus
          timeout: 1m
          poll_interval: 5s
          parameters:
            expected_status: "off"

  # ===========================================
  # Restart Operations
  # ===========================================

  - name: "Graceful Restart with Verification"
    description: "Restart with verification at each stage"
    operation_type: power_control
    operation: restart
    steps:
      # === POWER OFF SEQUENCE (Stages 1-3) ===

      - component_type: compute
        stage: 1
        max_parallel: 8
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "off"

      - component_type: nvswitch
        stage: 2
        max_parallel: 4
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "off"

      - component_type: powershelf
        stage: 3
        max_parallel: 1
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "off"

          # Pause between off and on
          - name: Sleep
            parameters:
              duration: 10s

      # === POWER ON SEQUENCE (Stages 4-6) ===

      - component_type: powershelf
        stage: 4
        max_parallel: 1
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"

          - name: VerifyReachability
            timeout: 3m
            poll_interval: 10s
            parameters:
              component_types: ["compute", "nvswitch"]

          - name: Sleep
            parameters:
              duration: 30s

      - component_type: nvswitch
        stage: 5
        max_parallel: 4
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"

          - name: Sleep
            parameters:
              duration: 15s

      - component_type: compute
        stage: 6
        max_parallel: 8
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0

        main_operation:
          name: PowerControl

        post_operation:
          - name: VerifyPowerStatus
            timeout: 30s
            poll_interval: 5s
            parameters:
              expected_status: "on"

  # ===========================================
  # Firmware Control Operations
  # ===========================================
  # Stage 1: PowerShelf firmware
  # Stage 2: NVSwitch + Compute firmware (parallel)
  # Stage 3: Compute power recycle to activate new firmware

  - name: "Default Firmware Upgrade"
    description: "Three-stage firmware upgrade with power recycle"
    operation_type: firmware_control
    operation: upgrade
    steps:
      - component_type: powershelf
        stage: 1
        max_parallel: 0
        timeout: 30m
        retry:
          max_attempts: 2
          initial_interval: 30s
          backoff_coefficient: 1.5
        main_operation:
          name: FirmwareControl
          parameters:
            poll_interval: 2m
            poll_timeout: 30m

      - component_type: nvswitch
        stage: 2
        max_parallel: 0
        timeout: 30m
        retry:
          max_attempts: 2
          initial_interval: 30s
          backoff_coefficient: 1.5
        main_operation:
          name: FirmwareControl
          parameters:
            poll_interval: 2m
            poll_timeout: 30m

      - component_type: compute
        stage: 2
        max_parallel: 0
        timeout: 30m
        retry:
          max_attempts: 2
          initial_interval: 30s
          backoff_coefficient: 1.5
        main_operation:
          name: FirmwareControl
          parameters:
            poll_interval: 2m
            poll_timeout: 30m

      - component_type: compute
        stage: 3
        max_parallel: 0
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0
        pre_operation:
          - name: PowerControl
            parameters:
              operation: "force_power_off"
          - name: Sleep
            parameters:
              duration: 10s
        main_operation:
          name: PowerControl
          parameters:
            operation: "power_on"
        post_operation:
          - name: VerifyPowerStatus
            timeout: 5m
            poll_interval: 15s
            parameters:
              expected_status: "on"

  # ===========================================
  # Legacy Format (Backward Compatibility)
  # ===========================================

  - name: "Legacy Power On (No Actions)"
    description: "Old format using delay_after instead of actions"
    operation_type: power_control
    operation: power_on
    steps:
      - component_type: powershelf
        stage: 1
        max_parallel: 1
        delay_after: 30s
        timeout: 10m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

      - component_type: nvswitch
        stage: 2
        max_parallel: 4
        delay_after: 15s
        timeout: 15m
        retry:
          max_attempts: 3
          initial_interval: 5s
          backoff_coefficient: 2.0

      - component_type: compute
        stage: 3
        max_parallel: 8
        timeout: 20m
        retry:
          max_attempts: 3
          initial_interval: 1s
          backoff_coefficient: 2.0
```
