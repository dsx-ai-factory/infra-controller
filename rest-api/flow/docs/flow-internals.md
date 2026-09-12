# Flow Implementation Reference

For service responsibilities and dependencies, see [NICo Flow](../../../docs/architecture/flow.md). Operator contracts live in [Flow Operations](../../../docs/operations/flow/overview.md).

## Layers

| Layer | Entry points | Responsibility |
| --- | --- | --- |
| Service | [Service wiring](../internal/service/service.go), [RPC handlers](../internal/service/server_impl.go) | Construct managers and dispatchers, expose gRPC, and manage startup and shutdown. |
| Conversion | [Protobuf converters](../internal/converter/protobuf) | Translate and validate wire representations at the service boundary. |
| Inventory | [Inventory manager](../internal/inventory/manager/manager.go) | Manage rack and component inventory. |
| Task execution | [Task packages](../internal/task) | Resolve operation targets, persist rack tasks, and dispatch execution. |
| Rules | [Rule resolver](../internal/task/operationrules/resolver.go), [action validation](../internal/task/operationrules/actions.go) | Resolve database associations/defaults and built-in rules; validate custom definitions. |
| Temporal | [Workflow package](../internal/task/executor/temporalworkflow/workflow), [activity package](../internal/task/executor/temporalworkflow/activity) | Orchestrate durable work and call component managers. |
| Component managers | [Component manager architecture](component-manager-architecture.md) | Resolve implementations and provider dependencies. |
| Operation runs | [Manager](../internal/operationrun/manager/manager.go), [planner](../internal/operationrun/manager/planner/planner.go), [dispatcher](../internal/operationrun/manager/dispatcher/dispatcher.go) | Materialize targets once, persist lifecycle changes, and dispatch phases. |
| Task schedules | [Task schedule internals](task-schedule-internals.md) | Persist scopes, claim due schedules, submit tasks, and advance timing. |
| System jobs | [Scheduler architecture](scheduler-architecture.md) | Schedule internal jobs such as inventory synchronization. |

## Planning and persistence

Operation-run creation calls the planner and saves the run plus all materialized targets in one store transaction. The planner resolves candidate scope, applies exclusions and selection, orders targets, assigns phases, and freezes component execution sets. Later phases read the saved targets rather than re-planning from inventory.

The operation-run manager owns manual lifecycle changes. The dispatcher owns reconciliation, safety-gate evaluation, target claims, and submission. Refer to [manual controls](../internal/operationrun/manager/manual_controls.go) and [dispatcher implementation](../internal/operationrun/manager/dispatcher) when changing pause, resume, phase advance, cancellation, or recovery behavior.

Task schedules use a separate dispatcher. The internal job scheduler is a third mechanism; its overlap policies and lifecycle do not define the user-facing schedule API.

Database definitions belong to [migrations](../internal/db/migrations) and [models](../internal/db/model). Use those sources when changing storage rather than maintaining a second SQL schema in this guide. See [rule versioning](operation-rules-versioning.md) for rule-format development notes and [rule execution](operation-rule-execution.md) for activity and workflow boundaries.

## Interfaces and configuration

The [Flow protobuf](../proto/v1/flow.proto) is the API source. [Generated Markdown](grpc-api.md) and [generated HTML](grpc-api.html) remain owned by the Flow Makefile's `gen-doc` target.

Use [Flow Component Managers](../../../docs/configuration/flow-component-manager.md) for configuration precedence and defaults. Local service startup and build instructions remain in the [Flow README](../README.md).
