# Operation Rules Guide

User-defined operation rules configure power control and firmware operations. Each rule specifies a sequence of steps that determine
component ordering, parallelism, verification, and retry behavior.

## Table of Contents

- [Concepts](#concepts)
- [Rule Schema](#rule-schema)
- [Actions Reference](#actions-reference)
- [Examples](#examples)
- [Execution behavior](#execution-behavior)
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

Each step defines optional `pre_operation` and `post_operation` lists and one required `main_operation` action:

- `pre_operation` — runs before the main operation (e.g., Sleep to settle)
- `main_operation` — the primary work (PowerControl, FirmwareControl, or a
  verification action when used as the entire step's purpose)
- `post_operation` — runs after the main operation (e.g., verify status)

All three phases execute inside a single child workflow per component-type step. The step supplies activity timeout and retry defaults; Flow derives a separate child-workflow execution budget.

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
| `steps` | array | no | Omission or an empty array is accepted. Define steps explicitly for a custom sequence. |

### Step fields

| Field           | Type     | Required | Description |
|-----------------|----------|----------|-------------|
| `component_type`| string   | yes      | Component this step targets: `"compute"`, `"nvswitch"`, `"powershelf"` |
| `stage`         | integer  | yes      | Execution order. Steps with the same stage run in parallel. Must be ≥ 1 |
| `max_parallel`  | integer  | no      | Max concurrent components. Defaults to `0` (unlimited); `1` = sequential; negative values are rejected |
| `timeout`       | duration | no       | Base activity start-to-close timeout; omission or zero uses 20 minutes. Also contributes to the derived child-workflow budget. |
| `retry`         | object   | no       | Activity retry defaults; omission uses 3 attempts, 1s initial interval, twofold backoff, and 1m maximum interval |
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

The user-rule validator rejects `BringUpControl`, `WaitBringUp`, and `InjectExpectation`. Their presence in internal workflows does not make them accepted user-rule actions. The public operation-rule API exposes power-control and firmware-control operation types.

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

## Execution behavior

Flow selects a rack-specific rule association first, then a global default for the operation, then a built-in fallback. The selected rule is embedded in workflow input before execution starts.

Stages run in order. A failed stage stops the task; earlier stages are not rolled back. Steps for component types absent from the rack are skipped. Activity retries can repeat external calls. The child-workflow execution timeout includes the configured retry budget, declared pre/post action timeouts, and a scheduling buffer; it is not equal to the step timeout.

For Temporal workflow and activity details, see [Operation Rule Execution](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/operation-rule-execution.md).

## CLI Usage

The examples use a local development Flow service and run from `rest-api/flow/`. Configure the client connection and authentication for other environments before running mutating commands. `--dry-run` validates a batch without contacting the server.

Batch lookup matches operation type and operation code, not the rule name. `--overwrite` deletes the matching rule before creating its replacement; it is not an atomic update and can remove rack associations. Use it only after reviewing affected rules and bindings.

Creating with `--is-default` fails if a default already exists for that operation. To change the default, create a non-default rule and use `set-default`. Replace `<rule-id>` and `<rack-id>` below with existing UUIDs. Save the Graceful power on JSON example as `my-rule.json` for the single-rule command.

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
# Create (skip existing operation type/code pairs)
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
flow rule associate --rack-id <rack-id> --rule-id <rule-id>
```

A batch must contain every required operation for each included operation type. The small YAML below illustrates structure only; use the complete reference file for batch loading. Single-rule flags (`--name`, `--rule-file`, `--operation-type`, `--operation`, `--is-default`) cannot be combined with `--from-yaml`. `--dry-run` and `--overwrite` are batch options.

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

The [loadable rule examples](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/examples/operation-rules-example.yaml) are the canonical batch file. The CLI examples above assume the working directory is `rest-api/flow/` in a repository checkout.
