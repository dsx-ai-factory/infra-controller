# Health alert classifications

NVIDIA Infra Controller (NICo) uses and recognizes the following set of health alert classifications by convention:

## `PreventAllocations`

Hosts with this classification can not be used by tenants as instances.
An instance creation request using the hosts Machine ID will fail, unless the targeted instance creation feature is used.

## `PreventInstanceDeletion`

Instances on hosts with this classification cannot be released through the
tenant API until the classification is cleared. NICo uses this classification
for online repair requests so that the instance remains assigned while repair
is in progress. Administrative force-deletion is unaffected.

## `PreventHostStateChanges`

Hosts with this classification won't move between certain states during the host's lifecycle.
The classification is mostly used to prevent a host from moving between states while it is uncertain whether all necessary configurations have been applied.

## `SuppressExternalAlerting`

Hosts with this classification will not be taken into account when calculating
site-wide fleet-health. This is achieved by metrics/alerting queries ignoring the amount of hosts with this classification while doing the calculation of 1 - (hosts with alerts / total amount of hosts).

## `ExcludeFromStateMachineSla`

Hosts with this classification will not be counted towards state machine transition time SLA.
This classification is mostly used to prevent the state machine from continuously alerting when some manual operations are being performed on the machine.

It is applied automatically (together with `PreventAllocations` and `SuppressExternalAlerting`) when a host is placed into maintenance mode via the `SetMaintenance` RPC, so that stuck-instance / state-machine SLA alerts do not page on-call for hosts an operator is actively working on — regardless of which state or substate the host is in at the time.

## `StopRebootForAutomaticRecoveryFromStateMachine`

For hosts with this classification, the NICo state machine will not automatically
execute certain recovery actions (like reboots). The classification can be used to prevent NICo from interacting with hosts while datacenter operators manually perform certain actions.

## `Hardware`

Indicates a hardware-related issue and is used as a broad bucket for hardware/BMC alerts.

## `SensorWarning`

Indicates that a sensor reading violated a caution/warning threshold.
In `nico-hardware-health`, this corresponds to crossing `lower_caution`/`upper_caution` thresholds.

## `SensorCritical`

Indicates that a sensor reading violated a critical threshold.
In `nico-hardware-health`, this corresponds to crossing `lower_critical`/`upper_critical` thresholds.

## `SensorFatal`

Indicates that a sensor reading violated a fatal threshold.
In `nico-hardware-health`, this corresponds to crossing
`lower_fatal`/`upper_fatal` thresholds.

## `SensorFailure`

Indicates that a sensor reading is outside the advertised valid range.
In `nico-hardware-health`, this corresponds to values outside `range_min`/`range_max` when that range is well-formed.

For `BmcSensor` alerts, severity is evaluated in this order:
`SensorFailure` -> `SensorFatal` -> `SensorCritical` -> `SensorWarning`.

Special case for sensor classifications:
if thresholds indicate warning/critical/fatal/failure but the BMC explicitly reports sensor health as `Ok`,
the probe is treated as success and no alert classification is emitted.

## `Leak`

Indicates an active leak reported by an NVUE leakage sensor or by a derived
tray-level or rack-level leak report. The operational effect depends on the
configured leak-processing thresholds and the object that the report targets.

## `LeakDetector`

Indicates that an enabled BMC Redfish leak detector reported a warning or
critical state. This classification marks the raw detector observation;
configured leak processing can combine detector observations into a derived
alert with the `Leak` classification.

## `SerialConsole`

Identifies alerts produced from serial-console events by the log parser. The
probe ID and target retain the event name and component reported by the source.
