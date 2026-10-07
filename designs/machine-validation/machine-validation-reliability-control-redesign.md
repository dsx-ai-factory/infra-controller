# Machine Validation Reliability and Control Redesign

## Software Design Document

## Revision History

| Version | Date | Modified By | Description |
| :---: | :---: | :---- | :---- |
| 0.1 | 2026-06-01 | Sunil Kumar | Initial draft |
|  |  |  |  |

# **1. Introduction**

This software design document describes a reliability and control redesign for Machine Validation in NICo. Machine Validation currently verifies host hardware by rebooting a managed host into Scout, selecting validation tests, executing them, and reporting final results back to the site controller. The current implementation works for successful flows, but failures in Scout, final result reporting, or completion handling can leave a machine stuck in validation with limited operator visibility or control.

The redesign makes validation runs durable and recoverable first, then adds observability and operator controls in later milestones. The first implementation slice introduces explicit run items, attempts, heartbeats, and reconciliation while preserving compatibility with the existing Machine Validation APIs during migration. Live log ingestion and richer operator controls are planned as follow-on slices.

## **1.1 Purpose**

The purpose of this document is to give NICo platform engineers, site operators, Scout developers, API/UI owners, and reviewers a shared design for improving Machine Validation reliability and operator control. The design focuses on:

1. Preventing machines from remaining in validation indefinitely after Scout or API failures.
2. Showing run state, active test state, heartbeat freshness, progress, and reconciliation outcomes.
3. Staging pause, resume, cancel, retry, and live-log workflows after the durable execution foundation is in place.
4. Preserving existing APIs and workflows while the new run model is introduced incrementally.

## **1.2 Definitions and Acronyms**

| Term/Acronym | Definition |
| :---- | :---- |
| NICo | NVIDIA bare-metal lifecycle management system. |
| SDD | Software Design Document. |
| API | Application Programming Interface. |
| REST | HTTP REST-based API layer used by external clients. |
| gRPC | Internal service API protocol used by NICo site-controller services. |
| Scout | Temporary host-side agent that runs during discovery and validation. |
| Machine Validation | Process that runs hardware and system validation tests before or after tenant use. |
| BOM/SKU Validation | Hardware inventory validation against the expected SKU. This is separate from Machine Validation. |
| Run | One Machine Validation request for one machine and context. |
| Run Item | One selected test inside a validation run. |
| Attempt | One execution attempt for one run item. Retries create additional attempts. |
| Heartbeat | Periodic liveness update from Scout for the run and active attempt. |
| Reconciler | API-side background process that marks stale runs or attempts terminal by policy. |
| Terminal State | Final run state such as `Succeeded`, `Failed`, `TimedOut`, `Canceled`, or `Aborted`. |

## **1.3 Scope**

This SDD covers the design for improving Machine Validation reliability, observability, and operator controls. The first implementation slice is `M0` through `M2` and is scoped to reliability and recovery. Later milestones are included for continuity, but they are not required for the first reviewable implementation.

1. Durable run, run-item, and attempt records.
2. API-owned run state transitions.
3. Scout heartbeats and event reporting.
4. API reconciliation for stale or failed validation work.
5. Compatibility with existing run/result APIs during migration.
6. Live/recent log ingestion and bounded log retention in `M3`.
7. Operator controls for pause, resume, cancel, and retry in `M4`.
8. Admin UI, CLI, REST, and gRPC surfaces required to view and control runs as each milestone exposes them.

This SDD does not cover:

1. Replacing SKU validation or BOM validation.
2. Defining the individual hardware validation tools themselves.
3. Redesigning the test catalog, test naming, test authoring, verification lifecycle, or platform profile ownership.
4. Parallel test execution in the first implementation.
5. Active-test pause/checkpointing in the first implementation.
6. Long-term log archival as the first storage target. Full archival can use object storage or debug bundles later.

### **1.3.1 Assumptions, Constraints, Dependencies**

Assumptions:

1. Existing `GetMachineValidationRuns`, `GetMachineValidationResults`, `PersistValidationResult`, `UpdateMachineValidationRun`, and `MachineValidationCompleted` callers must continue to work during migration.
2. Scout execution remains sequential in the first implementation.
3. Pause initially means "do not start the next test"; active-test interruption is cancellation.
4. Retry attempts must preserve earlier attempt results and logs.
5. Machine Validation can be blocking for discovery and cleanup flows, but the design should also allow non-blocking validation contexts.

Constraints:

1. The existing machine-controller currently relies on `last_machine_validation_time` and failure details as a compatibility signal.
2. Existing REST workflows may time out while validation continues; new run resources must keep the run queryable by ID.
3. In `M3`, log ingestion must be bounded by chunk size, retention, and per-run limits.
4. Plugins and their site administrators are responsible for ensuring emitted
   logs do not contain secrets or external configuration values. Retained logs
   remain access-controlled and bounded by the configured size and retention
   policy.

Dependencies:

1. Database migrations for run items, attempts, and any additional run-summary fields required on the existing `machine_validation` table.
2. Scout event and heartbeat reporting for run and active-attempt progress.
3. API reconciliation worker for stale runs and attempts.
4. Compatibility projection for existing run/result readers.
5. Later milestones depend on a streaming/cancelable Scout command runner, UI/CLI log views, control APIs, and REST/OpenAPI updates after the site-controller gRPC model stabilizes.

## **1.4 Requirements Summary**

### **1.4.1 Functional Requirements**

| ID | Milestone | Requirement |
| :--- | :--- | :--- |
| FR-1 | `M1` | The API must create or load a validation run before Scout starts execution and persist selected run items before or as Scout starts each test. Full API-side pre-materialization of the execution plan is optional for the first slice. |
| FR-2 | `M1` | Each selected test must have a durable run item with state, attempt count, start time, end time, timeout, current attempt, and failure reason. |
| FR-3 | `M1` | Each attempt must record command, args, execution context, container image if applicable, exit code, timeout status, start time, end time, and failure classification. |
| FR-4 | `M2` | Scout must heartbeat at run level and active-attempt level while work is active. |
| FR-5 | `M2` | The API must reconcile stale runs and attempts using heartbeat and timeout policy, then unblock the machine state with a terminal status. |
| FR-6 | `M3` | Scout should stream or batch stdout/stderr log chunks while a command is running. |
| FR-7 | `M3` | The admin UI and CLI should show live logs, final logs, progress, current test, attempt history, and stale heartbeat warnings. |
| FR-8 | `M4a` | Operators with appropriate privileges should be able to pause between tests, resume paused runs, and cancel before the next test starts. |
| FR-9 | `M4b` | Active cancellation should terminate the running test process group or container gracefully, then force kill after a configured grace period. |
| FR-10 | `M4c` | Retry policy should be defined per selected test or run item, with max attempts, retryable exit codes, retry-on-timeout, and backoff. |
| FR-11 | `M0-M2` | Existing `GetMachineValidationRuns`, `GetMachineValidationResults`, and on-demand start workflows must keep working during migration. |
| FR-12 | `M4` | All control operations must be audited with actor, reason, timestamp, and outcome. |
| FR-13 | `M3-M4` | REST APIs should expose redesigned run, log, and control workflows after the site-controller gRPC model stabilizes. |

### **1.4.2 Non-Functional Requirements**

| ID | Requirement |
| :--- | :--- |
| NFR-1 | A single lost Scout process or failed final completion call must not leave a machine in validation indefinitely. |
| NFR-2 | In `M3`, live log updates should be visible in the admin UI within 10 seconds under normal site conditions. |
| NFR-3 | In `M3`, log ingestion must be bounded by retention, chunk size, and per-run limits. |
| NFR-4 | API writes from Scout must be idempotent, using event sequence numbers or attempt IDs to handle retries. |
| NFR-5 | RBAC must separate viewing runs/logs from controlling runs. |
| NFR-6 | Plugin owners and site administrators must prevent secrets in emitted logs; retained output must be access-controlled and bounded. |
| NFR-7 | The data model should not prevent future parallel execution, but the first implementation must remain sequential. |
| NFR-8 | The system must expose metrics for implemented milestone behavior, including run duration, test duration, failures, stale heartbeats, reconciliation, and later retry, cancellation, and log ingestion failures. |

# **2. System Architecture**

## **2.1 High-Level Architecture**

Machine Validation currently depends on Scout completing all tests and sending final completion back to the API. The redesigned architecture moves durable run ownership to the API. Scout reports events, logs, and heartbeats; the API computes durable state and reconciliation outcomes.

Current flow:

```text
machine-controller creates/loads validation run
        |
        v
host reboots into Scout
        |
        v
Scout fetches test definitions using context, tags, platform, enabled, verified filters
        |
        v
Scout executes tests sequentially and persists one final result per test
        |
        v
Scout sends MachineValidationCompleted
        |
        v
API sets run terminal state and updates machine last_machine_validation_time
        |
        v
machine-controller leaves Validation state
```

Proposed flow:

```text
machine-controller requests or loads validation run
        |
        v
API creates or loads durable run summary
        |
        v
host reboots into Scout
        |
        v
Scout claims run, fetches tests, and reports selected run items
        |
        v
Scout executes each run item and reports events and heartbeats
        |
        v
API updates run, item, and attempt state durably
        |
        v
API reconciler terminalizes stale or completed runs
        |
        v
machine-controller observes terminal run state and leaves Validation state
```

## **2.2 Component Breakdown**

| Component | Description |
| :---- | :---- |
| API service | Owns durable state transitions, stale-run reconciliation, and compatibility result projection. Later milestones add log ingestion and control actions. |
| Database | Stores run summaries, selected run items, execution attempts, and compatibility result projections. Later milestones add live/recent log chunks and control actions. |
| machine-controller | Moves the machine into and out of validation based on durable run state and compatibility terminalization signals. |
| Scout | Executes validation tests, reports run-item and attempt events, and sends heartbeats. Later milestones add live log streaming and control polling. |
| Admin UI | Shows run lists, run detail, progress, active test, and heartbeat status. Later milestones add live logs, retry history, and control actions. |
| Admin CLI | Provides run/result workflows first, then log/control workflows for operators and automation. |
| REST layer | Preserves existing workflows first, then exposes site-scoped run, log, and control resources as each gRPC workflow stabilizes. |

# **3. Detailed Design**

The detailed design is organized around the main operational flows:

1. Current gaps and implementation feasibility.
2. Run lifecycle and state transitions.
3. Durable data model and compatibility strategy.
4. gRPC and REST API design.
5. Scout execution, heartbeats, logs, and controls.
6. machine-controller integration.
7. Admin UI/CLI behavior.
8. Retry policy, failure handling, and migration milestones.

## **3.1 Current System and Gaps**

Machine Validation currently spans these areas:

| Area | Current implementation |
| :--- | :--- |
| API and gRPC model | `crates/rpc/proto/forge.proto`, `crates/api/src/handlers/machine_validation.rs`, and `crates/api-model/src/machine_validation.rs` define run, result, test catalog, on-demand, and mutation APIs. |
| Database | `crates/api-db/src/machine_validation.rs`, `crates/api-db/src/machine_validation_result.rs`, and `crates/api-db/src/machine_validation_suites.rs` store runs, final test results, and test definitions. |
| State machine | `crates/machine-controller/src/handler/machine_validation.rs` moves a host into `Validation/MachineValidation`, waits for Scout completion, then transitions back to host init or failed state. |
| Scout runner | `crates/scout/src/main.rs` receives `Action::MachineValidation`, runs `crates/machine-validation`, persists one final result per test, and sends a final completion event. |
| Admin UI and CLI | `crates/api/templates/validation*.html` and `crates/admin-cli/src/machine_validation` show runs, results, tests, and on-demand commands. |
| REST layer | `infra-controller-rest` exposes site-scoped Machine Validation tests, runs, results, and external config endpoints through API handlers, site workflows, and site-agent activities. |
| User documentation | `docs/provisioning/host-validation.md` explains the current workflow and CLI usage. |

### **3.1.1 Current Gaps**

| Gap | Description |
| :---- | :---- |
| Run lifecycle can hang | The machine-controller waits on `last_machine_validation_time` being newer than the state version. That timestamp changes only after the API accepts final completion. If Scout crashes, the API rejects a late result, completion fails, or retry handling does not complete cleanly, the handler can return `do_nothing()` and the machine can remain in validation. |
| No durable active test state | The run table has coarse status only. There is no durable per-test running state, heartbeat, attempt record, or reconciler that marks stale work timed out or aborted. |
| Logs are final artifacts only | Scout captures stdout/stderr after a command exits and persists them as final result data. Logs are not streamed while a test is running. |
| Pause and stop are not supported | The on-demand API includes `Stop`, but the handler currently returns `Cannot stop an on-demand validation request`. Scout does not have a control loop or cancellation mechanism while a long test is running. |

### **3.1.2 Implementation Feasibility Review**

The design is feasible if the first implementation is scoped around reliability before broader observability and control improvements.

| Area | Feasibility | Design decision |
| :--- | :--- | :--- |
| Current-system hardening | High | Ship first. It can reduce hung validations without waiting for new run-item tables. |
| Durable run items and attempts | High | Make this the foundation for every later feature. Logs, retries, controls, and UI state should attach to attempts. |
| Heartbeats and reconciliation | High after durable attempts | Use reconciliation to terminalize stale work and update the existing machine-controller compatibility signal. |
| Live logs | Medium | Feasible, but requires replacing the current final-output command helper with a streaming Scout command runner. |
| Pause and resume | Medium | Implement pause between tests first. Do not promise active-test checkpointing. |
| Active cancellation | Medium-low until Scout runner changes | Requires process-group or container-aware cancellation. Store controls early, but treat active process termination as a separate deliverable. |
| Retry failed tests | Medium | Feasible after attempts exist. Retries must append attempts and preserve prior logs/results. |

Implementation guardrails:

1. Deliver `M0` through `M2` as the first reviewable implementation slice.
2. Keep existing run/result APIs compatible until all callers move.
3. Do not require machine-controller to understand the full redesigned model in the first slice.
4. Add new APIs and tables additively.
5. Treat Scout command execution as a boundary. Live logs and active cancellation should use a new streaming, cancelable runner.
6. Keep REST resources additive and compatible.
7. Do not change test catalog semantics, naming, verification, or platform matching except where existing selection data must be copied into durable run items.

## **3.2 Validation Run Lifecycle**

The API owns the durable run state transition graph. Scout reports events; it does not directly infer the final machine lifecycle outcome. A reconciliation job can move stale `Queued`, `Preparing`, `Running`, or `CancelRequested` states to `TimedOut` or `Aborted`.

Stale-run recovery policy:

1. Scout should send a run heartbeat and active-attempt heartbeat every 30 seconds while validation work is active.
2. The default stale-heartbeat threshold should be 5 minutes, and configuration must not allow a threshold below three heartbeat periods.
3. Each run also has a deadline derived from selected test timeouts plus configured overhead.
4. If the run deadline has passed, deadline handling takes precedence and the run becomes `TimedOut`.
5. If the deadline has not passed but the active heartbeat is stale, the reconciler marks active work `TimedOut` when Scout was executing a known attempt.
6. The reconciler marks the run `Aborted` when the API cannot safely identify active work, when Scout never claims a queued run before the claim timeout, or when stale state indicates control-plane or Scout process loss rather than a test timeout.
7. Discovery, cleanup, and on-demand runs use the same heartbeat and deadline rules; the machine-controller handles their terminal outcomes differently based on whether the context is blocking.

The first implementation slice only needs the non-control states required to make stale work terminal and observable. `PauseRequested`, `Paused`, and `CancelRequested` are included here for the later control milestone so the state model does not need another redesign.

### **3.2.1 Run States**

| State | Meaning |
| :--- | :--- |
| `Queued` | Run is created but Scout has not started it. |
| `Preparing` | Scout is downloading images, external configs, or preparing the host. |
| `Running` | At least one test item is active or pending. |
| `PauseRequested` | Operator requested pause; Scout should stop starting new tests. |
| `Paused` | No test is running and the run can be resumed or canceled. |
| `CancelRequested` | Operator requested cancellation; Scout should stop active work. |
| `Succeeded` | All required tests succeeded or were allowed to skip. |
| `Failed` | One or more required tests failed and no allowed remediation remains. |
| `TimedOut` | The run exceeded its deadline or a required attempt became stale. |
| `Canceled` | The run was canceled by an operator or policy. |
| `Aborted` | The run ended because Scout or the control plane could not continue safely. |

### **3.2.2 Test Item States**

| State | Meaning |
| :--- | :--- |
| `Pending` | Test is selected but not started. |
| `Skipped` | Test was not run because its precondition or policy skipped it. |
| `Running` | Current attempt is active. |
| `RetryWaiting` | Attempt failed and retry is scheduled after backoff. |
| `Succeeded` | Test completed successfully. |
| `Failed` | Test failed and no retry remains. |
| `TimedOut` | Test attempt timed out and no retry remains. |
| `Canceled` | Test stopped due to run cancellation. |

### **3.2.3 Transition Principles**

1. The API owns durable state transitions.
2. Scout reports events and heartbeats in the first slice, and logs in `M3`.
3. The machine-controller reads terminal run state and failure details to decide whether the host returns to provisioning, reaches Ready, or enters Failed.
4. Run terminalization must not depend on a single final `MachineValidationCompleted` call.
5. The reconciler must be able to terminalize stale work and update compatibility state.
6. Every state-changing event must include a stable event ID or a monotonic sequence scoped to the run and attempt.
7. State updates must be fenced with transactional predicates such as "update only if current state is the expected non-terminal state".
8. Duplicate events with the same identity are idempotent no-ops; older or out-of-order events that would move state backward are rejected.
9. Gapped event sequences are rejected or held for retry until the missing event is observed; the API must not infer intermediate transitions without enough information.
10. Terminal states are immutable except for idempotent replay of the same terminal event. Reconciliation must not overwrite a run, item, or attempt that is already terminal.

## **3.3 Data Model and Storage**

The MVP should extend the existing data model instead of replacing it. This keeps current APIs and machine-controller behavior compatible while adding the durable execution model needed for reliability.

### **3.3.1 Compatibility Strategy**

1. Keep and extend the existing `machine_validation` table as the run summary table during migration. Do not add a separate run-summary table in the first implementation slice.
2. Add `machine_validation_run_items` and `machine_validation_attempts` keyed by the existing validation run ID for `M1`.
3. Add `machine_validation_log_chunks` in `M3` and `machine_validation_controls` in `M4`; these tables are not prerequisites for the reliability-first slice.
4. Keep `machine_validation_results` readable for historical results. Its `stdout` and `stderr` fields remain final result artifacts, not the live log source. New final-result responses can be projected from terminal attempts, and the existing table can continue to be populated as a compatibility projection until old clients are migrated.
5. Keep `MachineValidationCompleted` as a compatibility signal, but do not make it the only terminalization path.
6. Update `last_machine_validation_time` when a run reaches a terminal state through either Scout completion or reconciliation.
7. For `M0` through `M2`, the authoritative terminalization path is the durable run state. Scout completion, stale-run reconciliation, and manual terminalization all flow through the same API terminalization path.
8. Compatibility fields such as `last_machine_validation_time`, failure details, run status, and final `machine_validation_results` projection are updated from the durable terminal run state, not independently inferred by the machine-controller.
9. The machine-controller may keep reading compatibility fields during migration, but those fields must be derived from the durable terminalization path so there is one source of truth.

### **3.3.2 Database Design**

The first implementation slice should contain the existing run summary plus run item and attempt entities. Log chunks and controls are defined here as later milestone entities so their foreign keys and ownership are clear.

Finite domains must use canonical shared API/domain enums and must be enforced at the database boundary with database enums or check constraints. This applies to run state, item state, attempt state, validation context, log stream, control action, control status, and failure classification. Plain `TEXT` should be reserved for open-ended values such as reasons, summaries, commands, identifiers, and operator-provided messages.

| `machine_validation` existing table, extended |  |  |
| :---- | :---- | :---- |
| UUID | `id` | Run ID. |
| UUID | `machine_id` | Machine being validated. |
| `machine_validation_context` | `context` | Validation context such as `Discovery`, `Cleanup`, or `OnDemand`. |
| TEXT | `requested_by` | User, service, or system actor that requested the run. |
| TEXT | `requested_reason` | Optional operator or policy reason. |
| `machine_validation_run_state` | `state` | Durable run state. |
| BOOLEAN | `blocking` | Whether terminal failure blocks the machine lifecycle. |
| JSONB | `selection_filter` | Test selection filter used to select run items. |
| INTEGER | `selected_test_count` | Number of selected run items. |
| INTEGER | `completed_test_count` | Number of completed run items. |
| INTEGER | `failed_test_count` | Number of failed run items. |
| TIMESTAMPTZ | `started_at` | Run start timestamp. |
| TIMESTAMPTZ | `ended_at` | Run terminal timestamp. |
| TIMESTAMPTZ | `deadline_at` | Run deadline. |
| TIMESTAMPTZ | `last_heartbeat_at` | Last run heartbeat from Scout. |
| TEXT | `failure_summary` | Terminal failure summary. |
| TIMESTAMPTZ | `created_at` | Created timestamp. |
| TIMESTAMPTZ | `updated_at` | Updated timestamp. |

| `machine_validation_run_items` |  |  |
| :---- | :---- | :---- |
| UUID | `id` | Run item ID. |
| UUID | `run_id` | Parent run ID. |
| TEXT | `test_id` | Selected test ID. |
| TEXT | `test_version` | Selected test version. |
| TEXT | `display_name` | Human-readable test name. |
| `machine_validation_context` | `context` | Context this item was selected for. |
| TEXT | `component` | Target component, such as GPU, CPU, memory, storage, or compute. |
| `machine_validation_item_state` | `state` | Durable item state. |
| INTEGER | `order_index` | Sequential execution order. |
| INTEGER | `attempt` | Current attempt number. |
| INTEGER | `max_attempts` | Maximum allowed attempts. |
| INTEGER | `timeout_seconds` | Attempt timeout. |
| TIMESTAMPTZ | `started_at` | Item start timestamp. |
| TIMESTAMPTZ | `ended_at` | Item end timestamp. |
| TIMESTAMPTZ | `last_heartbeat_at` | Last heartbeat for the active attempt. |
| TEXT | `skip_reason` | Reason for skip, if skipped. |
| TEXT | `failure_reason` | Failure reason, if terminal failed. |

| `machine_validation_attempts` |  |  |
| :---- | :---- | :---- |
| UUID | `id` | Attempt ID. |
| UUID | `run_item_id` | Parent run item ID. |
| INTEGER | `attempt_number` | Attempt number for the run item. |
| `machine_validation_attempt_state` | `state` | Attempt state. |
| TEXT | `command` | Command executed. |
| TEXT | `args` | Command arguments. |
| TEXT | `container_image` | Container image, if any. |
| BOOLEAN | `execute_in_host` | Whether command is executed in host context. |
| INTEGER | `exit_code` | Process exit code. |
| `machine_validation_failure_classification` | `failure_classification` | Timeout, command failure, API error, canceled, or other classification. |
| TIMESTAMPTZ | `started_at` | Attempt start timestamp. |
| TIMESTAMPTZ | `ended_at` | Attempt end timestamp. |
| TIMESTAMPTZ | `last_heartbeat_at` | Last attempt heartbeat. |
| TEXT | `stdout_summary` | Bounded stdout summary. |
| TEXT | `stderr_summary` | Bounded stderr summary. |

| `machine_validation_log_chunks` |  |  |
| :---- | :---- | :---- |
| UUID | `run_id` | Parent run ID. |
| UUID | `run_item_id` | Parent run item ID. |
| UUID | `attempt_id` | Parent attempt ID. |
| BIGINT | `sequence` | Stream sequence number. |
| `machine_validation_log_stream` | `stream` | `stdout` or `stderr`. |
| TIMESTAMPTZ | `timestamp` | Chunk timestamp. |
| TEXT | `content` | Log content. |
| BOOLEAN | `truncated` | Whether the chunk was truncated. |
| CONSTRAINT | `UNIQUE (attempt_id, stream, sequence)` | Idempotency key for retried log chunks. A duplicate with identical content is ignored; the same key with different content is rejected. |

| `machine_validation_controls` |  |  |
| :---- | :---- | :---- |
| UUID | `id` | Control request ID. |
| UUID | `run_id` | Target run ID. |
| `machine_validation_control_action` | `action` | `Pause`, `Resume`, `Cancel`, `RetryFailed`, `RetryTest`, or `MarkAborted`. |
| TEXT | `requested_by` | Actor requesting control. |
| TEXT | `reason` | Operator or policy reason. |
| TIMESTAMPTZ | `requested_at` | Request timestamp. |
| TIMESTAMPTZ | `acknowledged_at` | Scout/API acknowledgement timestamp. |
| TIMESTAMPTZ | `completed_at` | Control completion timestamp. |
| `machine_validation_control_status` | `status` | Pending, acknowledged, completed, rejected, or failed. |
| TEXT | `status_message` | User-actionable status details. |

### **3.3.3 Log Storage Defaults**

The current system does not have a live log destination. Scout captures command stdout/stderr only after `TokioCmd::output_with_timeout()` returns, sends the completed `MachineValidationResult` through `PersistValidationResult`, and the API stores the final output in `machine_validation_results.stdout` and `machine_validation_results.stderr`.

The redesigned system stores live/recent streamed output in `machine_validation_log_chunks`. Scout sends stdout/stderr chunks while the process is active through `AppendMachineValidationLog`; the API persists the emitted chunks by run, run item, attempt, stream, and sequence number. `machine_validation_results.stdout` and `machine_validation_results.stderr` remain available as a compatibility final-result projection for existing result readers.

Initial live-log defaults should be configurable, but the first implementation should start with conservative limits:

| Setting | Default |
| :---- | :---- |
| Maximum log chunk size | 16 KiB |
| Maximum retained live/recent log data | 10 MiB per attempt and 100 MiB per run |
| Terminal run log retention in database | 14 days |
| Full long-term logs | Debug bundle or object storage in a later milestone |
| Sensitive output | Plugin owners and site administrators must not emit secrets, access tokens, or external configuration values |

The database log chunks are intended for bounded live/recent visibility. Full long-term log archival is outside the first implementation and should be decided separately as database retention, object storage, debug bundle content, or a combination.

Security requirements for log persistence:

1. Plugin owners and site administrators must ensure stdout and stderr do not contain secrets or raw external configuration values before `AppendMachineValidationLog` persists them.
2. Scout and wrappers should avoid printing external configuration. NICo stores emitted chunks without inspecting or redacting them.
3. Retained chunks are privileged operational data and should be readable only by users with machine-validation log access.
4. Retention cleanup must delete log chunks after the configured retention period and should be independent from the final run summary retention.
5. Tests must cover log access control, size bounds, retention cleanup, and sequence handling.

## **3.4 Component Details**

### **3.4.1 External/User-facing APIs**

The first redesign API set should be additive so existing clients continue to work. `M0` through `M2` should expose only the APIs needed for durable run state, run items, attempts, heartbeat, and reconciliation. Log and control APIs are later milestone additions.

| API | Milestone | Purpose |
| :--- | :--- | :--- |
| `CreateMachineValidationRun` | `M1` | Creates or loads a durable run summary. The first slice does not need to return a fully materialized execution plan. |
| `GetMachineValidationRun` | `M1-M2` | Returns run summary, progress, heartbeat, current test, and terminal status. |
| `ListMachineValidationRuns` | `M1-M2` | Lists current and historical runs with filters. |
| `ListMachineValidationRunItems` | `M1` | Lists selected tests and per-test state. |
| `GetMachineValidationAttempt` | `M1-M2` | Returns attempt metadata and final result summary. |
| `GetMachineValidationLogs` | `M3` | Returns recent log chunks by run, test, attempt, and stream. |
| `ControlMachineValidationRun` | `M4` | Requests pause, resume, cancel, mark aborted, and later retry failed or retry test in `M4c`. |

`CreateMachineValidationRun` idempotency contract:

1. External callers must provide an idempotency key or client request ID. Internal machine-controller requests may use a deterministic key derived from machine ID, validation context, request source, and the state-machine request/version that triggered the run.
2. The API stores the idempotency key with the run and enforces uniqueness for active and recently completed requests.
3. A retry with the same key and equivalent payload returns the original run ID and current run state.
4. A retry with the same key but a materially different payload is rejected as a request conflict.
5. REST workflow timeouts must not create duplicate runs; callers retry with the same idempotency key and receive the same run ID.
6. The corresponding REST create-run endpoint follows the same semantics.

### **3.4.2 Scout Reporting APIs**

| API | Milestone | Purpose |
| :--- | :--- | :--- |
| `ReportMachineValidationEvent` | `M1` | Idempotent event API for run/test/attempt state transitions. |
| `HeartbeatMachineValidationRun` | `M2` | Reports run and active attempt heartbeat. |
| `CompleteMachineValidationRun` | `M1-M2` | Marks Scout-side execution complete. API still computes final state from run items. |
| `AppendMachineValidationLog` | `M3` | Appends stdout/stderr chunks with sequence numbers. |
| `GetMachineValidationControl` | `M4` | Lets Scout poll for pause/resume/cancel/retry actions. |

Scout reporting idempotency contract:

1. `ReportMachineValidationEvent` includes a stable event ID and a monotonic event sequence scoped to the run and, when applicable, the run item or attempt.
2. The API records accepted event IDs and the last accepted sequence for each scope.
3. Duplicate events with identical identity and payload are ignored and return the current durable state.
4. Duplicate event IDs with conflicting payloads are rejected.
5. Events older than the last accepted sequence are ignored if they repeat an already-applied transition and rejected if they conflict.
6. Sequence gaps are rejected with a retryable precondition error unless the event can be proven independent of the missing transition.
7. `AppendMachineValidationLog` uses `(attempt_id, stream, sequence)` as the idempotency key.

### **3.4.3 REST Resources**

The REST layer should initially preserve existing workflows and add run visibility only after the site-controller gRPC model stabilizes. Log and control REST resources should follow their gRPC milestones rather than becoming part of the first reliability slice.

| REST Method and Endpoint | Description |
| :---- | :---- |
| `POST /v2/org/{org}/nico/site/{siteID}/machine-validation/runs` | Create a run in `M1` after gRPC run creation stabilizes. |
| `GET /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}` | Fetch run summary in `M1-M2`. |
| `GET /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}/tests` | Fetch run items in `M1-M2`. |
| `GET /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}/logs?sinceSequence=<n>&limit=<n>` | Fetch log chunks in `M3`. |
| `POST /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}/controls` | Pause, resume, cancel, or retry a run in `M4`; retry is scoped to `M4c`. |

REST requirements:

1. REST state names must match the gRPC state names.
2. REST handlers must return run, control, and validation-execution errors in a user-actionable format.
3. REST task/workflow timeouts must not hide the underlying run state. If a workflow times out while dispatching a long validation, the run should still be queryable by `runID`.
4. OpenAPI schemas and generated SDKs must include each run, log, and control resource when its milestone is exposed.
5. Provider admin permissions should be required for run control, while read-only operators may view runs/logs according to site policy.
6. Existing singular REST routes should remain as compatibility aliases until REST clients move to the plural run/test resources.

### **3.4.4 Mapping REST to gRPC**

| REST Method and Endpoint | gRPC Method | Description |
| :---- | :---- | :---- |
| `POST /v2/org/{org}/nico/site/{siteID}/machine-validation/runs` | `CreateMachineValidationRun` | Create or load a durable run summary. |
| `GET /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}` | `GetMachineValidationRun` | Fetch run summary. |
| `GET /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}/tests` | `ListMachineValidationRunItems` | Fetch selected run items. |
| `GET /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}/logs` | `GetMachineValidationLogs` | Fetch log chunks. |
| `POST /v2/org/{org}/nico/site/{siteID}/machine-validation/runs/{runID}/controls` | `ControlMachineValidationRun` | Request pause, resume, cancel, or retry. |

### **3.4.5 Error Handling**

API handlers must construct the repository domain error type first and convert it to `tonic::Status` with `.into()`. The style guide names this pattern `NicoError`; the current Rust API core uses `CarbideError` for the same domain-error role. Handlers should not construct `tonic::Status` directly except at infrastructure boundaries that already own authentication or transport failure handling.

| REST | gRPC Status | Domain error | Notes |
| :---- | :---- | :---- | :---- |
| 400 Bad Request | `INVALID_ARGUMENT` | `NicoError::InvalidArgument` / `CarbideError::InvalidArgument` | Malformed request, invalid control action, invalid retry target, or invalid state transition. |
| 401 Unauthorized | `UNAUTHENTICATED` | Auth middleware or `NicoError::Unauthenticated` equivalent | Invalid credentials. |
| 403 Forbidden | `PERMISSION_DENIED` | `NicoError::PermissionDenied` / `CarbideError::PermissionDeniedError` | Actor cannot view logs or control the run. |
| 404 Not Found | `NOT_FOUND` | `NicoError::NotFound` / `CarbideError::NotFoundError` | Run, run item, attempt, or log resource does not exist. |
| 409 Conflict | `FAILED_PRECONDITION` or `ABORTED` | `NicoError::FailedPrecondition` or conflict equivalent / `CarbideError::FailedPrecondition` | Run is not in a state that accepts the requested action, or an idempotency key is replayed with conflicting payload. |
| 422 Unprocessable Entity | `INVALID_ARGUMENT` | `NicoError::InvalidArgument` / `CarbideError::InvalidArgument` | Schema-valid request that violates validation policy. |
| 503 Service Unavailable | `UNAVAILABLE` | `NicoError::Unavailable` / `CarbideError::UnavailableError` | Site controller or Scout execution path cannot accept work. |
| 500 Internal Server Error | `INTERNAL` | `NicoError::Internal` / `CarbideError::Internal` | Unexpected server error. |

## **3.5 Scout Execution Design**

Scout may keep the current test selection path in the first implementation slice. The reliability requirement is that selected tests and attempts become durable before or as they start, so operators and the reconciler can identify active and stale work. A later implementation can move to a fully API-provided execution plan if that is still useful.

Scout execution loop:

1. Claim or start the run and report `Preparing`.
2. Download images and external configs.
3. Report `Running`.
4. For each run item:
   - report `TestStarted`
   - create attempt
   - start command with the current runner; `M4b` can move active execution into a process group or container-aware runner
   - send stdout/stderr chunks through `AppendMachineValidationLog` in `M3`
   - heartbeat while active
   - poll control actions in `M4`
   - finish attempt with exit code, timeout, cancellation, or error
   - retry if policy allows in the retry-control milestone
5. Report Scout-side execution complete.
6. Let the API reconcile final run state.

Cancellation behavior in `M4`:

1. In `M4a`, Scout sees `CancelRequested`, stops scheduling new tests, and marks pending tests canceled.
2. If a process is already running, `M4a` lets the active test finish unless the deployment explicitly accepts active termination risk.
3. In `M4b`, Scout can terminate the active process group or container gracefully.
4. If the process is still alive after the grace period, Scout force kills it.
5. Scout marks the active attempt and pending tests as canceled.
6. API marks the run `Canceled` and the machine-controller unblocks the host.

Pause behavior:

1. Scout sees `PauseRequested`.
2. If no test is running, Scout acknowledges the pause and API marks the run `Paused`.
3. If a test is running, Scout lets the active test finish and does not start another test.
4. Active-test interruption is cancellation, not pause, in the first control implementation.
5. `Resume` returns the run to `Running`.
6. Future versions may add active-test checkpointing only for tests that explicitly support it.

## **3.6 machine-controller Design**

The machine-controller should use durable run state rather than relying only on `last_machine_validation_time`.

Required behavior:

1. When entering validation, create or load a run and set the host state to reference that run ID.
2. While validation is active, observe run state and heartbeat freshness.
3. If the run is terminal success, transition the host to the next provisioning state.
4. If the run is terminal failure, timeout, canceled, or aborted:
   - for blocking discovery/cleanup validation, transition to `Failed` with Machine Validation failure details
   - for non-blocking validation, preserve the machine lifecycle state and publish health/validation status
5. If Scout never starts or stops heartbeating, rely on API reconciliation to make the run terminal.

## **3.7 Admin UI and CLI Design**

The first run list should show:

1. run ID
2. machine ID
3. context
4. state
5. progress
6. current test
7. elapsed time
8. last heartbeat age
9. blocking/non-blocking mode

The first run detail should show:

1. run summary
2. selected test list with state, attempt count, duration, exit code, and failure reason
3. final result summaries
4. stale heartbeat and reconciliation details

Later `M3` and `M4` views should add live stdout/stderr log tail, control buttons, retry history, and audit events.

## **3.8 Retry Policy**

Retry execution should be deferred until after durable attempts exist and control workflows are available. The first implementation slice should preserve attempt history and failure reasons, but it does not need to re-run failed tests automatically. When retry is added, policy should be explicit:

```yaml
retry_policy:
  max_attempts: 2
  retry_on_timeout: true
  retry_on_exit_codes: [124, 137]
  backoff_seconds: 60
```

Rules:

1. Default max attempts is 1.
2. Retries must be visible as separate attempts.
3. A retry must not overwrite the previous attempt logs or result.
4. The final run state must include the first failing reason and the final failing reason.
5. Operators can retry failed tests only when the run is terminal and the machine is still in an allowed state.

## **3.9 Failure Handling**

| Failure | Required behavior |
| :--- | :--- |
| Scout crashes mid-run | Heartbeat expires; API marks active attempt stale and run `Aborted` or `TimedOut`; machine-controller unblocks the machine. |
| API rejects a result | Scout retries idempotently; if still failing, API reconciliation eventually marks the run terminal based on heartbeat/deadline. |
| Test command hangs | Attempt timeout fires; API or Scout marks the attempt timed out; retry policy decides next state only after the retry milestone is implemented. |
| Operator cancels | In `M4a`, Scout stops scheduling new tests; in `M4b`, Scout can stop active work; API records canceled state and audit reason. |
| Log upload fails | In `M3`, Scout retries log chunks; missing chunks are marked by sequence gaps; test execution can continue. |

## **3.10 Implementation Milestones**

The milestones intentionally separate reliability from observability and control. This keeps the first deliverable focused on preventing hung validations. `M0` through `M2` should be reviewed and delivered as the first implementation slice.

| Milestone | Name | Primary Outcome | Notes |
| :---- | :---- | :---- | :---- |
| `M0` | Current System Hardening | Reduce immediate hung-validation risk in the current model. | Can land before the full run-item/attempt data model. |
| `M1` | Durable Execution Foundation | Persist selected run items and execution attempts. | Foundation for retries, logs, controls, and UI state. |
| `M2` | Heartbeat and Recovery | Detect stale Scout work and terminalize runs by policy. | Completes the first reliability implementation slice. |
| `M3` | Logs and Operator Visibility | Stream bounded stdout/stderr and expose live run detail. | Requires a streaming Scout command runner. |
| `M4` | Control Operations | Add pause, resume, and cancel controls. | Split into between-test controls (`M4a`), active process cancellation (`M4b`), and retry controls (`M4c`). |

### **3.10.1 M0: Current System Hardening**

1. Set run state to `InProgress` when Scout calls `UpdateMachineValidationRun`.
2. If `MachineValidationManager::run` returns an error, send `MachineValidationCompleted` with `machine_validation_error`.
3. Add a conservative stale-run reconciler for active validations with no progress after a configured timeout.
4. Add metrics for active run age, stuck run count, and completion errors.

### **3.10.2 M1: Durable Execution Foundation**

1. Add run item and attempt tables.
2. Persist selected tests before or as Scout starts them.
3. Add idempotent Scout event reporting.
4. Preserve current run/result APIs by projecting from redesigned attempts.
5. Keep `machine_validation` as the compatibility run summary.
6. Keep Scout execution sequential. The data model may allow future parallel execution, but this milestone should not introduce it.

### **3.10.3 M2: Heartbeat and Recovery**

1. Add run and active-attempt heartbeat.
2. Reconcile stale Scout runs and stale attempts into terminal states.
3. Update machine terminalization through the same compatibility path used today.
4. Add metrics and alerts for stale validation.
5. Add tests for Scout crash, missed completion, API rejection, and stale active attempt scenarios.

### **3.10.4 M3: Logs and Operator Visibility**

1. Add a streaming Scout command runner that can read stdout/stderr while the process is active.
2. Add bounded log chunk ingestion with sequence numbers.
3. Add admin UI and CLI live log tail.
4. Add progress, current test, attempt count, heartbeat freshness, and failure reason to run views.
5. Add log access control and retention limits.
6. Keep full long-term archival out of scope; use bounded recent logs first.

### **3.10.5 M4: Control Operations**

Split this milestone so lower-risk control operations can ship before active process termination.

M4a: control records and between-test controls:

1. Add pause, resume, and cancel control APIs.
2. Add Scout control polling between tests.
3. Implement pause as "do not start the next test".
4. Implement cancel as "stop scheduling new tests and mark pending tests canceled".
5. Add audit records and RBAC policy.

M4b: active process cancellation:

1. Add process-group or container-aware cancellation for the active test.
2. Gracefully terminate first, then force kill after a configured grace period.
3. Record cancellation details on the active attempt.
4. Keep active-test pause/checkpointing out of scope unless a test explicitly supports it.

M4c: retry controls:

1. Add retry-failed and retry-test control APIs after attempts and basic controls are stable.
2. Preserve all previous attempt logs and results.
3. Append new attempts rather than rewriting terminal attempt data.
4. Keep automatic retry policy disabled by default until operators validate retry behavior per test class.

## **3.11 Future Work Outside This Proposal**

The following work is intentionally excluded from this reliability/control redesign and should be proposed separately if needed:

1. Test catalog redesign.
2. Guided test authoring UI.
3. Test ID naming migration.
4. Draft/verified/deprecated catalog lifecycle changes.
5. Platform profiles, SKU alias ownership, and selection preview workflows.
6. Legacy test ID deprecation.

# **4. Technical Considerations**

## **4.1 Security**

1. Internal Scout-to-API and service-to-service communication must continue using existing mTLS and authorization.
2. Viewing logs and controlling runs must be governed by RBAC. Viewing a run is not the same permission as canceling or retrying it.
3. Control operations must be audited with actor, reason, timestamp, requested action, acknowledgement, completion, and outcome.
4. Logs and external configuration data may contain secrets. Plugin owners and site administrators must ensure secrets are not emitted to stdout, stderr, or `machine_validation_log_chunks`; NICo stores emitted chunks without inspecting or redacting them.
5. In `M4b`, active cancellation must terminate only the process group or container associated with the active validation attempt.
6. REST and gRPC APIs must validate state transitions so clients cannot skip required terminalization or retry rules.

## **4.2 Observability**

The system must expose metrics for implemented milestones:

1. active validation count
2. run duration
3. test duration
4. retry count after retry controls are implemented
5. failure count
6. cancellation count after control operations are implemented
7. stale heartbeat count
8. reconciled run count
9. log ingestion failures after live log ingestion is implemented
10. active run age

The admin UI and CLI should surface stale heartbeat warnings and reconciliation outcomes in run detail views.

## **4.3 Acceptance Criteria**

First implementation slice (`M0` through `M2`):

1. A Scout crash during a validation run does not leave the machine in validation indefinitely.
2. A stale or hung attempt is marked timed out or aborted by policy, and the machine is unblocked through the compatibility terminalization path.
3. A failed or timed-out attempt is preserved with failure reason and terminal state so a future retry can append another attempt without deleting previous data.
4. Existing run and result history remains readable.
5. Existing `GetMachineValidationRuns`, `GetMachineValidationResults`, and on-demand start flows remain compatible.
6. Run items and attempts are persisted before or during execution so an operator can identify the selected test and current attempt after a Scout failure.
7. Stale-run metrics and alerts identify active validations that required reconciliation.

Logs and visibility (`M3`):

1. Live stdout/stderr appears in the admin UI while a long-running test is active.
2. The UI shows current test, attempt count, heartbeat freshness, and failure reason.
3. Log chunks are bounded, sequenced, and access-controlled according to policy. Plugin owners and site administrators ensure they do not contain secrets.
4. Missing log chunks are visible as sequence gaps rather than silently hidden.

Control operations (`M4`):

1. An operator can cancel an on-demand validation before the next test starts and see the run reach `Canceled`.
2. An operator can pause a validation between tests and resume it.
3. Active process cancellation is available only after the streaming/cancelable Scout runner is in place.
4. Retry controls, when enabled, append new attempts and preserve previous attempt results.
5. Control operations are audited with actor, reason, timestamp, and outcome.

## **4.4 Open Questions**

1. Should discovery and cleanup validation always be blocking, or should some validation contexts or runs be warning-only?
2. Where should full logs be archived: database only, object storage, debug bundle, or a combination?
3. What timeout and heartbeat thresholds should the reconciler use for discovery, cleanup, and on-demand contexts?
4. Should `M4a` ship pause/resume together, or should cancellation-only be the first operator-control release?
5. Which validation tests are safe to retry automatically, and which should require explicit operator retry?
