# Health probe IDs

This page provides a list of health probes provided by NVIDIA Infra Controller (NICo), along with their IDs.
Health reports will contain these IDs in the `alerts` section in case the associated check or validation has failed.

## Machine validation health probe identifiers

### `FailedValidationTest`

Indicates that a certain host validation test failed.
The alert will carry details about which test failed.

### `FailedValidationTestCompletion`

Indicates that the host validation test framework failed to complete scheduling
all specified tests on the host.

## SKU validation health probe identifiers

### `SkuValidation`

An alert with this ID is placed on a host in case the SKU validation workflow failed.
The alert will make the host un-allocatable by tenants.

## Repair workflow integrations related health probe identifiers

### `TenantReportedIssue`

Indicates that a tenant reported an issue with the host while releasing the bare metal instance. The host won't be available for other tenants until the alert is cleared.

### `RequestRepair`

Indicates that a tenant reported an issue with the host while releasing the bare metal instance
and that repair by an external framework is required.

## Site Explorer health probe identifiers

### `BmcExplorationFailure`

Indicates that the hosts BMC endpoint could not be scraped. This can happen if the BMC is not reachable, but also in case the BMC response to any API call is malformed.

### `PoweredOff`

Indicates that the power status of a host as reported by the BMC is **not** on.

### `SerialNumberMismatch`

Indicates that the serial number on a host does not match the serial number in the Expected Machine manifest.

### `OrphanManagedHost`

Indicates that an already-ingested Managed Host's BMC MAC is no longer listed in the `expected_machines` table. NICo continues to maintain the host, but the host will **not** be re-ingested if it is force-deleted. Clear the alert by either re-adding the entry to `expected_machines` or force-deleting the Managed Host. The alert is informational and does not block tenant allocations.

## Hardware/BMC health probe identifiers

`nico-hardware-health` currently reports sensor-based hardware health with a single probe ID:

### `BmcSensor`

Indicates that a BMC sensor reported a warning/critical/failure condition.

Details:
- `target` is set to the BMC sensor ID (for example, a fan/temperature/power sensor name).
- The alert `message` contains the entity type, reading, unit, and threshold ranges used for evaluation.
- Classifications are documented in [Health alert classifications](health_alert_classifications.md), including `Hardware`, `SensorWarning`, `SensorCritical`, and `SensorFailure`.

`message` format:

```text
<entity_type> '<sensor_id>': <status> - reading <value><unit> (<reading_type>), valid range: <range>, caution: <range>, critical: <range>
```

Example:

```text
power_supply 'PSU0_OutputPower': Critical - reading 1320.00W (power), valid range: 0.0 to 1500.0, caution: 1200.0 to 1300.0, critical: 0.0 to 1310.0
```

## NVLink rack health probe identifiers

### `NmxcUnreachable`

Raised on a rack when the NVLink partition monitor could not use the rack's
NMX-C for `nvlink_config.partition_monitor_unreachable_alert_after` consecutive
passes (default `3`): no switch yielded an endpoint, the endpoint URL was
invalid, the client could not be created, `Hello` failed, or `Hello` returned
no usable domain UUID. The monitor writes a `Merge` report on the rack under the
source `nvlink-partition-monitor`, with one alert whose `target` is the NMX-C
endpoint URL the monitor resolved, absent when none resolved, and whose
`message` names the failing step. The alert carries
[`Hardware`](health_alert_classifications.md#hardware) only; it does not block
allocations. A single failed pass is not reported, since a keepalive timeout on
a loaded site is routine, and the stored alert is not rewritten while the outage
continues. A rack is reachable on a pass if any of its chassis reached NMX-C.
The first pass that reaches NMX-C removes the report, as does a rack that no
longer has managed hosts for the monitor to reconcile. Only passes without a
stored report count towards the threshold, so removing the report by hand
during an ongoing outage suppresses it for another
`partition_monitor_unreachable_alert_after` consecutive failed passes. The
alert's `in_alert_since` is the first failed pass of the run that raised it, not
the pass that wrote it. A report write that fails is retried on the next pass.
Standalone chassis mapped through `nvlink_nmxc_endpoints` have no rack and
receive no such alert.

## NVLink isolation health probe identifiers

A host whose aggregate health carries the `IsolateFromNvLinkPartitions`
classification, for example from the admin CLI `compute-tray-due-for-swap`
template, is marked for NVLink isolation. On each NVLink partition monitor pass
(`nvlink_config.monitor_run_interval`), the monitor removes the host's GPUs from
every NVLink partition and records the progress as a `Merge` report under the
source `nvlink-isolation`. The report states the membership NMX-C listed when the
pass started, so a host reads as isolated on the pass after its removals
complete.

The monitor rewrites the report only when the status changes. A failed write
leaves the stored report unchanged, is logged with the machine ID, and is
retried on the next pass. When the host is no longer marked, the monitor removes
the report.

See [Moving or Replacing a Compute Tray](../../manuals/nvlink_partitioning.md#moving-or-replacing-a-compute-tray)
for the operator steps.

### `NvlinkIsolationPending`

An alert that names the GPUs NMX-C still lists in a partition, or states that
isolation cannot be confirmed because the host's NVLink GPU inventory is empty
or contains a GPU whose stored UID is `0`. The alert carries
[`PreventAllocations`](health_alert_classifications.md#preventallocations) and
[`SuppressExternalAlerting`](health_alert_classifications.md#suppressexternalalerting).

### `NvlinkIsolated`

A success that the monitor reports once the host's NVLink GPU inventory is
known and NMX-C lists none of those GPUs in any partition. The report then has
no alerts.

## NVLink GPU health probe identifiers

### `NvlinkUnhealthy`

Raised when NMX-C reports a GPU in the host's NVLink inventory as `Degraded`,
`NoNvlink`, or `DegradedBw`. The NVLink partition monitor writes a `Merge` report
under the source `nvlink-partition-monitor`. Its alerts carry
[`PreventAllocations`](health_alert_classifications.md#preventallocations),
blocking normal tenant allocation while they remain in aggregate host health.
Targeted allocation with `allow_unhealthy_machine` can bypass this health check.
A host-level `Replace` health report takes precedence over the monitor's merge
report. The monitor only updates its merge entry; it leaves replace overrides
unchanged, including one with the same source name.

The report carries one alert per affected GPU. Each alert's `target` is the
GPU's NMX-C UID, and its `message` names the health state NMX-C reported
(`degraded`, `no_nvlink`, or `degraded_bw`). An affected GPU stays in the report
until NMX-C reports that same GPU `Healthy` and its UID still appears in the
host's NVLink inventory. `Unknown` health, or the GPU being absent from the
response, keeps it flagged. NMX-C represents a GPU under rediscovery as UID `0`
with `Unknown` health, so a healthy sibling GPU never clears an affected one.
GPUs that were never flagged create no alert while `Unknown` or absent. UID `0`
is never treated as an identity on either side: NMX-C rows with UID `0` are
ignored, and an inventory entry whose stored UID is `0` (an unparseable fabric
GUID at discovery) is never evaluated. A failure of the pass's initial
partition-list fetch or of the GPU-health query prevents reevaluation. The
alert evaluation runs before the pass's later post-operation partition-list
refresh, so a failure of that refresh does not mean the stored report was
retained; it may already have been set or cleared on that pass. The two
fetches are told apart by the `nmxc_connect_err` field of the monitor's
per-pass metrics summary log and by the `error` label on
`carbide_nvlink_partition_monitor_nmxc_connect_error_count`, which read
`Failed to get NMX-C partition info list when updating db` for the later
refresh. A failed health-report
write leaves the stored report unchanged until the next pass retries it; each such failure increments
`carbide_nvlink_partition_monitor_unhealthy_alert_write_failures_total`
(label `write`: `set` or `clear`) and logs the machine ID and error. An alert
can therefore reflect an earlier fault rather than a
current unhealthy observation. The stored report is rewritten only when the set
of affected GPUs or their classifications changes, so an alert's `message` label
may lag a GPU that moves between unhealthy states until the set next changes.
If the stored copy of an alert loses `PreventAllocations` (for example, through
a manual edit of this source's report), the monitor restores it on the next pass
for as long as the alert is retained, including while NMX-C reports that GPU
`Unknown` or omits it.

The monitor removes its merge report once every affected GPU's alert has
cleared. Replacing a GPU with a different UID does not clear the old GPU's
alert, even if the replacement reports `Healthy`. If an affected UID has left
the host's inventory, automatic recovery cannot clear it; after confirming the
fault was resolved, remove the report manually.

To clear it manually, for example after confirming the fault was resolved
outside NICo:

```bash
nico-admin-cli machine health-report remove <machine-id> nvlink-partition-monitor
```

This removes the whole report for that source, including all affected GPU
alerts. If both `Replace` and `Merge` reports use this source name, the command
removes the replace report first; run it again to remove the merge report.

The monitor re-evaluates on each successful GPU-health query
(`nvlink_config.monitor_run_interval`, default `60s`). If NMX-C still reports an
unhealthy GPU matching the host's inventory, the monitor re-asserts the alert
with a new `in_alert_since` after manual removal. Use removal for resolved
faults whose alerts the monitor can no longer clear, such as a replaced GPU or
a host that has left the NVLink inventory.

## DPU related health probe identifiers

### `BgpPeeringTor`

Indicates that a BGP session with a top-of-rack (TOR) switch could not be established by a host/DPU.

### `BgpPeeringRouteServer`

Indicates that a BGP session with the route server that is part of the NICo control plane could not be established by a host/DPU.

### `BgpStats`

Indicates that BGP statistics could not be extacted by `dpu-agent`

### `BgpDaemonEnabled`

Indicates that the BGP daemon (FRR) is not running on the DPU

### `DhcpRelay`

Indicates issues regarding the start of the DHCP relay on the DPU

### `DhcpServer`

Indicates issues regarding the start of the DHCP server on the DPU

### `HeartbeatTimeout`

Indicates that there was no communication between `dpu-agent` and NICo core for a certain amount of time.
This condition usually implies that the DPU won't be able to apply any configuration changes.

### `StaleAgentVersion`

Indicates that `dpu-agent` has not been updated to the newest version, even though the newest release had been available for a certain amount of time.

### `ContainerExists`

Indicates that a container that was expected to run on the DPU is not running

### `SupervisorctlStatus`

Indicates an issue with retrieving the list of running services

### `ServiceRunning`

Indicates that an expected service on the DPU is not runnning

### `PostConfigCheckWait`

The alert is placed on a host for a few seconds after a configuration change by dpu-agent in order to allow the configuration changes to "settle" before doing the health assessment.
That avoids the host to move between states even though the new configuration might be  problematic.

### `RestrictedMode`

Indicates that the DPU is not running in restricted mode

### `DpuDiskUtilizationCheck`

Indicates that the dpu-agent failed to check disk utilization

### `DpuDiskUtilizationCritical`

Indicates that the dpu-agent disk utilization on the DPU is above a critical threshold

## Other health probe identifiers

### `MissingReport`

The alert indicates that no health report was received, where health report
was expected. It is different from `HeartbeatTimeout` in the following sense
- `HeartbeatTimeout` alerts can be emitted if data is available, but stale.
  `MissingReport` is only emitted if data has never been received.
- `MissingReport` is mainly used on the NICo client side. It has no impact on
  state changes.

### `MalformedReport`

An alert which can be generated if a HealthReport can not be parsed
This alert is only be used the NICo client side if failing to render the health
report is preferrable to failing the workflow.

### `Maintenance`

The alert is used by site admins to mark hosts that are under maintenance - e.g. for CPU or memory replacements.

### `HostUpdateInProgress`

Indicates that an update for host firmware was scheduled on the host

### `IbCleanupPending`

Indicates that the host was released back to the admin pool without the system being able to fully clean up all port to partition key associations for all InfiniBand interfaces.
This means the host might still be bound to a tenants partition.
Once the IB subsystem can communicate with UFM and detects that the port is not bound to a partition anymore, the alert will automatically clear.
