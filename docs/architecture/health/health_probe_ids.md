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
