# Flow Operations

Flow orchestrates rack operations through tasks. Use these guides to choose the execution rule, automate dispatch, or manage a phased firmware rollout.

| Guide | Use it to |
| --- | --- |
| [Operation Rules](operation-rules.md) | Define component ordering, actions, verification, timeouts, and retries. |
| [Task Schedules](task-schedules.md) | Run an operation on a persistent rack scope at an interval, cron time, or once. |
| [Operation Runs](operation-runs.md) | Select firmware targets and manage phased execution with concurrency limits and safety gates. |

A rule describes how an operation executes. A schedule describes when to submit tasks. An operation run tracks a selected rollout cohort and its phases. These are separate resources; pausing a schedule or run does not undo hardware actions already performed.

Flow's interfaces are documented in the [generated gRPC reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/grpc-api.md). The rules guide also covers the Flow CLI. These interfaces are distinct from the REST API reference.

Before operating hardware, review [Flow component-manager configuration](../../configuration/flow-component-manager.md) and the [Flow architecture](../../architecture/flow.md). For firmware-specific prerequisites and target parameters, see [Rack and Tray Firmware Updates](../firmware-updates/rack-component-firmware.md).
