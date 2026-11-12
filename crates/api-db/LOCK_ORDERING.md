# Machine Write Locks

This reference describes network configuration persistence, Discovery, IPv6
loopback backfill, DPF flags, and DPU reprovisioning requests. Each caller retains
its locks until its transaction commits or rolls back. These protocols do not
establish a global order for all Machine lifecycle operations.

## Network Configuration Persistence

`machine::try_update_network_config`, `try_update_network_config_for_created`,
and the private `try_update_network_config_unless_force_deleting` share
`try_update_network_config_inner`. They borrow the caller's transaction.

The first statement conditionally updates the target's configuration and
version. A missing target, changed version, or failed caller-specific creation
or deletion guard returns `NotApplied`. The second statement materializes the
other group rows, locks them with `FOR NO KEY UPDATE` in C collation order, and
updates only those captured IDs. C collation matches native Machine ID order,
including `machine::find` with `for_update` used by
`handlers::dpu::lock_attached_dpus` for reprovisioning `Mode::Set`. The target
is locked first; only the other members follow ID order. A host-target writer
therefore holds the host before DPUs. This is not a global host/DPU lock order;
DPU-target `NOWAIT` breaks the opposing target-first cycle.

Host targets wait for the group rows. DPU targets use `NOWAIT`: a host writer
can already hold the host while waiting for the target DPU. Waiting for that
host while retaining the DPU would complete a cycle. SQLSTATE `55P03` remains
a database error, distinct from a version rejection. On any error, the caller
must roll back its entire transaction, including its earlier writes. Successful
callers commit target and group writes together.

Membership comes from `machine_group_member_ids`, which follows
`attached_dpu_machine_id` regardless of interface type, including `Bmc`. The
statement locks the exact Machine rows it updates, but does not lock the
interface graph. Callers that require fixed membership must retain their own
interface locks. Ordering adds a sort and materialization proportional to the
selected group. `machine::find(for_update)` also orders its selected rows with
C collation; callers select one ID or an explicit list, and an index in another
collation need not provide that order. No performance result is claimed.

The DPU-target writers are `PreparedDpuLoopback::attach` (Discovery and Site
Explorer machine creation) and IPv6 backfill. Host-target writers include
controller network/reset handling, primary-interface changes, VPC peering,
force-delete preparation, site-prefix changes, admin-interface repair, and
quarantine updates. These callers share the persistence helper; this reference
does not extend the nonblocking policy to host targets.

## Discovery

`handlers::machine_discovery::discover_machine` owns the outer transaction for
interface association, topology, loopback allocation, and initial host state.
Its admin segment advisories precede interface row locks. Host interface writes
precede `PreparedDpuLoopback::attach`, whose missing-loopback path already tries
the group through `machine::try_lock_group_for_update`. A rejected try retains
the existing `FailedPrecondition` retry response. Any database `55P03` returned
by attachment rolls back the entire outer transaction and returns `Unavailable`.
A membership change after initial admission can make the final group write
encounter a newly busy member.

An unowned link can reuse an existing predicted host. Association takes only a
foreign-key `KEY SHARE` lock, compatible with the `FOR NO KEY UPDATE` lock
from `machine::update_reboot_requested_time`. Complete loopbacks can also skip group admission. Before
`machine::update_state`, Discovery therefore tries the selected host with
`try_lock_by_id_for_no_key_update`, then tries its history-retention advisory
with `try_lock_machine_state_history`. A missing or busy host, or a busy history
advisory, rolls back the complete attempt and returns `Unavailable`. Both
acquisitions are nonblocking because Discovery already holds interface and
possibly Machine writes.

The history advisory uses the same Machine ID and table identity as the
`machine_state_history_keep_limit` trigger. Successful admission retains its
locks through the Discovery commit. It does not change Discovery's identity
checks or the DPU set selected by `update_state`. An earlier host foreign-key
check can still wait behind `FOR UPDATE` before these try-locks are reached.
`update_state` also takes blocking row/history dependencies for attached DPUs.
The retained standalone-DPU force-delete/history cycle belongs to
[force-delete preparation](https://github.com/dsx-ai-factory/infra-controller/issues/8909);
other Discovery/destructive-state interactions remain in
[the separate lifecycle follow-up](https://github.com/dsx-ai-factory/infra-controller/issues/8788).
These selected host checks do not establish nonblocking admission for every
Discovery dependency.

Backend preparation cancellation runs after the entity transaction ends.
Admin address repair starts in a separate transaction after commit; its failure
does not reject an already committed Discovery. Client retries start a fresh
request and can collide again; there is no fairness guarantee.

## IPv6 Backfill and Pool Growth

`machine::update_dpu_loopback_ips_v6` scans candidate IDs, then opens a fresh
transaction for each of at most three attempts per DPU. Each attempt reloads
configuration and version. A missing row, an existing IPv6 loopback, or a
visible committed `ForceDeletion` needs no reservation.

Otherwise, the attempt reserves an address before trying the eligible target
with `FOR NO KEY UPDATE NOWAIT`. The lock query excludes `ForceDeletion`, and
the conditional configuration write checks deletion and version again. A
rejected compare-and-swap rolls back its reservation. Target or group `55P03`
rolls back the entire failed attempt before another query or retry.

After the three attempts, a failed DPU does not prevent later independent DPUs
from committing. The pass returns the last attempt's error for the first DPU
that exhausted its attempts. A later version rejection can replace that DPU's
earlier lock error. Other database errors propagate immediately, preserving
earlier DPU commits. A later invocation reloads everything in a new pass.
The attempts run without backoff and do not wait for busy Machine rows; pool reservation still uses
the pool allocator's existing waits and transaction policy. Discovery can hold
the group while waiting for a pool reservation. Target `NOWAIT` prevents
backfill from retaining that reservation while waiting for Discovery's group.

`handlers::resource_pool::grow` commits pool definitions and bindings before
starting this backfill. A returned `55P03` becomes
`CarbideError::Ipv6LoopbackBackfillContended`, returning `Unavailable` with
`NICO-API-503` and explicit text that pool growth committed. The original
annotated database error remains its source for server diagnostics. Callers
must inspect pool configuration and remaining IPv6 assignments before retrying
the growth request. Capacity exhaustion returns `ResourceExhausted`, exhausted
version conflicts or a vanished pool return `FailedPrecondition`, and other
database failures return `Internal`. These failures also retain committed pool
growth and earlier DPU updates.

API startup calls the same backfill from `setup.rs`, logs a failure, and continues
without scheduling a later pass. A busy DPU can therefore remain without an
IPv6 loopback until another startup, pool-growth request, or DPU rediscovery.
This is bounded best-effort reconciliation, without eventual-completion or
fairness guarantees.

## DPF Flags and DPU Reprovisioning Requests

`handlers::dpf::modify_dpf_state` takes a strong host `FOR UPDATE` lock before
reading eligibility. New interface owner references require a conflicting
foreign-key lock. Existing interfaces can change attachment or classification
without updating the host, so `try_lock_host_interfaces` tries every existing
host interface, including `Bmc`, in interface ID order with `FOR NO KEY UPDATE`.
This permits foreign-key readers inserting DHCP/address records while excluding
changes to the selected interfaces. A moved or removed selection restarts
admission. DPF then reloads attached DPUs, deduplicates them, and tries their
rows with `FOR NO KEY UPDATE` in native Machine ID order. Host and DPU enabled
flags commit together; `used_for_ingestion` is preserved, and a committed host
ingestion marker prevents disabling DPF.

A rejected dependency releases the entire outer transaction, including every
acquired host, interface, and DPU lock. `wait_for_host_interface` or
`wait_for_machine_no_key_update` waits for that one dependency in an isolated
transaction and rolls it back before returning. A missing dependency returns
promptly. The handler starts a fresh transaction and reloads its complete
selection and authorization. Retaining earlier locks while waiting can cycle
with `handlers::dpu::record_dpu_network_status`, queued controller machine
writes, or interface updates in Discovery.

DPF database preparation has a 300-second cap from the listener's receipt
stamp, including time in earlier authorization/admission layers. Direct handler
calls fall back to handler entry. Valid shorter `grpc-timeout` metadata reduces
the cap. Pool acquisition, all retries and waits, and commit use this one
deadline. Expiry returns `DeadlineExceeded` with `NICO-API-504`, drops work, and
queues SQLx rollback for an open transaction. PostgreSQL can finish that cleanup
later, and a commit already in flight may have succeeded. Inspect current host
and attached-DPU flags before retrying an ambiguous result. The deadline does
not extend to other DPF endpoints or public reprovisioning requests.

Public Set retains its `machine::find(for_update)` admission in C collation
order and its migration validation. Host Clear and controller writes retain their
primary-first snapshot order, including approval and queued request starts.
DPF and Restart release their complete transaction before waiting for a DPU, so
these writers cannot block behind an earlier DPU held by either admission loop.
Full Set/Clear admission and repeated-interface semantics remain assigned to [the lifecycle follow-up](https://github.com/dsx-ai-factory/infra-controller/issues/8788).

Restart captures the initial distinct attached-DPU and pending-ID sets, then
admits the host, every host interface, and all current DPUs with the same
reject, release, wait, and reload protocol as DPF. Its input transaction must
contain only reads because a retry rolls it back and replaces it. Before
persistence, it reloads aggregate host health and current requests. Changed
attachments or pending membership return `FailedPrecondition`; missing
host-update authorization returns `InvalidArgument`. A selected request replaced
with another pending request remains selected; Restart merges fields into that
current request. Machine/interface locks preserve host-owned dependencies;
rack health reports included in aggregate health retain their existing
point-in-time read semantics. These locks do not serialize rack-report updates.

`machine::restart_dpu_reprovisioning` independently locks pending rows in C
order with `FOR NO KEY UPDATE`, deduplicates input, and rejects empty, missing,
or cleared selection before changing any survivor. It checks complete update
cardinality, and the caller retains the transaction through commit. Set uses
its separate ordered admission; the two persistence paths acquire DPU rows in
the same order. These operations acquire no history locks.

Set/Clear retain their existing open transaction and admin-segment coordination
locks across the bounded Kubernetes migration check and request writes.
Releasing those locks during that read would change migration coordination.
DPF and Restart admission can cause Discovery to return its existing transient
`Unavailable` response. Restart does not enforce a total deadline across
admission waits and retries. Full Set/Clear interface admission and remaining endpoint deadlines
belong to [the lifecycle follow-up](https://github.com/dsx-ai-factory/infra-controller/issues/8788).
