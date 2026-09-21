# Flow Implementation

For service responsibilities and dependencies, see [NICo Flow](../../architecture/flow.md). Operator contracts live in [Flow Operations](../../operations/flow/overview.md).

## Layers

| Layer | Entry points | Responsibility |
| --- | --- | --- |
| Service | [Service wiring](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/service/service.go), [RPC handlers](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/service/server_impl.go) | Construct managers and dispatchers, expose gRPC, and manage startup and shutdown. |
| Conversion | [Protobuf converters](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/converter/protobuf) | Translate and validate wire representations at the service boundary. |
| Inventory | [Inventory manager](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/inventory/manager/manager.go) | Manage rack and component inventory. |
| Task execution | [Task packages](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task) | Resolve operation targets, persist rack tasks, and dispatch execution. |
| Rules | [Rule resolver](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/operationrules/resolver.go), [action validation](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/operationrules/actions.go) | Resolve database associations/defaults and built-in rules; validate custom definitions. |
| Temporal | [Workflow package](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/executor/temporalworkflow/workflow), [activity package](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/task/executor/temporalworkflow/activity) | Orchestrate durable work and call component managers. |
| Component managers | [Component manager architecture](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/component-manager-architecture.md) | Resolve implementations and provider dependencies. |
| Operation runs | [Manager](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/operationrun/manager/manager.go), [planner](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/operationrun/manager/planner/planner.go), [dispatcher](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/operationrun/manager/dispatcher/dispatcher.go) | Materialize targets once, persist lifecycle changes, and dispatch phases. |
| Task schedules | [Task schedule internals](task-schedule-internals.md) | Persist scopes, claim due schedules, submit tasks, and advance timing. |
| System jobs | [Scheduler architecture](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/scheduler-architecture.md) | Schedule internal jobs such as inventory synchronization. |

## Planning and persistence

Operation-run creation calls the planner and saves the run plus all materialized targets in one store transaction. The planner resolves candidate scope, applies exclusions and selection, orders targets, assigns phases, and freezes component execution sets. Later phases read the saved targets rather than re-planning from inventory.

The operation-run manager owns manual lifecycle changes. The dispatcher owns reconciliation, safety-gate evaluation, target claims, and submission. Refer to [manual controls](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/operationrun/manager/manual_controls.go) and [dispatcher implementation](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/operationrun/manager/dispatcher) when changing pause, resume, phase advance, cancellation, or recovery behavior.

Task schedules use a separate dispatcher. The internal job scheduler is a third mechanism; its overlap policies and lifecycle do not define the user-facing schedule API.

Database definitions are in [migrations](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/db/migrations) and [models](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/internal/db/model). See [rule versioning](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/operation-rules-versioning.md) for rule-format development notes and [rule execution](operation-rule-execution.md) for activity and workflow boundaries.

## Interfaces and configuration

The [Flow protobuf](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/proto/v1/flow.proto) is the API source. [Generated Markdown](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/grpc-api.md) and [generated HTML](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/docs/grpc-api.html) remain owned by the Flow Makefile's `gen-doc` target.

Use [Flow Component Managers](../../configuration/flow-component-manager.md) for configuration precedence and defaults. Local service startup and build instructions remain in the [Flow README](https://github.com/dsx-ai-factory/infra-controller/blob/main/rest-api/flow/README.md).
