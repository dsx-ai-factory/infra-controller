# Reliable State Handling

NVIDIA Infra Controller (NICo) uses state controllers to move resources through lifecycle states despite temporary
failures and process restarts.

State controllers give NICo a consistent place to resume work. The framework provides scheduling, persistence hooks,
and common observability. Each handler and persistence implementation must make its database changes safe to retry and
reconcile. Handlers must also make their external effects safe to retry and reconcile.

The framework does not make database changes and external input/output (I/O) atomic. It also does not guarantee
exactly-once execution, enforce stale-write protection, or enforce one lifecycle writer for every resource.

## Controller Model

State controllers manage lifecycles for resources such as:

- Machines and managed hosts
- InfiniBand (IB) partitions
- Network segments and virtual private cloud (VPC) prefixes
- Security Protocol and Data Model (SPDM) attestations
- Power shelves, racks, and switches

### Separate Lifecycle Decisions From Persistence

Two generic interfaces define the processor contract:

- **[`StateHandler`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/state-controller/src/state_handler.rs):**
  Selects the handling outcome for the resource.
- **[`StateControllerIO`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/state-controller/src/io.rs):** Loads
  resources and versioned controller state. It also persists controller state, history, and applicable outcomes and
  supplies metrics, service-level agreement (SLA), and manual-intervention reason tokens.

The processor orders the shared I/O hooks and commits the final outcome transaction. Each `StateControllerIO`
implementation defines its concrete persistence behavior. Each handler owns the work it performs before returning,
including its external-I/O recovery protocol.

### Keep Lifecycle Ownership Explicit

Design each resource lifecycle around one primary lifecycle owner. Other components should persist requests or
observations and let that lifecycle owner advance controller state.

This is a design boundary, not a guarantee enforced by the framework. Bootstrap, discovery, recovery, and selected
command paths can write lifecycle state directly. Treat each direct writer as an explicit handoff or documented
exception. Make it participate in the same concurrency and recovery protocol as the normal owner.

Do not add a second lifecycle writer implicitly.

### Model Every State

For machines and managed hosts, an exhaustive `match` over
[`ManagedHostState`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-model/src/machine/mod.rs) makes
unhandled states visible to the compiler.

The primary transition logic lives in the
[machine-controller handler](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/machine-controller/src/handler.rs).
The [managed-host state diagrams](state_machines/managedhost.md) show the rendered flows.

### Measure Progress

When a meter is configured, the framework measures state transitions, time in state, handler latency, and failures.
Each controller can add resource-specific measurements.

A controller can define an SLA for individual lifecycle states. When a state defines one, the framework records
whether a resource has exceeded it. Deployments can use those measurements for alerting.

## Scheduling and Outcomes

### Enqueue Work

Each controller owns a
[`StateControllerConfig`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/state-controller-common/src/config.rs)
instance. The
[`StateControllerConfig` reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/cfg/README.md#statecontrollerconfig)
lists all scheduling settings and defaults. Controllers receive work through three paths:

- **Periodic enqueue:** Each controller periodically enqueues its resources according to `iteration_time`.
- **Transition enqueue:** After a committed transition, finalization schedules an immediate requeue.
- **Explicit enqueue:** Other control-plane components can request an earlier run through the
  [`Enqueuer`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/state-controller/src/controller/enqueuer.rs).

Explicit enqueue is idempotent and does not interrupt active handling. When the resource is already queued or claimed,
the request does not create a second queue entry. If active handling does not schedule another run, finalization can
remove that wake-up with the current queue row. Periodic scheduling is the fallback.

### Claim Work and Guard Writes

Before dispatching work, the processor claims queued resources in the database. The processor bounds one handling
attempt with `max_object_handling_time`.

Each processor attempts to handle up to `max_concurrency` resources at the same time. It checks for more work according
to `processor_dispatch_interval`.

Another processor can reclaim a queue row after its `processing_started_at` timestamp is older than 3 times the
configured timeout. The queue claim coordinates dispatch. It does not fence the lifecycle-state write or an external
effect.

The processor passes `old_version` and `new_version` to `persist_controller_state`, but those values do not enforce a
guard by themselves. Each `StateControllerIO` implementation must apply the conflict protection that its storage model
requires. Common guards include:

- An optimistic version predicate
- An owner or claim predicate
- A transactionally-locked reread
- Another guard with equivalent protection

The processor cannot detect a stale write when an implementation ignores `old_version` and reports success. Check the
concrete implementation before relying on stale-write rejection. The machine and SPDM controller implementations do
not apply an `old_version` guard.

When persistence reports a conflict, the processor leaves controller state and history unchanged. It logs the conflict,
tracks it in memory, and schedules a requeue to load the winning state. Handlers and persistence logic must remain safe
when work is retried or another processor takes over a stale claim.

### Handle Each Outcome

Here, *handler-owned writes* means database writes queued through the handler context or returned in the outcome's
transaction.

The processor applies all queued handler-owned writes in one transaction. If one write fails, the transaction rolls
back and the outcome becomes a handler error.

`StateHandlerOutcome` controls persistence and later scheduling:

- **`Wait`:** Keeps controller state unchanged and persists the wait reason. A later periodic or explicit enqueue
  resumes processing.
- **`Transition`:** Asks the persistence implementation to store the next state. When persistence reports success, the
  processor stores state history and the outcome in the same transaction. Finalization then schedules an immediate
  requeue.
- **`DoNothing`:** Keeps controller state unchanged and persists the no-op outcome. A later periodic or explicit
  enqueue can run the handler again.
- **`Deleted`:** Commits handler-owned database work and skips outcome persistence. Finalization removes the queue
  claim. The per-object state series is also cleared after processing a deleted or missing object.

A rejected transition has an important consequence. Controller state and history remain unchanged, but the
`Transition` outcome and handler-owned writes can still commit. The processor logs the conflict, tracks it in memory,
and schedules an immediate requeue. It does not persist a distinct conflict outcome.

Outcome persistence and queue finalization use separate transactions. If the process stops between them, the queue
claim remains until finalization succeeds or another processor reclaims the stale claim.

Any handler-owned write that survives a conflict must be correct without the transition. Otherwise, protect it with
the same ownership or version guard.

### Record Errors and Diagnose Retries

Failures alter normal outcome handling as follows:

- **SLA overrun:** A successful `Wait` or `DoNothing` result becomes an SLA error after the resource exceeds its
  state-specific SLA. Successful handler-owned writes still commit.
- **Handler error:** Controller state remains unchanged. The processor discards queued handler writes and tries to
  persist an error outcome in a new transaction.
- **Load error or missing object:** Processing stops before the handler runs. The processor records the error in logs
  and configured metrics but does not persist an outcome.
- **Timeout or persistence failure:** An outcome record is not guaranteed. Logs and configured metrics still report the
  failure, and a later enqueue can retry the resource.

The processor cannot roll back an external effect or database work that a handler committed independently.
A timeout also does not prove that remote or independently spawned work stopped. Treat a timed-out external call as
ambiguous and reconcile it before retrying.

Persistence and handler timeouts can also roll back an uncommitted transition, including its state, history, outcome,
and handler-owned writes. The next run observes the previous durable state and must recover from that fact.

During an orderly shutdown, the processor stops claiming new queue rows. It waits for already-claimed handlers, which
remain bounded by `max_object_handling_time`, then finalizes their queue rows and transition requeues. Unclaimed rows
stay queued. A process crash skips this drain, so another processor must reclaim stale claims.

For machines, the admin command-line interface (CLI) exposes persisted state history for diagnosing transitions and
retries. Refer to [Query State History](../playbooks/stuck_objects/diagnostic_tools.md#query-state-history).

## External Effects and Crash Recovery

State handling often crosses both a database boundary and an external-system boundary. Treat the database as the
durable reconciliation record, not as a cross-system transaction.

A database rollback cannot undo completed Redfish operations against a baseboard management controller (BMC), switch,
DPU, or another external system. Build every such transition to recover from each crash point.

### Persist a Recovery Point

Before an external action can become ambiguous, make sure durable state contains enough information to identify and
resume it. A recovery point can include:

- A target state
- A stable operation identifier or idempotency key
- A substate or attempt record
- An observed external identity or deterministic discovery key
- Facts needed to select and resume a documented compensation operation

Do not leave an in-memory future or local variable as the only evidence that work started.

A committed intent counts as a recovery point only when it contains or deterministically yields the information needed
to reconcile an ambiguous call.

### Reconcile Ambiguous Results

An external call can partially succeed. It can also succeed completely while its response is lost. Do not assume that
the previous call did nothing or completed exactly once.

Use stable identities or idempotency keys when the integration supports them. Otherwise, persist enough correlation
data to rediscover the effect or use another documented recovery protocol.

Classify the result and take the matching action:

- **Absent:** Retry only when repetition is safe.
- **Complete and desired:** Adopt the effect and persist completion.
- **Partial or unwanted:** Finish, compensate for, or remove the effect only through a documented safe operation.
- **Unknown:** Continue safe observation, replay through a documented way to retry safely, or use compensation that is
  safe for every possible result.

Fail closed when absent work cannot be repeated safely, a partial or unwanted result cannot be resolved safely, or the
outcome remains unknown. Retain the durable recovery point, leave the lifecycle unadvanced, and record an actionable
error.

Finishing, compensating for, and removing effects are external operations too. Persist their progress and give them the
same crash-window and retry analysis as the original action. Compensation is not a database rollback.

### Choose a Recoverable Write Order

There is no universal rule to write before or after an external call. Choose the order from the recovery evidence
available to the next run. Changing the database and external-I/O order does not make an operation recoverable by
itself.

#### Database Before External I/O

Commit a pending or awaiting-verification recovery point before the external call. Do not make the external call if
that commit fails.

Recover each crash window as follows:

- **Process stops before the call:** Start the operation only when durable facts prove that no attempt began. Otherwise,
  apply the ambiguous-result recovery protocol.
- **Process stops after the call:** Apply the ambiguous-result recovery protocol from the pending recovery point.

#### External I/O Before the Database

Use this order only when the previous durable state provides a stable operation identity, deterministic discovery key,
documented way to retry safely, or another recovery protocol that is safe for every possible result.

Both crash windows rely on that recovery method:

- **Process stops before the response:** Apply the ambiguous-result recovery protocol.
- **Process stops after success but before the database write:** Apply the ambiguous-result recovery protocol, then
  persist the verified result.

If the previous durable state cannot support recovery, persist the required fact first. If that is not possible, defer
or reject the transition and record an actionable error.

### Keep Database Transactions Short

A database transaction can protect related database facts, but it cannot make database changes and external effects
atomic. Use these transaction boundaries:

- **State load:** The processor reads the object snapshot and controller state in a short transaction. It commits that
  transaction before the handler performs external work.
- **Related database changes:** Keep changes in one transaction when they must agree with each other.
- **Database first:** Commit the recovery-point transaction before the external call. Skip the call if the commit fails.
- **External I/O first:** Perform the call without an open transaction. Start a new transaction for the recoverable
  follow-up write.

### Treat Work Locks as Leases

A `WorkLock` and a queued-object claim are separate coordination mechanisms. Neither provides exactly-once execution.

When NICo coordinates long-running work through the
[work-lock manager](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-db/src/work_lock_manager.rs), a
`WorkLock` is an expiring lease. Its keepalive loop can lose the lease while the caller continues running. Keeping the
Rust value in scope is not proof of current ownership.

Use the lease as follows:

- **Database writes:** Call `WorkLock::fence_transaction` before protected writes and perform those writes in the same
  short transaction. Treat every fencing error, including a database query failure, as a failed fence. Roll back or
  discard the transaction.
- **Retries:** Retry protected database writes only in a new transaction after `fence_transaction` succeeds there.
- **External effects:** Apply external fencing or compare-and-set, safe repetition, serialization, and reconciliation as
  required.
- **After ownership loss:** Continue read-only discovery only when it is safe without the lease. Acquire a new lock and
  reestablish every required protection before mutating external state.

`WorkLock` starts a background task that attempts to renew the lease automatically. `fence_transaction` does not renew
the lease. Nominal lease expiry does not prove ownership loss. If no takeover has changed the worker ID,
`fence_transaction` can still succeed and serialize a later takeover behind the transaction. If a takeover has changed
the worker ID, wait until a new `WorkLock` acquisition succeeds before protected work. Do not use elapsed time as proof
of ownership or loss.

`fence_transaction` is opt-in. Any transaction whose database writes rely on `WorkLock` ownership must first call
`fence_transaction` on a `WorkLock` acquired for the same key. Perform and commit all protected writes in that
transaction. The mechanism cannot stop a transaction that omits the fence or coordinates on another key. External
effects also need every applicable protection.

### Protect Shared External Resources

A per-resource controller claim does not protect an external resource shared by several NICo resources. Apply every
protection that the external operation requires:

- Reject stale writers through external fencing or compare-and-set.
- Serialize live operations when overlap is unsafe.
- Make the external operation idempotent or otherwise safe to repeat. Apply the same repetition-safe protocol on the
  first attempt and every retry.
- Reconcile ambiguous outcomes before making another change.

These protections solve different problems. One does not replace another.

If a required protection is unavailable, defer or reject the state transition. Record an actionable error instead of
modifying the shared resource.

### Document and Test the Recovery Invariant

For each transition, document these facts:

- The durable fact at every crash point
- What the next run can observe
- The safe action for complete, absent, partial, unwanted, and unknown results
- The ownership, conflict, fencing, and retry protections
- The fail-closed result when safe recovery is unavailable

Test every applicable recovery path:

- Successful transitions and no-op outcomes
- Retries and timeouts
- Restarts before a response and on both sides of each durable write
- Partial external success
- Deletion and cancellation
- Concurrent handling and stale-claim takeover
- `WorkLock` lease loss during handling
- Failed `fence_transaction` calls followed by transaction rollback or discard
- Successful lock reacquisition and a new fence before protected database writes
- Reestablishment of every required external protection before external mutation
- Skipped, polling, and maintenance transitions
- Explicit error transitions and persisted resume behavior

If you cannot answer these questions and test the applicable paths, the transition's recovery contract is incomplete.
