# SPDM Attestation Jobs

**Status:** Implemented

An SPDM attestation job records one attempt to check a machine's selected
devices. The [profile design](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/design/attestation/attestation-profiles.md)
explains how those devices are selected. The job gives that attempt its own
identity, progress, and results.

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

## Progress and cancellation

The SPDM controller collects and verifies evidence for each device, saving
progress so it can resume after a restart. Jobs can overlap, but only one job
can hold the evidence collection claim for a particular machine and device
at a time. Other jobs wait for that claim to be released.

A job's status combines its device states in this order:

1. **In progress** if any device has not reached a final state.
2. **Failed** if any device failed.
3. **Cancelled** if any device was cancelled.
4. **Passed** if every device passed.

Cancellation records a request; it is not an immediate confirmation that the
BMC stopped collecting evidence. The controller tries to stop an active BMC
task before releasing its claim. A cancellation timeout allows release
without confirmation that the task stopped. Repeating a cancellation request
preserves the original deadline. Cancelling an unknown job succeeds without
changing any records.

## Reading results and keeping history

The commands below belong to `nico-admin-cli attestation spdm`. They use the
[SPDM gRPC methods](https://github.com/dsx-ai-factory/infra-controller/blob/3b9810b9d32560579870654deba2a3904ac06ede/crates/rpc/proto/forge.proto#L1098).

### Start a run: `trigger`

`trigger` schedules attestation for one machine through
`TriggerMachineAttestation`. On success, it prints the job ID when devices
were scheduled, or `none` when no job was created. Keep that ID to inspect or
cancel the run.

### Inspect or cancel a run: `job`

The `job` group operates on individual runs:

| Subcommand | Purpose | gRPC method |
| --- | --- | --- |
| `list` | Find retained jobs by status | `ListAttestation` |
| `get` | Read one job's device details | `GetAttestation` |
| `cancel` | Request cancellation of one job | `CancelAttestation` |

`list` requires `--selector` with `in-progress`, `failed`, `cancelled`, or
`passed`. `get` and `cancel` take a job ID. Both `list` and `get` print JSON;
`get` returns the details from `SpdmGetAttestationResponse`. Reading an unknown
or removed job returns not found. A successful `cancel` acknowledges the
request; the controller performs the cancellation as described above.

### Check machines: `machines`

`machines list` calls `ListMachineAttestationStatuses`. Without
`--machine-id`, it predicts fleet statuses from each machine's latest retained
controller-triggered job, ignoring external jobs and making no BMC calls.
Machines without a controller job have unknown status.

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

Implementation: [scheduling](https://github.com/dsx-ai-factory/infra-controller/blob/3b9810b9d32560579870654deba2a3904ac06ede/crates/machine-controller/src/handler/attestation.rs),
[job reads and status](https://github.com/dsx-ai-factory/infra-controller/blob/3b9810b9d32560579870654deba2a3904ac06ede/crates/api-db/src/attestation/spdm.rs),
[worker](https://github.com/dsx-ai-factory/infra-controller/blob/3b9810b9d32560579870654deba2a3904ac06ede/crates/spdm-controller/src/handler.rs),
and [storage and cleanup](https://github.com/dsx-ai-factory/infra-controller/blob/3b9810b9d32560579870654deba2a3904ac06ede/crates/api-db/migrations/20260917113009_spdm_job_id.sql).
