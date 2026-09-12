# Operation Run Guide

An operation run executes a firmware rollout over a frozen set of rack targets. Use it to limit concurrency, divide the rollout into phases, and stop further dispatch when safety gates trip. Flow persists the target plan at creation and starts dispatching it in the background.

## gRPC Contracts

These RPCs belong to the Flow gRPC service. See the [generated reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/grpc-api.md) for message definitions; this guide describes operational behavior.

### RPCs

The operation-run API adds the following RPCs:

```proto
CreateOperationRun
GetOperationRun
ListOperationRuns
ListOperationRunTargets
PauseOperationRun
ResumeOperationRun
AdvanceOperationRunPhase
CancelOperationRun
```

An operation run is a durable, phased rollout over selected rack execution
targets. The create request stores a reusable configuration containing target
selection, execution options, and an operation template.

Creation freezes the selected components, ordering, and phase assignments in one transaction. Later inventory changes do not re-plan that run. An empty target plan is rejected.

### Create And Read APIs

`CreateOperationRunRequest` requires a non-empty `name` and
`OperationRunConfiguration`; `description` is optional. `CreateOperationRunResponse` returns only the
generated run ID, keeping create lightweight and avoiding expensive target or
stats computation on the create path.

`GetOperationRunRequest` takes an ID and an `include_stats` flag. When
`include_stats` is false, the response returns the run summary plus
configuration. When true, Flow computes derived stats from
`operation_run_target` rows and includes `OperationRun.stats`.

`ListOperationRunsRequest` returns lightweight `OperationRunSummary` records,
not full configurations or target-derived stats. Filtering supports name query,
operation kind, status, and status reason. Status and reason are modeled
together as `OperationRunStateFilter`; each filter entry ANDs its populated
fields, and multiple entries OR together.

### Target Listing

`ListOperationRunTargetsRequest` lists materialized rack execution targets for
one run. It supports a target status filter, pagination, and phase scope.
`UNKNOWN` status means no status filter.

Phase scope can be:

- `CURRENT_PHASE`
- `COMPLETED_PHASES`
- `CURRENT_AND_COMPLETED_PHASES`

This lets callers inspect just the active phase, prior completed phases, or current and prior phases together. Future phases are excluded even though their targets are already persisted. Omission defaults to `CURRENT_PHASE`.

### Configuration

`OperationRunConfiguration` has three parts:

```proto
OperationRunSelector selector
OperationRunOptions options
OperationRunOperation operation
```

The selector supports percentage-based selection only.
`PercentageSelector.percentage` is required and valid from `1..100`. `seed` is
optional; if omitted, Flow generates and stores one so the chosen cohort is
deterministic and auditable. Selection rounds up: 1% of three candidate racks selects one rack.

`OperationRunOptions` includes:

- `max_concurrent_targets`: required positive maximum for concurrent target work.
- `safety_policy`: required, with at least one safety gate.
- `conflict_policy`: optional; defaults to retry with the durations listed below.
- `ordering_policy`: optional; defaults to random ordering with generated seed.
- `phase_policy`: optional; defaults to one phase containing all selected
  targets.

### Safety Gates

`OperationRunSafetyPolicy` contains repeated gates. Gates compose with OR
semantics: any tripped gate pauses the run.

Supported gates are:

- `OperationRunFailureRateGate`
- `OperationRunFailureCountGate`

Both support `CURRENT_PHASE` or `CUMULATIVE_RUN` scope. Omitting `scope` or sending
`UNKNOWN` defaults to `CURRENT_PHASE`. Failure-rate thresholds are integers from 1 through 100 percent; failure-count thresholds must be positive. Failure rate uses
`failed_targets / planned_targets` for the selected scope.

### Ordering, Conflict, And Phases

Ordering is a `oneof` policy. Random ordering is supported. Physical-location ordering has a protobuf policy branch but is rejected by the planner.

Conflict handling supports retry policy only. Omitting `conflict_policy` uses
retry; each omitted retry duration defaults independently as follows and is
stored in the effective configuration:

| Field | Default |
|---|---|
| `retry_timeout` | 1 hour |
| `initial_retry_delay` | 30 seconds |
| `max_retry_delay` | 5 minutes |

All three durations must be positive, and `max_retry_delay` must be at least
`initial_retry_delay`. Exceeding the retry timeout pauses the run with
`CONFLICT_RETRY_TIMEOUT`.

Phase policy supports equal phases, explicit percentage phases, and explicit
count phases. For count phases, configured counts define the early phases; the
final generated phase covers the remaining targets. Percentage phases must sum to 100. Phase allocation uses cumulative rounding; for seven targets, 50%/50% gives four and three. A phase that receives zero targets is rejected, so use fewer phases for small cohorts.

`OperationRunPhaseAdvancePolicy.auto_advance` controls phase boundaries and
defaults to false, including when `advance_policy` is omitted. When false, a
successful non-final phase pauses with `PHASE_GATE` and waits for
`AdvanceOperationRunPhase`. When true, the dispatcher advances automatically as long
as safety gates are not tripped.

### Target Scope

`OperationRunTargetScope` controls how candidate scope is built before applying
the selector.

The embedded operation `target_spec`, when present, is the inclusive base scope.
If `target_spec` is omitted, the planner uses the default qualified/applicable
scope. `default_scope_component_filter` can restrict that default scope to
specific component types or component UUIDs, such as "all compute trays in all
qualified racks"; that field is only valid when `target_spec` is omitted.
`exclude_operation_run_ids` then removes materialized targets from prior
operation runs from that base scope before selector application.

### Operation Template

`OperationRunOperation` is a `oneof`. The supported operation is
`upgrade_firmware`.

For normal `UpgradeFirmware`, `target_spec` means "run exactly on these
targets" and is required. Inside `CreateOperationRun`, the embedded
`target_spec` is optional and defines candidate scope before selector
application.

### State And Stats

Run state is modeled as `OperationRunState` with `OperationRunStatus` and
`OperationRunStatusReason`. Reasons distinguish operator pause, phase gate,
safety gate, and conflict retry timeout.

Stats are optional and derived, not returned unless requested.
`OperationRunStats` contains current phase stats and cumulative phase stats.
Each phase stat includes phase index, selected target count, and outcome
counts: completed, failed, terminated, skipped.

`OperationRunTarget` represents a materialized rack execution target. It tracks
rack ID, sequence index, phase index, optional child task ID, target status,
message, the resolved `components_by_type` execution set, and timestamps. The
target output uses resolved components grouped by type rather than a
`ComponentFilter`, because target rows are materialized execution state instead
of unresolved selection criteria.

## Operating a run

| Action | Behavior |
| --- | --- |
| `PauseOperationRun` | Pauses a pending or running run. An already-paused run retains its original pause reason. Terminal runs reject pause. Submitted hardware work is not rolled back or cancelled by pause. |
| `ResumeOperationRun` | Reopens a paused run that is not at a manual phase gate. It continues the current phase; the dispatcher evaluates safety gates before further work. A still-tripped gate can pause it again. |
| `AdvanceOperationRunPhase` | Opens the next phase only when paused at `PHASE_GATE` and the current phase is complete. Optional positive `expected_phase_index` must match the next phase's zero-based index; omission or zero skips that check. |
| `CancelOperationRun` | Durably cancels a non-terminal run, then attempts to cancel submitted current-phase tasks. Child cancellation is best effort within a shared five-second budget; a successful response does not prove every child stopped. Terminal runs are returned unchanged. |

Use `GetOperationRun` to inspect `state`, `status_message`, and configuration before deciding how to continue. Use `ListOperationRunTargets` to find the child `task_id` and per-target outcome. Cancelling a run does not undo firmware changes already made.

Invalid lifecycle transitions return `FailedPrecondition`. Unknown run IDs return `NotFound` on detail and lifecycle calls; malformed wire requests, empty target plans, and phases assigned zero targets return `InvalidArgument`. A missing operation-run manager returns `FailedPrecondition`.

## Reading progress

`GetOperationRun` omits stats unless `include_stats` is true. Computing stats reads targets across current and prior phases, so use the lightweight response when only run state is needed. `ListOperationRuns` returns summaries without configuration or target stats and orders by creation time descending.

`ListOperationRunTargets` orders by phase index and then sequence index, both ascending. Phase and sequence indexes are zero-based. Its `total` is the filtered count before pagination. An omitted or `UNKNOWN` target status applies no status filter. Every returned rack must have an external ID; otherwise the call fails with `FailedPrecondition`.

Current-phase stats describe the latest included phase; cumulative stats include current and prior phases. Future phases are excluded. `selected_targets` counts planned targets within that scope, not successful tasks. Inspect `outcome_counts` and the run state together: completion can include failures, and pending, blocked, claimed, or submitted targets are not terminal outcomes.

The dispatcher polls every 10 seconds by default. Submission, child task execution, and later reconciliation add latency; polling is not an execution or completion deadline. Persisted target plans and claims support dispatcher recovery after a service restart.
