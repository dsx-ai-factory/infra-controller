# Operation Rule Execution <Badge intent="info">v2.2</Badge> <Badge intent="launch" minimal>New</Badge>

Operator guide: [Operation Rules](../../operations/flow/operation-rules.md).

## Resolution and execution

The task manager resolves an explicit `rule_id` first, then a rack association, then a global operation default, and finally the built-in fallback. Failure to load an explicit rule returns an error without trying lower-priority sources. The resolved definition travels in workflow input rather than being reloaded from the database during execution.

The parent executes stages in ascending order. `executeGenericStageParallel` launches one `GenericComponentStepWorkflow` per component-type step with targets and waits for the stage. Missing target types are skipped. A stage failure stops the task without rolling back earlier hardware actions.

Each child runs pre-operation actions, the main action, and post-operation actions in sequence. Its target contains all selected component IDs of that type. Action executors perform component batching and external activities. Cross-component verification receives the complete target map.

Only actions registered with `batchByMaxParallel` are partitioned. `Sleep` and `VerifyFirmwareConsistency` execute once with the full context; `VerifyReachability` also executes once but batches its status requests. `VerifyFirmwareConsistency` is an internal action, not an accepted user-rule action.

The `component-action-max-parallel` Temporal version marker preserves replay compatibility: histories predating the change retain unlimited dispatch, while new executions apply the configured `max_parallel`. Batches run sequentially within each action, not as separate child workflows.

## Timeouts and retries

`buildActivityOptions` uses the step timeout as the activity start-to-close timeout, defaulting to 20 minutes. The step retry policy supplies activity retry defaults. Without it, defaults are three attempts, a one-second initial interval, twofold backoff, and a one-minute maximum interval. Individual action executors may override activity options.

`childWorkflowExecutionTimeout` derives a separate execution budget:

- Main action: the step timeout (30 minutes when zero) times configured attempts, plus retry backoff, multiplied by the action's batch count.
- Pre/post actions: each declared action timeout times that action's batch count.
- Scheduling buffer: two minutes.

Batch counts are action-specific; step-wide actions count once. Retry backoff uses `max_interval`, falling back to `initial_interval`, for each retry. When `retry` is omitted, the calculation uses one attempt and no backoff, even though activities default to three attempts. With unlimited parallelism and all timeouts omitted, the child budget is 32 minutes versus 20 minutes per activity attempt, so the child deadline can cut off the default retries.

See [workflow helpers](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/executor/temporalworkflow/workflow/helpers.go), [child orchestration](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/executor/temporalworkflow/workflow/genericcomponentstep.go), and [action executors](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/executor/temporalworkflow/workflow/actions.go) for the execution paths. The [action validator](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/operationrules/actions.go) defines accepted user-rule actions; internal executor registration alone does not make an action accepted by that validator.
