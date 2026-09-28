# rms-mock

A mock of the Rack Management Service (RMS) gRPC API for simulated racks.
machine-a-tron mounts it on its bmc-mock listener, so the HTTPS endpoint that
serves the simulated Baseboard Management Controllers (BMCs) also answers
`RackManager` and `RackManagerV2` calls. The mock keeps no inventory of its
own: it reports node placement from the same simulated hardware the Redfish
chassis are built from, so the two cannot disagree. RPCs outside its scope
return `UNIMPLEMENTED`.

## Served RPCs

Besides `GetVersion` and `BatchGetNodeDeviceInfo`, the mock serves the RPCs a
rack passes on its way to ready and through maintenance:
`ConfigureSwitchCertificate` with `GetConfigureSwitchCertificateJobStatus`,
the V2 `ConfigureScaleUpFabricManager` with `GetJobStatus`,
`GetScaleUpFabricStatus`, and `BatchGetScaleUpFabricServiceStatus`, power
control with `BatchGetPowerState` and `BatchSetPowerState`, firmware with
`ListFirmwareObjects`, `ApplyFirmwareObject`, and `GetFirmwareJobStatus`,
NVLink switch operating system (NVOS) images with `ApplySwitchSystemImage` and
`GetSwitchSystemImageJobStatus`, and the switch lifecycle with
`UpdateSwitchSystemPassword` and `BatchResetSwitchFactoryDefault`.

Power is read from and applied to the simulated BMC under the same rules as a
Redfish request, so a device the BMC refuses is a per-node failure. `RESET` is
a power cycle, and a device mid-cycle reads `OFF`. Nothing is installed on a
simulated device: firmware, images, and passwords are accepted and tracked as
jobs, and a factory reset clears the switch's fabric primary role when its job
completes, so the rack re-elects. The mock elects one fabric-manager primary
per rack, which is the one switch that reads back enabled: the requested
primary when it is one of the rack's simulated switches, otherwise the one
lowest in the rack, with node id breaking ties.

The mock matches a requested node to a simulated device by address, BMC MAC
first, and echoes `node_id` back without interpreting it. NICo sends its own
row id, the machine, switch, or power shelf id, once the device is ingested,
and the device's BMC MAC before that. A node that matches no simulated
device is a per-node failure: the batch fails and the node's result says why. The
RPCs whose caller reads per-node results, `ConfigureSwitchCertificate` and
`ApplyFirmwareObject`, issue no job for it. The RPCs whose caller learns the
outcome from the job alone, `ApplySwitchSystemImage`,
`UpdateSwitchSystemPassword`, and `BatchResetSwitchFactoryDefault`, give it a
child job that fails for the same reason. `ConfigureScaleUpFabricManager` has
no per-node results, so when none of its switches match, the job it returns
fails and names them. An empty node set is an empty batch, which completes on
its first poll.

## Jobs

A batch RPC issues one parent job, returned as the batch's `job_id`, and one
child per matched node, returned in `jobs` keyed by the caller's node id. An
unmatched node gets a child only on the RPCs listed above. Jobs advance
each time they are polled rather than with time: a node job is running on its
first observation and terminal on its second, whether it is polled directly
or through its parent. Polling a parent advances every child once and reports
what they add up to: failed once any child has failed, with a message naming
the failed nodes, completed once every child has (so an empty batch completes
on its first poll), and running otherwise. `GetJobStatus` lists the polled job first and, with
`include_child_job_states`, its children after it. Starting a job for a node
leaves any earlier job for that node to report its own outcome.

Ids are `rms-mock-<run>-<n>`, where `run` is the process start time in Unix
nanoseconds and `n` counts up from 1, so a restart never re-issues an id NICo
still polls. Treat them as opaque. A job seen complete is forgotten, and so is
a parent once it has reported a terminal state or has no child left, while a
failed node-level job keeps reading failed. The mock keeps the most recent
4,096 failed node jobs and forgets older ones, which then read as completed.
RMS instead keeps completed and failed jobs for 24 hours by default and caps
its job tracker at 10,000 records. A poll for a job this process has no record
of reports it completed on `GetJobStatus` and
`GetConfigureSwitchCertificateJobStatus`, with one completed child listed
under it for the callers that accept a completed parent only with children.
`GetFirmwareJobStatus` and `GetSwitchSystemImageJobStatus` answer
`RETURN_CODE_FAILURE` with a reason for an id this process never issued, as
their proto specifies.

A job for a matched node completes unless the host names its node in
`FaultConfig::fail_jobs_for_node_ids`, which is set from Rust and not from
configuration. A child issued for an unmatched node fails as described above.
The crate's tests use the fault table to exercise NICo's failure paths. A
selected job still reports running before it fails with
`simulated failure for node <node_id>`.

## Firmware Catalog

`ListFirmwareObjects` reports one object per id in `firmware_object_ids` of
the `[rms_mock]` table in the machine-a-tron configuration, ids only, and does
not apply the request's filters, which NICo does not set. Left out, the
catalog holds one object, `rms-mock-fw-1.0.0`. An apply is attributed to the
`Id` its source of truth (SOT) document names, or to the first configured
object when it names none. A document that is not a JSON object is
`INVALID_ARGUMENT` and starts no job.

## Pointing NICo at the Mock

Refer to
[Machine-a-tron RMS Mock](../../docs/development/machine-a-tron-rms-mock.md)
for the `nico-api.rms` values that route NICo's RMS calls to machine-a-tron
and for what the mock does not do. The optional `[rms_mock]` table in the
machine-a-tron configuration sets `version_string`, which `GetVersion`
reports, and `firmware_object_ids`.
