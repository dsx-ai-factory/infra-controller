# Flow Operations <Badge intent="info">v2.2</Badge> <Badge intent="launch" minimal>New</Badge>

Flow orchestrates rack operations through tasks. Use these guides to choose the execution rule, automate dispatch, or manage a phased firmware rollout.

| Guide | Use it to |
| --- | --- |
| [Operation Rules](operation-rules.md) | Define component ordering, actions, verification, timeouts, and retries. |
| [Task Schedules](task-schedules.md) | Run an operation on a persistent rack scope at an interval, cron time, or once. |
| [Operation Runs](operation-runs.md) | Select firmware targets and manage phased execution with concurrency limits and safety gates. |

A rule describes how an operation executes. A schedule describes when to submit tasks. An operation run tracks a selected rollout cohort and its phases. These are separate resources; pausing a schedule or run does not undo hardware actions already performed.

Flow's interfaces are documented in the [generated gRPC reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/grpc-api.md). The rules guide also covers the Flow CLI. These interfaces are distinct from the REST API reference.

Before operating hardware, review [Flow component-manager configuration](../../configuration/flow-component-manager.md) and the [Flow architecture](../../architecture/flow.md). For firmware-specific prerequisites and target parameters, see [Rack and Tray Firmware Updates](../firmware-updates/rack-component-firmware.md).

## Inventory queries

NVLink Domain topology is nullable when unavailable. Set `includeComponents=true` to include
components; otherwise `components` is `null`. When requested but none are
known, the Domain's component list is empty.

Rack, Tray, and NVLink Domain list endpoints use deterministic pagination.
When `orderBy` is omitted, results are ordered by name ascending with an
immutable unique identifier as the tie-breaker. See
[Rack-Level Administration](../../manuals/rack_level_admin.md#rest-api)
for endpoint links.

## Task completion and recovery

Follow each returned task with [GET /task/{id}](api:GET/v2/org/:org/nico/task/:id),
using the same `siteId`. A task-creation response means orchestration was
accepted; it does not confirm a hardware result. Inspect `status`, `message`,
and available reports, then verify the device state before retrying a
failed or interrupted operation. A missing task returns HTTP 404, including
when [cancellation](api:POST/v2/org/:org/nico/task/:id/cancel) addresses a missing
ID.

Flow checks persisted pending and running tasks with saved execution IDs
at startup and on a one-minute background interval. This interval is not a
completion deadline: a sweep can take longer, and failed lookups are retried
on later sweeps. When Temporal has closed an execution without Flow's normal
final status update, Flow records its terminal outcome and releases the task's
scheduling occupancy. Waiting work can then advance under the usual conflict
rules.

| Temporal execution result | REST task status after reconciliation |
| --- | --- |
| Completed | `Succeeded` |
| Failed | `Failed` |
| Canceled, terminated, or timed out | `Terminated` |
| Saved run missing, with no live successor found | `Failed`; the message requests manual inspection |

For a known closed execution, the finish time comes from Temporal. For a
missing execution, it is the time Flow observed the absence, rather than a
hardware completion time. A live successor keeps the task active. Lookup
errors leave the task active for retry.

Reconciliation preserves the last activity reports; it cannot reconstruct
unreported device outcomes, roll back commands, or prove that hardware work
has stopped. Cancellation and emergency preemption likewise require checking
the actual device before resubmitting work. See
[leakage interruption and recovery](../leak-detection-handling.md#interrupted-operations-and-recovery).
