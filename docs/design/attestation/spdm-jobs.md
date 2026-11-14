# SPDM Attestation Jobs

**Status:** Implemented

An SPDM attestation job records one attempt to check a machine's selected
devices. The [profile design](attestation-profiles.md)
explains the default selection; manual triggers can override it with a regex.
The job gives that attempt its own identity, progress, and results.

## Job identity

When at least one device is selected, scheduling creates a UUID `job_id` and
one work record per device. All records in that job share the same `job_id`
and `started_at`. If nothing is selected, no job is created.

The records live in `spdm_machine_devices_attestation`, identified by
`(machine_id, device_id, job_id)`. There is no separate job table. Workers
read and update records for that exact job, so a late update from an earlier
attempt cannot overwrite a newer attempt's record.

Starting another job creates new records without cancelling or erasing the
earlier job. For example, jobs A and B can both contain results for the same
device. Finding A's timestamp proves A was recorded; it does not prove A is
the latest attempt.

## Progress, waivers, and failure

The SPDM controller collects and verifies evidence for each device, saving
progress so it can resume after a restart. Jobs can overlap, but only one job
can hold the evidence collection claim for a particular machine and device
at a time. Other jobs wait for that claim to be released.

A job's status combines its devices' effective results in this order:

1. **In progress** if any device has not reached a final state.
2. **Failed** if any device failed.
3. **Waived** if any device was waived.
4. **Passed** if every device passed.

`job waive` and `job fail` record the latest cancellation request for every
retained device in the job, including completed devices. Each call replaces
`cancellation_requested_at` and `cancellation_target` (`waive` or `fail`). Both
directions are allowed; the last committed request wins. Repeating a target
refreshes its request timestamp. Unknown jobs return not found, and invalid UUIDs
are rejected. Use these semantics after all Core replicas have been upgraded.

Unfinished device attestations remain in progress until the SPDM controller
finishes BMC cleanup and records a terminal result. If a device holds an evidence
collection claim, the controller releases it when the BMC confirms the task
stopped, returns HTTP 404 or 410 in `WaitForTaskCancellation`, or the cancellation
timeout expires. A missing task is treated as stopped without an explicit terminal
status; the timeout therefore does not guarantee that collection attempts cannot
overlap. Changing or repeating a request cannot postpone the original cleanup
deadline. The first unfinished request is recorded separately as
`cancellation_cleanup_started_at`; automatic cleanup also preserves its
`cleanup_started_at` in `WaitForTaskCancellation`. The earliest cleanup start
controls the timeout.

For terminal devices, status queries apply the latest target immediately without
rerunning attestation or rewriting stored results, evidence, or completion times.
A completed failure can therefore count as `Waived`, and a completed pass or
waiver can count as `Failed`. Without a request, the stored result determines the
status. Job and machine aggregates use this effective result with the precedence
above; unfinished devices still take precedence over terminal overrides.
`GetAttestation` returns the stored device state and latest cancellation target,
so the original result remains available as history.

The database retains the existing `cancelled_at` column for
`cancellation_requested_at`, and the RPC response retains its `cancelled_at`
timestamp field. The stored `Cancelled` JSON spelling and `cancelled` metric label
remain compatible; business models, API statuses, and CLI output use `Waived`
(`SPDM_ATT_WAIVED`). Protobuf status and selector numbers are unchanged, with
deprecated aliases for the old enum names. Legacy requests without a recorded
target are treated as waivers.

The managed-host recovery path reads the effective latest result for each
device selected by the current profile and BMC inventory. Waiving a completed
failed job can therefore let recovery resume ingestion without scheduling a new
attestation. A newer result for a selected device still takes precedence over
that device's result in an older waived job. Failing a completed job changes its
effective status but does not undo a machine lifecycle transition that has
already completed.

Once its tracked job is terminal, the managed-host controller rechecks the
selected device IDs. If that selection changed, including becoming empty, it
returns to triggering without modifying the old job or its cancellation request.
The next iteration schedules replacement work or skips an empty selection.

## Reading results and keeping history

The commands below belong to `nico-admin-cli attestation spdm`. They use the
[SPDM gRPC methods](https://github.com/dsx-ai-factory/infra-controller/blob/f63617df0786b86a6a56d5260eb35e3de16712f8/crates/rpc/proto/forge.proto#L1098).

### Start a run: `trigger`

`trigger` schedules attestation for one machine through
`TriggerMachineAttestation`. On success, it prints the job ID when devices
were scheduled, or `none` when no job was created. Keep that ID to inspect or
fail or waive the run.

By default, selection uses the machine's hardware-class profile. Supplying
`--attester-id-regex` replaces that selection for this request, including when
the profile is missing or its mode is `NONE`, without modifying the stored
profile. The pattern uses Rust regex syntax and searches within each reported
attester ID, case-sensitive by default: `GPU_` selects IDs containing that text;
`GPU_0$` selects IDs ending in `GPU_0`; `^HGX_IRoT_GPU_0$` selects exactly that ID.
Empty or invalid patterns are rejected before contacting the BMC. Device
enablement, SPDM type/version checks, and site-wide SPDM enablement still apply.

The trigger prints the applied regex for an override, the scheduled attester IDs
in ascending order, and the job ID. A matching override reports
`IndividualAttesterTriggerScheduled`. No eligible match creates no job:
`NoAttestersFound` means discovery found no eligible devices, while
`IndividualAttesterTriggerMatchedNothing` means eligible devices existed but
none matched the regex. Manual overrides apply no profile, so their scheduling
events report `profile_source=unprofiled` even when a profile exists. The distinct
manual outcomes separate successful selection and unmatched regexes from
profile-driven `Scheduled` and `PolicyMatchedNothing` attempts. Inspect the
targeted run through its job ID; machine assessment still uses the current
profile's selection.

### Inspect, fail, or waive a run: `job`

The `job` group operates on individual runs:

| Subcommand | Purpose | gRPC method |
| --- | --- | --- |
| `list` | Find retained jobs by status | `ListAttestation` |
| `get` | Read one job's device details | `GetAttestation` |
| `waive` | Waive pending or completed device results | `WaiveAttestation` |
| `fail` | Fail pending or completed device results | `FailAttestation` |

`list` requires `--selector` with `in-progress`, `failed`, `waived`, or
`passed`. `get`, `waive`, and `fail` take a job ID. Both `list` and `get` print JSON;
`get` returns the details from `SpdmGetAttestationResponse`. Reading an unknown
or removed job returns not found. A successful `waive` or `fail` acknowledges the
request; the controller performs the BMC cleanup as described above.

For a stuck run, fail its original job and inspect it until cleanup completes:

```bash
nico-admin-cli attestation spdm job fail 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation spdm job get 12345678-1234-5678-90ab-cdef01234567
```

If the selected devices have not changed, start another run with `trigger`
after fixing the underlying problem. Triggering another run alone does not
replace the job the managed-host controller is waiting for.

### Check machines: `machines`

`machines list` calls `ListMachineAttestationStatuses`. Without
`--machine-id`, it predicts fleet statuses from each machine's latest retained
controller-triggered job, ignoring external jobs and making no BMC calls.
Machines without a controller job have unknown status.
The default includes every machine type. Use `--machine-type host` to select
discovered hosts, excluding predicted hosts, or `--machine-type dpu` to select
DPUs. Either filter can be combined with a status flag such as `--failed`;
machines without a retained controller job remain unknown when no status flag
is supplied. `--machine-type` cannot be combined with `--machine-id`.

With `--machine-id`, it uses the current profile and, when needed, live BMC
inventory to select devices. It checks each selected device's latest result
across all job origins.
Missing results mean in progress; an empty or disabled selection has no
verdict. This assessment does not create or change jobs. A historical job's
result can therefore differ from the machine's assessment.

### Retained history

Older runs remain visible through `job list` and `job get` until history
cleanup removes them. Cleanup runs when a device records completion and
removes whole eligible jobs. It keeps the ten newest completed jobs per
machine, unfinished jobs, jobs holding evidence collection claims, and jobs
containing a device's latest attestation record. Newest means `started_at`
descending, then `job_id` descending.
The job being updated is also kept until a later cleanup, so ten is not a
strict limit on retained jobs.

Implementation: [scheduling](https://github.com/dsx-ai-factory/infra-controller/blob/f63617df0786b86a6a56d5260eb35e3de16712f8/crates/machine-controller/src/handler/attestation.rs),
[job reads and status](https://github.com/dsx-ai-factory/infra-controller/blob/f63617df0786b86a6a56d5260eb35e3de16712f8/crates/api-db/src/attestation/spdm.rs),
[worker](https://github.com/dsx-ai-factory/infra-controller/blob/f63617df0786b86a6a56d5260eb35e3de16712f8/crates/spdm-controller/src/handler.rs),
and [storage and cleanup](https://github.com/dsx-ai-factory/infra-controller/blob/f63617df0786b86a6a56d5260eb35e3de16712f8/crates/api-db/migrations/20260917113009_spdm_job_id.sql).
