# Machine Lock Ordering

This reference records the transaction boundaries for
[Machine write ordering](https://github.com/dsx-ai-factory/infra-controller/issues/5333).
Keep each operation's authorization checks, lock order, and rollback boundary together.

## Lock Scope

Machine state history uses the transaction advisory acquired by the
[history retention trigger][retention]. It is keyed by Machine ID and history
table identity. Ordinary state persistence takes that advisory before changing
the corresponding Machine row. A guarded version update uses compare-and-swap (CAS).

`FOR UPDATE` conflicts with foreign-key `KEY SHARE` locks. `FOR NO KEY UPDATE`
permits compatible foreign-key readers and matches the non-key Machine UPDATEs
described below. `SKIP LOCKED`, `NOWAIT`, and try-advisory calls avoid waiting
for the conflicting row or advisory owner. They do not release earlier locks.
The transaction or acquiring savepoint determines their release boundary.

Admission means the resource acquisition and current-input checks required
before an operation's dependent writes. A cohort is that operation's selected
set of DPU IDs; its interface and membership rules are stated below.
A write to a row already held at the required strength adds no new row-lock
wait. Requests for another row or a history advisory can still wait, so an
operation's call order need not be its order of fresh lock acquisition.

Native Machine ID order determines acquisition in the explicitly sorted loops.
Public Set first acquires DPU rows through `lock_attached_dpus`; its later
request writes follow snapshot order on rows already locked. This prelock does
not admit existing host interfaces against concurrent Discovery. Duplicate
snapshot entries can also make Set or Clear reject the request; those retained
gaps are tracked by [Complete Machine Lifecycle Admission And Device Result Writes][remaining-admission].
The [Machine DB implementation][machine-db] uses explicit `COLLATE "C"` in
locking `find`, `find_dpus_by_host_machine_id`,
`try_lock_managed_host_transition_dependencies`, `restart_dpu_reprovisioning`,
`try_update_network_config_inner`, `find_network_group_peer_ids`, and
`find_dpu_reprovision_completion_inputs`. The last two are ordered readers,
not lock-acquisition statements.
Other selectors retain their own ordering contracts. In particular,
`find_machine_ids` does not add C collation. Sorting an SQL array argument does
not order the rows acquired by its UPDATE.

## Shared Database Operations

The table distinguishes borrowed transactions from operations that create savepoints.
Its DB symbols are defined in the [Machine DB implementation][machine-db]:

| Operation | Transaction and Release | Acquisition and Protected Invariant | Conflicting Writer |
| --- | --- | --- | --- |
| Guarded host state persistence (`try_update_controller_state`) | Borrows caller transaction and creates a savepoint. Rejection rolls back that savepoint. Success retains locks until outer completion | History advisory before non-key host UPDATE. Checks host version and captured maintenance request. Propagates the supplied state/version to attached DPUs in native ID order | Direct `advance` and eligible independent NIC report persistence write the same state/history |
| Single-Machine advance (`advance`) | Borrows caller transaction without a savepoint. Caller commits or rolls back | History advisory before one non-key Machine UPDATE. Records supplied state/version in both places. It has no expected-version predicate or host-wide propagation | Guarded controller persistence and other `advance` callers for that Machine |
| Managed-host transition admission (`try_prepare_managed_host_transition`) | Borrows the completion attempt. Every result retains any acquired locks. Caller rolls back the whole attempt on rejection | Try host history, host `FOR UPDATE`, existing interfaces including BMC, then each DPU's history and `FOR UPDATE` in ID order. Checks host token and exact captured non-BMC DPU set | Public DPF writes host before DPUs. Discovery can change an existing interface's attachment or classification without updating the host |
| NIC firmware completion preparation (`prepare_nic_firmware_completion`) | Creates a savepoint inside caller transaction. Rejecting rolls back its acquisitions. Success retains them for caller CAS | Required named Instance first, host history and `FOR UPDATE`, then nonblocking interfaces and DPU history/row locks. Checks purpose owner, attachment, token, and recorded failures | Instance configuration and release take Instance before Machine. Public DPF and attachment writers can own later dependencies |
| NIC firmware owner release (`prepare_nic_firmware_owner_release`) | Borrows caller transaction without a savepoint. Rejection also retains acquisitions and requires caller rollback | Caller first restores lockdown. Helper locks deleted original Instance, host history/row, and nonblocking interface/DPU dependencies. Checks current token and failures before normal Instance teardown | Instance deletion or replacement and concurrent host/DPU failure or attachment updates |

`try_prepare_managed_host_transition` delegates interface/cohort acquisition to
private `try_lock_managed_host_transition_dependencies`. The blocking history
helper is private `lock_machine_state_history`; nonblocking paths call
`try_lock_machine_state_history`. Transition admission writes no history or
state. Its existing-interface locks protect attachment and classification while the caller checks domain data.
The host row also blocks new host foreign-key references at `FOR UPDATE` strength.
NIC preparation retains its own Instance and failure policy; using its lock
sequence does not replace those checks.

## Controller and API Transaction Owners

The table names who starts each transaction and when its locks are released:

| Owner and Creator | Acquisition | Protected Invariant and Competing Writer | Completion or Rejection |
| --- | --- | --- | --- |
| [DPU heartbeat timeout][heartbeat] creates a handler transaction | Try host `FOR NO KEY UPDATE`, then selected DPU `FOR NO KEY UPDATE`; compatible `KEY SHARE` readers can remain | Public DPF writes host before DPUs. Reread host token and timeout observation before overwriting health | Busy/missing locks roll back and return Wait before state handling; changed token or no-longer-qualifying timeout invalidates. Successful health and outcome persistence use the returned transaction |
| [Terminal RebootHost][reboot-host] creates each final database attempt | Transition admission after accepted reboot, then `find_dpu_reprovision_completion_inputs` reads current host state/token, host-request presence and complete optional requests for distinct DPUs | Public DPF can own host first. Request writes need not change the host token, so compare each full `Option<ReprovisionRequest>`, including idle siblings. Public Clear rejects a started request | Busy retries database completion with accepted result retained. Stale rolls back and invalidates. Returns transaction with next state |
| [Reset start][reset] creates a handler transaction | Blocking host history, then host non-key lock | Scout report can update the same waiting firmware state. Public Reset Set/Clear can replace or cancel the pending request | Stamp only the captured unstarted typed request and token. Caller retains transaction through CAS or abandons it on rejection |
| [Scout firmware report][scout] creates each API attempt | Blocking host history, then `FOR NO KEY UPDATE NOWAIT`, then separate full host read | Reset can preempt the report's task. Invalid reports can also meet a host-owning Ready or credential handler before state validation | On SQLSTATE `55P03` roll back everything before isolated non-key host wait. Retry fresh state/task checks. Commit before enqueueing work |
| [NIC completion and failed-owner handoff][nic-handler] create handler transactions | Call the appropriate NIC preparation operation above before returning | Normal teardown must retain the original deleted Instance and must not skip safe firmware/lockdown checks | Framework applies batch and guarded state transition on returned transaction. Any required preparation rejection invalidates the iteration |
| [Public reprovisioning Set/Clear][public-dpu] create API transactions | Admin admission and segment advisories first. `lock_attached_dpus` prelocks the complete DPU set for Set/migration; Set then writes already-locked rows in snapshot order. Ordinary Clear acquires rows through native-sorted writes | Site Explorer changes attachments. Restart independently updates request JSON. Set does not freeze existing attachment changes; duplicate snapshot entries can make Set/Clear fail | Retain admin locks across Kubernetes read, reload after prelock, and commit request writes. Clear requires an unstarted request |
| [Public Restart][restart-admission] starts an API transaction and replaces rejected attempts | `admit_dpu_reprovisioning_restart`: checked strong host lock, try existing interfaces, then try every current DPU with `FOR NO KEY UPDATE` in native ID order | Reread and validate the current host, exact distinct attached cohort and eligible pending-ID set. Set/Clear or Discovery can change these while an earlier attempt waits | Roll back the whole rejected attempt before an isolated resource wait and fresh retry. DB `restart_dpu_reprovisioning` merges current JSON and checks every returned ID. Caller commits |
| [Public DPF update][dpf] creates an API transaction | `lock_host_for_dpf_update` checks host presence under a strong narrow lock, nonblocking existing host interfaces, fresh snapshot, then native-sorted distinct DPU non-key tries before flag writes | Ingestion marker writes the host. Fresh snapshot authorizes disable and captures the admitted DPU cohort. Strong host ownership fences new owner-FK references; interface locks fence existing attachment changes. Reports can own a DPU before updating an admitted interface | Rejected interface or DPU admission rolls back the whole attempt before an isolated wait or fresh retry. Denial writes no flags. Success commits host and distinct admitted DPU flags together under one preparation deadline |
| [Force-delete Machine phase][force-delete] creates each API attempt | Routing coordination, try strong host row, try host history and interfaces, fresh authorization/DPU reads, try each selected state DPU's history, then its non-key row. Before an actual Admin network write, `find_network_group_peer_ids` reads the written group peers outside that cohort for non-key tries | Normal controller and NIC report persistence can own history. Current type/Instance consent and DPU membership are read after admission. A report can own D before updating an admitted interface. The written Admin group includes BMC-only peers that are not state DPUs | Rejected host/history/interface/DPU/peer admission releases routing and the whole phase before an isolated wait or fresh retry under one preparation deadline. Later Instance/external cleanup acts on captured admitted targets after commit |

Restart's interface wait uses `wait_for_managed_host_transition_lock`; a busy
DPU uses `wait_for_machine_nonkey_update`. Both follow full attempt rollback.
The [DB Restart merge][restart] remains two statements: C-ordered non-NULL
request selection with `FOR NO KEY UPDATE`, then the current-JSON merge.
It preserves `Some` to `Some` request replacement for the same selected IDs;
it does not compare the full captured request or add a request epoch.

For an orphan DPU, `prepare_force_delete_machine_phase` tries a strong DPU row
lock, then its history advisory. `try_lock_dpu_attachment_interfaces` then
nonblockingly admits every existing interface with that DPU attachment,
including BMC, hostless and self-owned references. A type-only change on an
existing reference can create a qualifying host link without changing the DPU
FK; strong DPU ownership alone does not fence that producer. The interface
locks retain its classification and ownership through the phase commit.

The phase rereads current host association and DPU instance type after this
admission. If a host appeared, it rolls back the whole DPU/history/interface
prefix and routes from the original query through host instance/type consent.
Contention releases routing and the whole phase before one isolated resource
wait; changed membership releases the attempt and retries. Strong DPU ownership
fences new attachment FKs, while admitted interfaces fence existing-reference
changes, only through Machine-phase commit. Later Instance and external cleanup
do not retain that transaction.

Attached-host force-delete tries each selected state DPU's history before its non-key row.
A failed try rolls back the whole attempt before an isolated wait or fresh retry.
After all selected DPU locks are held, it rereads their state versions before
deciding which DPUs to advance; host and interface ownership keep membership stable.

Force-delete takes site-wide routing coordination before trying the host. A
busy host releases the whole attempt, including routing, before an isolated
wait. The admitted phase still retains that shared coordination through commit,
so unrelated routing-policy writers or force-delete requests can wait for it;
this protocol does not provide site-wide parallel execution.

Heartbeat lock rejection returns Wait before state-specific handling. A changed
host token or timeout observation that no longer qualifies instead returns IterationInvalidated
after rolling back. The next iteration reevaluates the timeout candidate. Terminal retries retain
accepted results only while admission remains current in the same invocation.
A stale token, changed request, or disappearing interface invalidates the
iteration; later handling can dispatch the device action again. Process loss
and claim expiry likewise do not provide exactly-once device execution.

## Network Groups and Backfill

[Network configuration persistence][group] (`try_update_network_config`, private
`try_update_network_config_inner`) borrows its caller's transaction.
Its first statement conditionally updates the target. Its second statement
materializes group IDs, locks them in C order, and updates only those captured
IDs. Host targets wait for non-key group locks. DPU targets use NOWAIT because
they already own their target before reaching the host or sibling DPUs.
This coordinates host-first force-delete and ordered public Set against
DPU-target discovery or backfill without waiting across the opposing row order.
Group membership includes BMC links. It is not the transition helper's non-BMC
DPU set, and the statement does not freeze the interface graph.

[Discovery][discovery] owns its outer interface, topology, and allocation
transaction. Group SQLSTATE `55P03` rolls back that entire transaction and returns
Unavailable. An unowned link can reuse an existing predicted host through an
association whose foreign-key check takes only `KEY SHARE`. That permits a
controller's non-key receipt UPDATE; complete loopbacks can also skip the group
write. Before setting host state, this path tries the selected host
`FOR NO KEY UPDATE`, then its history advisory. Either failed try rolls back the
whole discovery transaction and returns Unavailable before external post-commit
work. Concurrent sibling discoveries can both reject and repeat a collision
under the client's fixed retry backoff.

[IPv6 loopback backfill][backfill] reloads config and reserves an address in a
fresh transaction for each DPU attempt. A visible committed ForceDeletion or
missing row needs no new reservation: `is_force_deleting_or_deleted` reads it
without a Machine lock before allocation. Otherwise
`update_dpu_loopback_ips_v6` reserves the pool address before trying the eligible
target `FOR NO KEY UPDATE NOWAIT` lock. That lock query also excludes
ForceDeletion; a concurrent transition still reaches the guarded CAS check.
Private `try_update_network_config_unless_force_deleting` then applies target
CAS before the NOWAIT group write. Its target predicate excludes force-deleting
rows. Up to three immediate fresh attempts cover target/group contention and
CAS rejection; this policy does not wait for a busy target or group. Each
target/group SQLSTATE `55P03` attempt rolls back before another query. Exhaustion continues other DPUs, preserving
completed rows. For exhausted retries, the pass retains the last attempt error
of the first failing DPU in scan order. A later CAS failure can replace that
DPU's earlier busy error. Other SQL errors propagate immediately. A later
invocation starts a new pass.
[Pool growth][pool-growth] commits pool rows before invoking this live backfill,
so its backfill error can follow a committed pool change. An exhausted SQLSTATE
`55P03` remains the original database error in the backfill. The pool-growth API
wraps it as typed `Unavailable` with `NICO-API-503`, fixed partial-commit text,
and the annotated database cause retained for diagnostics. Inspect the current
pool configuration and remaining IPv6 assignments before retrying. Other
backfill errors retain their existing conversion; earlier DPU commits remain.

Site Explorer calls DPU-target group persistence only while inserting its first
host and new DPUs in one transaction. Later siblings reuse that transaction's
host ownership. Existing-host repair and existing-DPU branches return before
this call, so they do not introduce an existing-host group opponent here.
Database errors propagate through the creation transaction and are reported as
a failed exploration; a later exploration pass may retry.

[Lock error classification][lock-error] (`DatabaseError::is_lock_not_available`)
recognizes annotated SQLSTATE `55P03` without replacing the error. Missing/changed network targets remain
`NotApplied`; other SQL failures remain errors. Public quarantine Set and
[Clear][quarantine] require `HostMachineId`, so those public calls use the
host-target group policy. DB `clear_quarantine_state` accepts generic
`MachineId`; its signature does not impose that public host-only restriction.

## Connections, Waits, and Source Estimates

The caller rolls back its whole rejected attempt before
[waiting for one transition resource][transition-wait] or
[waiting for one non-key Machine row][nonkey-wait]. Each helper checks out one
pool connection, begins a separate transaction, acquires only the reported
resource, and rolls it back before returning. The connection remains occupied
through that wait. Retry then checks out a connection for a fresh attempt.
These helpers do not add a spare connection reserve for unrelated hosts.
Borrowed operations and savepoints reuse the caller's connection without an
additional pool checkout.
Releasing the rejected attempt's unrelated rows permits independent work only
when that work needs neither the reported resource nor shared coordination,
and a pool connection is available. The isolated wait still consumes a
connection; neither fresh retry nor row ordering guarantees independent progress.

Controller completion waits share the existing object-handling deadline.
Public Scout reports, DPF changes, and the force-delete Machine phase use one
absolute preparation deadline captured at handler entry: at most 300 seconds,
reduced by a valid shorter `grpc-timeout`. The header accepts one to eight
decimal digits and `H`, `M`, `S`, `m`, `u`, or `n`; zero expires immediately.
Missing or malformed metadata keeps the cap. Pool acquisition, transactions,
retries, isolated waits, and commit share this budget; retries do not restart it.
Scout enqueue and force-delete external cleanup follow preparation whose commit
was observed successfully. DPF has no separate post-commit enqueue in this handler.
Restart does not use this preparation deadline; its admission loop adds no new
absolute budget. Bounding Set and Restart admission retries and their waits is
part of the remaining [Machine lifecycle admission work][remaining-admission].

Expiry drops the Rust operation and SQLx queues rollback. It does not guarantee
immediate backend cancellation or connection return, and an in-flight COMMIT
can remain ambiguous. A server-committed Scout result can therefore miss its
immediate wakeup; the periodic controller enqueue can later observe the stored
result. ForceDeletion can commit before the timeout prevents external cleanup,
and its controller branch does not perform that cleanup; another API/operator
request can be needed. A timeout does not discard a successful commit Result
already returned to the work branch. Deadline errors use the standard typed
`CarbideError` conversion, retaining `DeadlineExceeded`, the message
`machine database preparation deadline exceeded`, `NICO-API-504` metadata, and
the in-process typed source and conversion location.
The relative header is measured from handler entry, not
original request arrival. Blocked waits can still consume pool capacity after
unrelated row locks have been released. Acquisition order implies no fairness.

Controller CAS and `advance` run for their corresponding state writes.
On successful persistence, locks remain through the caller's commit or rollback. Terminal and NIC
admission run at accepted-result completion or failed-owner handoff, outside
the device call; reset admission requires a selected unstarted request, and
heartbeat locks require a selected timeout candidate. Their successful locks
remain on the returned transaction through guarded state/outcome persistence.

Set/Clear, Restart, DPF, Scout reports and force-delete run on explicit API
requests. Set/Clear retain admin coordination across the bounded Kubernetes
label read and request commit. Other admitted database work retains its locks
through its stated transaction or phase, not the later external work.
Discovery's group write runs only when it fills a required loopback; its late
state admission runs when synchronizing a newly selected predicted-host link.
Network persistence also runs for its existing config-changing callers.
Setup and IPv6 pool growth invoke backfill, which attempts eligible DPUs
separately and continues after an exhausted DPU. Rejected attempts release
unrelated rows before isolated waits, but shared coordination and finite pool
capacity can still limit other operations. Invocation conditions and transaction
boundaries explain potential frequency and hold duration; neither has been measured.

The following source estimates exclude transaction control, trigger SQL,
caller snapshots, outcome persistence, and unnamed side writes:

| Operation | Top-Level SQL | Retained Resources and Invocation |
| --- | --- | --- |
| Guarded controller CAS | `3 + 2d` | `d + 1` Machine/history pairs during transition persistence |
| Single-Machine advance | `2` | One Machine/history pair per explicit advance |
| Transition admission | `6 + 2d` | `d + 1` Machine/history pairs and `f` interfaces per selected completion attempt |
| NIC completion preparation | `7 + 2d`, plus one for named Instance | Same dependencies and optional Instance per completion |
| NIC owner-release preparation | `7 + 2d` | Deleted Instance plus host and DPU dependencies per safe failed-owner handoff |
| Isolated wait | `1` plus separate begin/rollback | One checked-out connection and one resource per wait |
| Heartbeat timeout | `2` tries, `2` reads, `1` health UPDATE | Host and selected DPU for a timeout candidate |
| Reset start or Scout precheck | `3` each | One history advisory and host row per admitted attempt |
| DB Restart merge; Set prelock/request writes; ordinary Clear; DPF flag writes | `2`; `1 + s`; up to `s`; `1 + d` | Restart API admission is additional. Set/Clear use snapshot entries and retain admin coordination; DPF writes distinct admitted DPU IDs |
| DPF host/interface/DPU admission | `3 + d` | Checked strong host row, two interface queries and one non-key try per distinct DPU; snapshot enrichment is additional |
| Terminal completion input projection | `2` | Reads host and distinct attached DPU requests after transition admission; no new locks |
| Network target and group update | `2` | Up to `g` Machine rows per successful group update |
| Discovery predicted-host state admission | `2` tries | Selected host non-key row and history advisory; outer interface, topology, allocation, and any group work are additional |
| Backfill deletion read, target lock, CAS and group update | `4` | Eligible successful path; address reservation and config reads are additional. Visible deletion skips reservation and remaining writes; target/group use NOWAIT |
| Force-delete host/history/interface/state-DPU admission | `4 + 2d` | Strong host try, history try, two interface queries, then history and non-key row tries per distinct state DPU; routing, current reads and later writes are additional |
| Force-delete written Admin peer admission | `1 + e`, only when an Admin network write is needed | `find_network_group_peer_ids` plus non-key tries for distinct peers not already admitted as state DPUs; includes BMC-only peers |
| Orphan force-delete D/history/interface admission | `4` | Strong D try, history try and two `try_lock_dpu_attachment_interfaces` selects. Current-host/type reads, routing and subsequent writes are additional; a missing-interface classification adds one read |

Here `d` counts distinct attached non-BMC DPU IDs, `s` counts DPU snapshot
entries (which can repeat an ID), `f` counts existing host interfaces, `g` counts
written group members, and `e` counts distinct written Admin peers outside the
admitted state-DPU cohort. DPF deduplicates its sorted IDs
before both tries and writes. Snapshot duplicates are not charged as distinct
Machine resources. Busy paths can add existence
reads and repeated admission. Counts describe the primitives, not whole API
requests or controller iterations. Timing, contention frequency, retry counts,
and pool occupancy have not been measured by this table.

## Queue and State Boundaries

[Machine controller IO][machine-io] uses `HostMachineId` and a shared PostgreSQL
queue. [Queue acquisition][queue] reserves an unclaimed or expired entry across
processors. [Object handling][processor] shares a deadline across claim,
dispatch, and handling. Reservation expiry is three times the configured object
budget; the claim supplies no database fencing token to the state CAS.

The processor commits its snapshot transaction before invoking the handler.
It applies the original deferred batch before guarded state and outcome
persistence. Wait can update the host outcome without writing state history.
[Deferred Machine operations][batch] do not themselves provide admission for
Ready reprovision, restart, Assigned Ready, custom-PXE entry, Instance cleanup,
firmware request reset, or shared power handlers.

The common Ready scout clear still updates the host before framework history.
[Host UEFI rotation][host-uefi] records credential promotion or quarantine and
can clear the host force before returning Ready on that transaction.
[Settled NIC lockdown][lockdown] can also clear the host force before framework
history, after its per-card credential and DPA state work. DPA card controllers
write DPA state rather than host Machine history. Host UEFI and NIC-lockdown
Machine iterations share the host queue reservation. A fresh
[independent NIC result][nic-report] instead requires
`NicFirmwareUpdating` and a valid expected token, so it does not supply a fresh
Ready, Host UEFI, or NIC-lockdown state writer. Scout takes history before state
validation but releases it on a busy host before waiting. Force-delete tries
history while holding the host and backs out on contention. Discovery's late
state path tries the selected host's non-key row before trying history; either
rejection rolls back its whole outer transaction. These specific protocols
describe the reachable competitors; they are not a universal history-first
ordering rule.

The remaining [Machine lifecycle admission and device-result work][remaining-admission]
retains public Set/cohort and distinct-ID checks, Set/Restart request budgets,
per-device BMC endpoint acknowledgement, DPU UEFI attempted-stage/result checks,
and typed verifier `Wait` records with earlier queued writes. Its full criteria
also cover the related Admin-normalization and Discovery boundaries. These are
remaining requirements, not guarantees supplied by the shared helpers above.

[retention]: migrations/20260708172302_squash_snapshot.sql
[restart]: src/machine.rs
[restart-admission]: ../api-core/src/handlers/dpu.rs
[group]: src/machine.rs
[heartbeat]: ../machine-controller/src/handler.rs
[reboot-host]: ../machine-controller/src/handler.rs
[reset]: src/machine.rs
[scout]: ../api-core/src/handlers/host_reprovisioning.rs
[nic-handler]: ../machine-controller/src/handler/nic_firmware.rs
[public-dpu]: ../api-core/src/handlers/dpu.rs
[dpf]: ../api-core/src/handlers/dpf.rs
[force-delete]: ../api-core/src/handlers/machine.rs
[discovery]: ../api-core/src/handlers/machine_discovery.rs
[backfill]: src/machine.rs
[pool-growth]: ../api-core/src/handlers/resource_pool.rs
[lock-error]: src/lib.rs
[quarantine]: ../api-core/src/handlers/machine_quarantine.rs
[transition-wait]: src/machine.rs
[nonkey-wait]: src/machine.rs
[machine-io]: ../machine-controller/src/io.rs
[queue]: ../state-controller/src/controller/db.rs
[processor]: ../state-controller/src/controller/processor.rs
[batch]: ../machine-controller/src/write_ops.rs
[host-uefi]: ../machine-controller/src/handler/host_uefi_rotation.rs
[lockdown]: ../machine-controller/src/handler/nic_lockdown_rotation.rs
[nic-report]: ../api-core/src/handlers/nic_firmware_operation.rs

[remaining-admission]: https://github.com/dsx-ai-factory/infra-controller/issues/8788

[machine-db]: src/machine.rs
