# Health probe IDs

NICo health reports identify each check with a stable probe ID. Failed checks
appear in `alerts`; successful checks can appear in `successes`.

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

## NVLink domain health probe identifiers

### `NmxControllerHealth`

Indicates that NMX-C reported `Unhealthy` or `UnhealthyDbCorrupted` controller
health for an NVLink domain. A `Healthy` report clears the probe. `Degraded` and
`Unknown` do not generate a domain health report.

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
GUID at discovery) is never evaluated. A failed partition-list or GPU-health
query prevents reevaluation, and a failed health-report write leaves the stored
report unchanged. An alert can therefore reflect an earlier fault rather than a
current unhealthy observation. The stored report is rewritten only when the set
of affected GPUs or their classifications changes, so an alert's `message` label
may lag a GPU that moves between unhealthy states until the set next changes.
If the stored copy of an alert loses `PreventAllocations` (for example, through
a manual edit of this source's report), the monitor restores it on the next pass
while NMX-C still reports that GPU unhealthy.

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

Reports a DPU top-of-rack (ToR) uplink problem. For a finding on one expected
uplink, the `target` identifies p0 or p1 and the message identifies the failed
condition. On the NVUE path, a request failure or a minimum greater than two
uses an untargeted critical alert.

Both NVUE and FRR use this ID when a BGP transport session is unavailable. A p0
transport alert includes `PreventAllocations` because normal PXE boot requires
p0. A lone p1 transport failure is unclassified or suppressed, depending on
`min_dpu_functioning_links`. With a positive minimum, both unavailable sessions
produce alerts with `PreventAllocations` and `PreventHostStateChanges`.

For FNN configurations with an IPv6 loopback, the FRR path also uses this ID
when an established transport session did not negotiate IPv6 unicast. A single
address family warning is unclassified and does not indicate a transport
failure. Refer to
[DPU ToR Uplink Health](../../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
for the complete policy and transport state matrix.

### `BgpPeeringRouteServer`

Indicates that a BGP session with the route server that is part of the NICo control plane could not be established by a host/DPU.

### `BgpStats`

Indicates that `dpu-agent` could not collect or validate FRR BGP statistics.
The FRR path also uses this critical alert when
`min_dpu_functioning_links` is greater than the two expected uplinks.

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

Indicates that an expected service on the DPU is not running.

### `PostConfigCheckWait`

`dpu-agent` adds this critical alert to one health report after it changes HBN
or reloads local DHCP in ContainerExec mode. The alert includes
`PreventAllocations` and `PreventHostStateChanges`. NICo waits for the next
health report before it uses the newly acknowledged configuration version.

This is not a fixed timer. The alert clears from the next report when the agent
does not apply another configuration change. If it continues across multiple
reports, check whether the agent repeatedly applies the configuration. Refer to
[Health Sampling After Configuration Changes](../../dpu-management/dpu_configuration.md#health-sampling-after-configuration-changes)
for behavior on each path.

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
