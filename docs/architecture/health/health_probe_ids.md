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

## SPDM attestation health probe identifiers

Attestation alerts are reported under the `attestation` source. Every update
replaces all of that source's alerts at once, so an alert goes away by not
being in the next update rather than being cleared on its own. A machine that
no attestation profile covered raises no alert at all.

### `SpdmAttestationFailed`

Indicates that an attester's newest finished SPDM attestation failed. The
target is the attester ID the machine's BMC reports, such as `HGX_ERoT_GPU_3`,
so a machine carries one alert per failed attester. The message is the stored
failure text from the attestation result. The alert prevents allocations.

It clears when that attester is attested again and passes, or when a profile
change stops selecting it. Starting a new job does not clear it, because the
attester's newest finished result is unchanged until that job finishes the
attester. A missing alert therefore means NICo is making no claim about the
attester right now, not that the problem is fixed.

### `AttestationNoAttesters`

Indicates that an attestation profile applied to the machine, but its BMC
offered no eligible attester. The target is the machine's hardware class, and
is absent when no class is recorded. The alert carries no classification and
does not prevent allocations, because it reports a gap between the profile and
the hardware rather than a bad result from a device.

### `AttestationPolicyUnsatisfied`

Indicates that an attestation profile asked for attesters that the machine's
BMC did not offer. The message names the profile that applied, the requirement
that went unsatisfied, and the profile version, so an operator can see which
revision decided. The target and classification match `AttestationNoAttesters`.

A machine can carry this alert alongside `SpdmAttestationFailed` alerts. When
only some of a profile's patterns matched nothing, the attesters that did match
are still attested, so a device can then fail.

## Repair workflow integrations related health probe identifiers

### `TenantReportedIssue`

Indicates that a tenant reported an issue with the host while releasing the bare metal instance. The host won't be available for other tenants until the alert is cleared.

### `RequestRepair`

Indicates that a tenant reported an issue with the host while releasing the bare metal instance
and that repair by an external framework is required.

### `RequestOnlineRepair`

Indicates that an instance was selected for online repair without releasing it.
The alert prevents new allocations and instance deletion, and suppresses
external fleet-health alerting until the online repair override is cleared.

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

`nico-hardware-health` reports sensor-based hardware health with a single probe ID:

### `BmcSensor`

Indicates that a BMC sensor reported a warning/critical/fatal/failure condition.

Details:

- `target` is set to the BMC sensor ID (for example, a fan/temperature/power sensor name).
- The alert `message` contains the entity type, reading, unit, and threshold ranges used for evaluation.
- Classifications are documented in [Health alert classifications](health_alert_classifications.md), including `Hardware`, `SensorWarning`, `SensorCritical`, `SensorFatal`, and `SensorFailure`.

`message` format:

```text
<entity_type> '<sensor_id>': <status> - reading <value><unit> (<reading_type>), valid range: <range>, caution: <range>, critical: <range>, fatal: <range>
```

Example:

```text
power_supply 'PSU0_OutputPower': Critical - reading 1320.00W (power), valid range: 0.0 to 1500.0, caution: 1200.0 to 1300.0, critical: 0.0 to 1310.0, fatal: not set
```

### `IntrusionSensorTriggered`

Indicates that a BMC event reports an asserted intrusion sensor. The target is
the host BMC. The alert prevents allocations until a matching normal or cleared
event removes it.

## Leak detection health probe identifiers

### `BmcLeakDetection`

Reports BMC Redfish leak detector observations. The target identifies the
detector. Warning and critical detector states use the `LeakDetector`
classification, while a detector that cannot be read uses `SensorFailure`.
Configured leak processing can derive tray-level and rack-level alerts with the
`TrayLeak` and `RackLeak` classifications from these observations.

### `NvueLeakage`

Reports leakage sensor state from the NVUE API on a switch. For per-sensor
observations, the target identifies the sensor; an endpoint-wide unavailable
response has no target. A reported leak uses the `Leak` classification;
unavailable, missing, or unrecognized sensor state uses `SensorFailure`.

## BMS leak health probe identifiers

The DSX Exchange consumer maps Building Management System (BMS) leak events to
rack health reports. The target is the rack ID. Active events prevent
allocations and use the `SensorCritical` and `Hardware` classifications. The
leak-detection probe also uses `RackLeak`; the sensor-fault probe does not,
because it does not establish that a leak is active.

### `BmsLeakDetectRack`

Indicates that BMS reported a rack-level leak.

### `BmsLeakSensorFaultRack`

Indicates that BMS reported a rack-level leak sensor fault.

## NVLink domain health probe identifiers

### `NmxControllerHealth`

Indicates that NMX-C reported `Unhealthy` or `UnhealthyDbCorrupted` controller
health for an NVLink domain. A `Healthy` report clears the probe. `Degraded` and
`Unknown` do not generate a domain health report.

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
continues. Only passes that contact the rack count: a rack skipped by change
detection is not checked, so an outage on an otherwise unchanged rack is first
noticed at its next full check, up to
`nvlink_config.partition_monitor_full_check_interval` later (default `10m`),
and the consecutive-pass count starts there. The first pass that reaches NMX-C
removes the report, as does a rack that no longer has managed hosts for the
monitor to reconcile. Only passes without a stored report count towards the
threshold, so removing the report by hand during an ongoing outage suppresses
it for another `partition_monitor_unreachable_alert_after` consecutive failed
passes. The alert's `in_alert_since` is the first failed pass of the run that
raised it, not the pass that wrote it. A report write that fails is retried on
the next pass, including the removal of a report from a rack that has since
recovered and is being skipped by change detection. Standalone chassis mapped
through
`nvlink_nmxc_endpoints` have no rack and receive no such alert.

## Switch reachability health probe identifiers

Both probes are raised by the switch reachability monitor in `nico-api`, which
runs whenever `nvlink_config.enabled` is `true` unless
`nvlink_config.switch_reachability.enabled` overrides it. Each pass opens a TCP
connection to every switch in the `ready` controller state; switches in other
states, and switches whose NVOS management interface has no IP address yet, are
not probed and keep no report. Alerts are stored as one `Merge` report on the
switch under the source `switch-reachability-monitor`, one alert per service,
each carrying [`Hardware`](health_alert_classifications.md#hardware) only, so
neither blocks allocations. Each service is debounced separately: its alert is
raised after `nvlink_config.switch_reachability.unreachable_alert_after`
consecutive failed checks (default `3`, one per `run_interval`, default `60s`)
and removed on the first check that connects. Only checks without a stored
alert count toward the threshold, so removing an alert by hand during an
ongoing outage suppresses it for another `unreachable_alert_after` checks. The
alert's `in_alert_since` is the first failed check of the run that raised it. A
report write that fails is retried on the next pass. One `nico-api` replica owns
the monitor at a time and keeps its work lock across passes so the counts see
every pass; when that replica stops or loses its lease, the next owner counts
from zero, so a takeover restarts the debounce without touching stored alerts. A switch that leaves the
`ready` state has its report removed on the next pass. A
successful TCP connection proves only that the service accepts connections,
not that it answers requests.

### `SwitchUnreachable`

The switch's NVOS management address did not accept a TCP connection on
`nvlink_config.switch_reachability.nvos_port` (default `443`). The alert's
`target` is the probed `address:port` and its `message` includes the last
connection error.

### `NmxtUnreachable`

The NMX-T telemetry listener on a primary switch did not accept a TCP
connection on `nvlink_config.switch_reachability.nmx_t_port` (default `9352`).
Only switches marked `is_primary` host NMX-T, so other switches never receive
this alert, and a primary that is demoted has the alert removed on the next
pass while its `SwitchUnreachable` checks continue. The alert's `target` is
the probed `address:port` and its `message` includes the last connection error.

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

The monitor re-evaluates on each successful GPU-health query, which runs when
a machine group is checked: every pass (`nvlink_config.monitor_run_interval`,
default `60s`) for a group whose inputs changed, and otherwise at its full check
(`nvlink_config.partition_monitor_full_check_interval`, default `10m`). If NMX-C still reports an
unhealthy GPU matching the host's inventory, the monitor re-asserts the alert
with a new `in_alert_since` after manual removal. Use removal for resolved
faults whose alerts the monitor can no longer clear, such as a replaced GPU or
a host that has left the NVLink inventory.

## NVLink isolation health probe identifiers

A host whose aggregate health carries the `IsolateFromNvLinkPartitions`
classification, for example from the admin CLI `compute-tray-due-for-swap`
template, is marked for NVLink isolation. On each check of the host's machine
group, the NVLink partition monitor removes the host's GPUs from every NVLink
partition and records the progress as a `Merge` report under the source
`nvlink-isolation`. The report states the membership NMX-C listed when the check
started, so a host reads as isolated on the check after its removals complete.
A check that applies changes is repeated on the next pass
(`nvlink_config.monitor_run_interval`, default `60s`).

The monitor rewrites the report only when the status changes. A failed write
leaves the stored report unchanged, is logged with the machine ID, and keeps
the machine group from being skipped until the next pass retries it. When the
host is no longer marked, the monitor removes the report.

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

### `UnexpectedBgpPeer`

Indicates that `dpu-agent` found a BGP session whose peer was not among the
configured host routes or route servers. The target identifies the unexpected
peer. The alert prevents allocations and host state changes.

### `BgpStats`

Indicates that `dpu-agent` could not collect or validate FRR BGP statistics.
The FRR path also uses this critical alert when
`min_dpu_functioning_links` is greater than the two expected uplinks.

### `BgpDaemonEnabled`

Indicates that the BGP daemon (FRR) is not running on the DPU

### `DhcpRelay`

Indicates issues regarding the start of the DHCP relay on the DPU

### `DhcpServer`

Indicates that the DHCP server on the DPU is not working. Its meaning depends on how `dpu-agent` configures HBN:

- ContainerExec: the DHCP server process in the HBN container is not running. While HBN is up, the agent checks it on the primary DPU and on any DPU that uses the tenant network. The alert prevents allocations and host state changes.
- NVUE, which DPF uses: the agent's latest attempt to update the DHCP server failed, and the message contains the error. On a secondary DPU that uses the admin network, the update stops the server. If service VPC slots are configured, it instead keeps the server running for those slots only. On DPF, the DHCP server runs in its own pod. The agent skips the update while HBN is down and reports no result then. The alert prevents allocations but not host state changes, so instance release can proceed.

### `Fmds`

Indicates that `dpu-agent` could not connect to FMDS or push configuration to it in its latest iteration, and the message contains the error. Only the NVUE path, which DPF uses, reports this probe. On every iteration, the agent connects to the external FMDS that DPF deploys, so the alert also appears when FMDS is down on a host without an instance. The alert prevents allocations but not host state changes, so instance release can proceed.

### `Ifreload`

Indicates that the HBN container's `ifreload --all --syntax-check` command
failed to execute, returned a nonzero exit code, or produced non-empty standard
output.

### `FileExists`

Indicates that an expected HBN configuration file does not exist. The target
identifies the missing file.

### `FileIsValid`

Indicates that an expected HBN configuration file could not be inspected or did
not meet its minimum size requirement. The target identifies the file.

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

### `NvueApiRunning`

Indicates that `dpu-agent` could not retrieve basic system information from the
NVUE API.

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

### `IbPortDown`

Indicates that one or more monitored InfiniBand ports are not active. The alert
message reports the affected port GUIDs and prevents allocations.

### `Quarantine`

Indicates that an administrator quarantined the host to block its network
traffic. The alert prevents allocations until the quarantine is cleared.

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

Indicates that termination deferred InfiniBand membership cleanup because
configured-port entries were missing or incomplete within an existing IB
observation. The host can still be bound to a tenant partition. While present,
the alert prevents new allocations on the host by default.
Clearing it alone does not confirm UFM membership removal. Refer to
[Force-Delete and Cleanup](../../manuals/networking/infiniband_partitioning.md#force-delete-and-cleanup)
for the trigger, override for targeted allocation, monitor clearance condition, and
verification limits.
