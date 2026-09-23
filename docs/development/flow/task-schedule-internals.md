# Task Schedule Implementation

Operator guide: [Task Schedules](../../operations/flow/task-schedules.md).

## Dispatcher

The `Dispatcher` runs as a background goroutine started by the service at boot.
It claims due schedule rows with `SELECT … FOR UPDATE SKIP LOCKED` so other
instances skip locked rows.

It polls every 10 seconds and fetches up to 10 due schedules per poll by
default. A schedule with no eligible scopes advances its timing without
submitting tasks.

### Three-phase fire

Each schedule firing is split into three phases to avoid nesting transactions
(the task manager opens its own transaction when submitting a task):

1. **Locking phase (transaction):** Lock the row, fetch scopes, run the overlap check,
   advance `next_run_at` (so the row is no longer "due"), commit.
2. **Submission phase (outside transaction):** Call `SubmitTask` once per eligible scope.
3. **Writeback phase (new transaction):** Write back `last_task_id` on each scope row.

If Phase 2 fails for a scope, the scope is logged and skipped — other scopes
still fire. Phase 1 committing before Phase 2 means `next_run_at` has already
advanced; the schedule will not fire again for that same tick even if all
submissions fail. Failed task-ID writeback can leave the overlap check using
stale state; see [dispatch and failure behavior](../../operations/flow/task-schedules.md#dispatch-and-failure-behavior).

### Advancing next_run_at

Timing advances after a normal firing, an overlap skip, or an empty scope:

| `spec_type` | Result |
|---|---|
| `one-time` | Clear `next_run_at` and set `enabled = false`. |
| `interval` | Set `next_run_at` to dispatch time plus duration. |
| `cron` | Set `next_run_at` to the next cron time. |

### Operation template

The `operation_template` JSONB column stores the operation type, code, and
parameters needed to reconstruct an `operation.Request` at fire time. The
target is **not** stored in the template — it is resolved from the scope rows
at fire time. This means changing the scope (via scope management RPCs) takes
effect on the very next firing without modifying the operation template.

## Database schema

See the [schedule migration](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/db/migrations/20260410000000_create_task_schedule_table.up.sql) for table definitions, indexes, and foreign keys.

### Key columns

| Column | Notes |
|---|---|
| `task_schedule.name` | Unique across all schedules. Human-readable identifier. |
| `task_schedule.next_run_at` | `NULL` for disabled one-time schedules that have fired. The partial index makes the dispatcher's poll query efficient. |
| `task_schedule.enabled` | `false` = paused (will not fire). Set by `PauseTaskSchedule` or automatically after a one-time schedule fires. |
| `task_schedule_scope.component_filter` | `NULL` means all components. See [Component filter variants](../../operations/flow/task-schedules.md#component-filter-variants). |
| `task_schedule_scope.last_task_id` | The task submitted for this rack on the most recent firing. Used by the overlap check for the `skip` policy. `NULL` until the first firing. |
| `task_schedule_scope.schedule_id` FK | `ON DELETE CASCADE` — scopes are removed automatically when the schedule is deleted. |
