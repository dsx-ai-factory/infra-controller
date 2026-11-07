# Temporal Workflow Guide

This guide explains how to add a new operation to the Temporal workflow executor in the Flow system.

## Table of Contents

- [Overview](#overview)
- [Architecture](#architecture)
- [Adding a New Operation](#adding-a-new-operation)
  - [Step 0: Define the Task Type and Operation Metadata](#step-0-define-the-task-type-and-operation-metadata)
  - [Step 1: Define Activity Methods](#step-1-define-activity-methods)
  - [Step 2: Assign Names and Expose Activities](#step-2-assign-names-and-expose-activities)
  - [Step 3: Create the Workflow File](#step-3-create-the-workflow-file)
- [Complete Example](#complete-example)
- [Best Practices](#best-practices)
- [Workflow Patterns](#workflow-patterns)

## Overview

The Flow Temporal workflow system provides durable, retryable execution of long-running operations across distributed rack components. It has three layers:

1. **Manager**: Receives generic `ExecutionRequest`s, looks up the right workflow from the registry, and submits it to Temporal
2. **Workflows**: Orchestrate activities in sequence or parallel; each workflow self-registers with its `TaskType`
3. **Activities**: Execute actual work (API calls, status checks) against component managers

## Architecture

```text
ExecutionRequest{OperationType, OperationInfo}
         │
         ▼
┌─────────────────┐
│     Manager     │  Looks up WorkflowDescriptor from registry
│  manager.go     │  Calls client.ExecuteWorkflow(desc.WorkflowName, ...)
└────────┬────────┘
         │ ExecuteWorkflow
         ▼
┌─────────────────────────────────────┐
│           Workflow                  │  Defined per operation type
│  workflow/powercontrol.go, etc.     │  Calls executeRuleBasedOperation()
└────────┬──────────────┬────────────┘
         │ child wf     │ ExecuteActivity (name constant)
         ▼              ▼
┌────────────────┐  ┌──────────────────┐
│ GenericComponent│  │    Activities    │  Registered with explicit names
│  StepWorkflow  │  │ activity/*.go    │  via RegisterActivityWithOptions
└────────────────┘  └──────────────────┘
```

### Registry Pattern

**Workflow registry** (`workflow/registry.go`): uses `init()` self-registration. Task-dispatched workflow files call `registerTaskWorkflow[T, *T](taskType, name, fn)`, which derives the timeout and builds the `Unmarshal` closure automatically. Internal workflows (those without a `TaskType`) call `register(WorkflowDescriptor{...})` directly. Nothing needs to be added to a central list — the registry is populated automatically at startup.

**Activity registry** (`activity/registry.go`): uses per-instance dependency injection. `Build()` creates an `*Activities` value via `activity.New(updater, reportUpdater, registry)` and calls `acts.All()` to obtain the name → bound-method map, then registers each entry with the Temporal worker via `RegisterActivityWithOptions(fn, {Name: name})`. Status and report updaters are wired as separate parameters so each role is explicit at the call site. Because activities are methods on `*Activities`, each manager instance holds its own isolated copy of the dependencies — no shared mutable globals.

## Adding a New Operation

### Step 0: Define the Task Type and Operation Metadata

Before any activity or workflow code can compile, two prerequisites must exist.

**1. Register the task type** in `internal/task/common/common.go`:

```go
const (
    // ... existing constants ...
    TaskTypeHealthCheck TaskType = "health_check"
)

func TaskTypeFromString(s string) TaskType {
    switch s {
    // ... existing cases ...
    case TaskTypeHealthCheck.String():
        return TaskTypeHealthCheck
    // ...
    }
}
```

**2. Add operation options** (at minimum a timeout) in `internal/task/operations/options.go` or equivalent:

```go
func GetOperationOptions(tt taskcommon.TaskType) OperationOptions {
    switch tt {
    // ... existing cases ...
    case taskcommon.TaskTypeHealthCheck:
        return OperationOptions{Timeout: 10 * time.Minute}
    // ...
    }
}
```

**3. Define the task-info struct** in the `operations` package. Include a `Validate()` method — it is called by the `Unmarshal` closure that `registerTaskWorkflow` builds automatically:

```go
// HealthCheckTaskInfo carries the parameters for a health check operation.
type HealthCheckTaskInfo struct {
    // CheckType selects which checks to run (e.g. "full", "connectivity").
    CheckType string `json:"check_type"`
}

func (i *HealthCheckTaskInfo) Validate() error {
    if i.CheckType == "" {
        return fmt.Errorf("check_type is required")
    }
    return nil
}
```

**4. Define a component-manager capability and operation interface** when the
operation calls into component managers. Add the capability in
`componentmanager/capability`, advertise it from descriptors that support the
operation, and add the matching operation-specific interface in
`componentmanager`. The activity should check the capability first, then assert
the interface.

### Step 1: Define Activity Methods

Add methods to `*Activities` in `activity/activity.go`. Each method performs one unit of work and must be idempotent (Temporal may retry it).

```go
// HealthCheck checks the health status of a component.
func (a *Activities) HealthCheck(
    ctx context.Context,
    target common.Target,
) (operations.HealthStatus, error) {
    reader, err := a.requireHealthStatusReader(target)
    if err != nil {
        return operations.HealthStatusUnknown, err
    }

    return reader.HealthCheck(ctx, target)
}
```

**Key points:**

- Receiver is `*Activities`; use the typed capability helper (e.g.,
  `a.requireHealthStatusReader`, `a.requirePowerController`) to obtain the
  operation interface before invoking methods
- First non-receiver parameter is always `context.Context`
- Activities are retried automatically per the workflow's retry policy
- Validate inputs; return descriptive errors

### Step 2: Assign Names and Expose Activities

In `activity/activity.go`, add a name constant. In `activity/registry.go`, add the bound method to `All()`. These are the only two places the name string appears — everywhere else uses the constant.

```go
// activity/activity.go
const (
    // ... existing constants ...
    NameHealthCheck = "HealthCheck"
)
```

```go
// activity/registry.go — inside All()
func (a *Activities) All() map[string]any {
    return map[string]any{
        // ... existing entries ...
        NameHealthCheck: a.HealthCheck,
    }
}
```

`Build()` in `manager.go` calls `acts.All()` and registers each entry with the Temporal worker via `RegisterActivityWithOptions(fn, {Name: name})` — no manual update to `manager.go` is needed.

Use `NameHealthCheck` (not `"HealthCheck"`) in all `workflow.ExecuteActivity` call sites.

### Step 3: Create the Workflow File

Create `workflow/healthcheck.go`. The file must:

1. Call `registerTaskWorkflow[T, PT](...)` in `init()` to register the workflow
2. Implement the workflow function (prefer unexported)

```go
package workflow

import (
    "fmt"

    "go.temporal.io/sdk/workflow"

    taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
    "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// init registers the HealthCheck workflow descriptor with the package registry.
func init() {
    registerTaskWorkflow[operations.HealthCheckTaskInfo, *operations.HealthCheckTaskInfo](
        taskcommon.TaskTypeHealthCheck, "HealthCheck", healthCheck,
    )
}

// healthCheck orchestrates health checks across all target components.
func healthCheck(
    ctx workflow.Context,
    reqInfo task.ExecutionInfo,
    info *operations.HealthCheckTaskInfo,
) error {
    ctx = workflow.WithActivityOptions(ctx, healthCheckActivityOptions)

    if err := updateRunningTaskStatus(ctx, reqInfo.TaskID); err != nil {
        return err
    }

    typeToTargets := buildTargets(&reqInfo)

    err := executeRuleBasedOperation(
        ctx,
        typeToTargets,
        activity.NameHealthCheck,
        info,
        reqInfo.RuleDefinition,
    )

    return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err)
}
```

`registerTaskWorkflow` derives the `Timeout` from `operations.GetOperationOptions` and builds the `Unmarshal` closure via `unmarshalAndValidate`, so neither needs to be written by hand. `manager.Execute()` looks up the descriptor by `OperationType` and submits it to Temporal — no changes to `manager.go` are needed.

**Key points:**

- `registerTaskWorkflow` is the standard entry point for task-dispatched workflows; use `register()` directly only for internal workflows that have no `TaskType`
- `WorkflowName` is what Temporal uses internally; keep it stable — it need not match the Go function name
- `WorkflowFunc` can be unexported to decouple Go symbol renames from the stable Temporal name
- Use `activity.NameXxx` constants, not string literals, in `workflow.ExecuteActivity` calls

## Complete Example

A full end-to-end trace for the `HealthCheck` operation:

### 1. Operation info type (`operations` package)

```go
// HealthCheckTaskInfo carries the parameters for a health check operation.
type HealthCheckTaskInfo struct {
    // CheckType selects which checks to run (e.g. "full", "connectivity").
    CheckType string `json:"check_type"`
}

func (i *HealthCheckTaskInfo) Validate() error {
    if i.CheckType == "" {
        return fmt.Errorf("check_type is required")
    }
    return nil
}
```

### 2. Activity method, name constant, and registration

```go
// In activity/activity.go — add the name constant and method.

const (
    // ... existing constants ...
    NameHealthCheck = "HealthCheck"
)

func (a *Activities) HealthCheck(ctx context.Context, target common.Target) (operations.HealthStatus, error) {
    reader, err := a.requireHealthStatusReader(target)
    if err != nil {
        return operations.HealthStatusUnknown, err
    }

    return reader.HealthCheck(ctx, target)
}
```

```go
// In activity/registry.go — add the bound method to All().

func (a *Activities) All() map[string]any {
    return map[string]any{
        // ... existing entries ...
        NameHealthCheck: a.HealthCheck,
    }
}
```

### 3. Workflow file (`workflow/healthcheck.go`)

```go
func init() {
    registerTaskWorkflow[operations.HealthCheckTaskInfo, *operations.HealthCheckTaskInfo](
        taskcommon.TaskTypeHealthCheck, "HealthCheck", healthCheck,
    )
}

func healthCheck(ctx workflow.Context, reqInfo task.ExecutionInfo, info *operations.HealthCheckTaskInfo) error {
    ctx = workflow.WithActivityOptions(ctx, healthCheckActivityOptions)

    // ... orchestration logic ...
    if err := updateRunningTaskStatus(ctx, reqInfo.TaskID); err != nil {
        return err
    }

    err := executeRuleBasedOperation(
        ctx,
        buildTargets(&reqInfo),
        activity.NameHealthCheck,
        info,
        reqInfo.RuleDefinition,
    )
    return updateFinishedTaskStatus(ctx, reqInfo.TaskID, err)
}
```

### 4. Dispatching from the caller

The caller constructs an `ExecutionRequest` and calls `executor.Execute()`. No operation-specific code is needed in the manager or executor layers.

```go
req := taskdef.ExecutionRequest{
    Info: taskdef.ExecutionInfo{
        TaskID:         task.ID,
        Components:     components,
        RuleDefinition: ruleDef,
        OperationType:  taskcommon.TaskTypeHealthCheck,
        OperationInfo:  task.Operation.Info, // json.RawMessage
    },
    Async: true,
}
resp, err := executor.Execute(ctx, &req)
```

## Best Practices

### Activity names

- Define one `NameXxx` constant per activity in `activity/activity.go`
- Always use the constant in `workflow.ExecuteActivity` calls — never write the string inline
- The constant is the single source of truth; `RegisterActivityWithOptions` and all call sites use it

### Workflow registration

- Each workflow file owns its own `init()` — no central list to maintain
- Use `registerTaskWorkflow[T, *T](taskType, name, fn)` for task-dispatched workflows; it derives `Timeout` from `GetOperationOptions` and builds the `Unmarshal` + `Validate` closure automatically
- Use `register(WorkflowDescriptor{...})` directly only for internal workflows that have no `TaskType` (e.g. `genericComponentStepWorkflow`)
- `WorkflowName` is written once and never needs to match the Go function name; `registerTaskWorkflow` panics at startup if `TaskType` is zero or invalid, and `register` panics on any other misconfiguration

### Workflow determinism

- Workflows must be deterministic: no random values, no direct I/O, no `time.Now()` (use `workflow.Now()`)
- All non-deterministic work — API calls, status checks, sleeps — must happen inside activities
- Use `workflow.Sleep()`, not `time.Sleep()`

### Rule-based execution

For operations that fan out across component types, use `executeRuleBasedOperation()`. It drives execution through the `RuleDefinition` attached to the task:

- Stages run sequentially
- Steps within a stage run in parallel via `genericComponentStepWorkflow` child workflows
- Each step can have pre/post actions and a configurable `max_parallel` batch size

### Error handling

- Wrap errors with context (which component or stage failed)
- Always call `updateFinishedTaskStatus()` — even on the error path — so the task record is updated
- Retry policies live in the workflow's `workflow.ActivityOptions` variable or in the per-step `RetryPolicy` field of the rule definition — not scattered through workflow code

## Workflow Patterns

### Direct activity call (single component type)

```go
ctx = workflow.WithActivityOptions(ctx, activityOpts)
if err := workflow.ExecuteActivity(ctx, activity.NameHealthCheck, target).Get(ctx, nil); err != nil {
    return fmt.Errorf("health check failed: %w", err)
}
```

### Parallel activities with result collection

```go
futures := make([]workflow.Future, len(targets))
for i, target := range targets {
    futures[i] = workflow.ExecuteActivity(ctx, activity.NameHealthCheck, target)
}
for i, f := range futures {
    if err := f.Get(ctx, nil); err != nil {
        return fmt.Errorf("target %s failed: %w", targets[i].String(), err)
    }
}
```

### Polling loop

```go
deadline := workflow.Now(ctx).Add(timeout)
for {
    if workflow.Now(ctx).After(deadline) {
        return fmt.Errorf("timed out after %v", timeout)
    }
    if err := workflow.Sleep(ctx, pollInterval); err != nil {
        return err
    }
    var result activity.SomeStatusResult
    if err := workflow.ExecuteActivity(ctx, activity.NameGetSomeStatus, target).Get(ctx, &result); err != nil {
        continue // transient error, keep polling
    }
    if result.Done {
        return nil
    }
}
```

### Rule-based fan-out (recommended for multi-component operations)

```go
err := executeRuleBasedOperation(
    ctx,
    typeToTargets,          // map[ComponentType]Target
    activity.NameMyActivity, // legacy fallback name for steps without MainOperation
    operationInfo,
    reqInfo.RuleDefinition,
)
```

This drives the entire operation through the `RuleDefinition` stages and steps, handling parallelism and batching automatically via `genericComponentStepWorkflow`.

## References

- [Temporal Documentation](https://docs.temporal.io/)
- [Temporal Go SDK](https://github.com/temporalio/sdk-go)
- Key files in this package:
  - `activity/activity.go` — `*Activities` methods, name constants
  - `activity/registry.go` — `Activities` struct, `New`, `All` (per-instance dependency injection)
  - `workflow/registry.go` — workflow registry (`WorkflowDescriptor`, `registerTaskWorkflow`, `unmarshalAndValidate`, `register`, `Get`, `GetAllWorkflows`)
  - `workflow/genericcomponentstep.go` — `genericComponentStepWorkflow`, `nameGenericComponentStepWorkflow`
  - `workflow/helpers.go` — `executeRuleBasedOperation`, `buildTargets`, batching helpers
  - `workflow/actions.go` — pre/post action executors (`actionExecutorRegistry`)
  - `manager/manager.go` — `Build` (worker setup), `Execute` (workflow dispatch)
