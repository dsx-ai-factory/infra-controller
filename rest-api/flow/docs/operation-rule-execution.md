# Operation Rule Execution

Operator guide: [Operation Rules](../../../docs/operations/flow/operation-rules.md).

## Resolution and execution

The task manager resolves a rack association, then a global operation default, then the built-in fallback. The resolved definition travels in workflow input rather than being reloaded from the database during execution.

The parent executes stages in ascending order. `executeGenericStageParallel` launches one `GenericComponentStepWorkflow` per component-type step with targets and waits for the stage. Missing target types are skipped. A stage failure stops the task without rolling back earlier hardware actions.

Each child runs pre-operation actions, the main action, and post-operation actions in sequence. Its target contains all selected component IDs of that type. Action executors perform component batching and external activities. Cross-component verification receives the complete target map.

## Timeouts and retries

`buildActivityOptions` uses the step timeout as the activity start-to-close timeout, defaulting to 20 minutes. The step retry policy supplies activity retry defaults. Without it, defaults are three attempts, a one-second initial interval, twofold backoff, and a one-minute maximum interval. Individual action executors may override activity options.

`childWorkflowExecutionTimeout` derives a separate execution budget: a base of the step timeout (30 minutes when zero), multiplied by configured attempts, plus configured backoff, declared pre/post action timeouts, and a two-minute buffer. It does not simply use the step timeout as the whole child-workflow timeout.

See [workflow helpers](../internal/task/executor/temporalworkflow/workflow/helpers.go), [child orchestration](../internal/task/executor/temporalworkflow/workflow/genericcomponentstep.go), and [action executors](../internal/task/executor/temporalworkflow/workflow/actions.go) for the execution paths. The [action validator](../internal/task/operationrules/actions.go) defines accepted user-rule actions; internal executor registration alone does not make an action accepted by that validator.
