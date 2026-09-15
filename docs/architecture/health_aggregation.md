# Health Checks and Health Aggregation

NVIDIA Infra Controller (NICo) integrates a variety of tools to continuously assess and report the health of any host under its management. It also allows site operators to configure and extend the set of health checks via runtime configurations and extension APIs.

The health information that is obtained by these tools is rolled up within NICo Core into an "aggregated host health".
The aggregated host health information is used for multiple purposes:

1. For NICo internal decision making - e.g. "is this host usable as a bare metal instance by a tenant" and "is the host allowed to transition between 2 states".
2. The aggregated host health information is made available to NICo API users. Site administrators can use the information to assess host health and external fleet health automation systems can use it to trigger remediation workflows.
3. A filtered subset of the aggregated health status is made available to tenants in order to inform them whether their host is subject to known problems and whether they should release it.

## Health check types

Health checks roughly fall into 3 categories:

1. Out of band health checks: These continuous health checks are able to continuously assess the health of a host - independent of whether the host is used as a bare metal instance or not. Within this category, NICo provides the following types of health checks
    1. [BMC health metric based health monitoring](#bmc-health-monitoring)
    2. [BMC inventory based health monitoring](#bmc-inventory-monitoring)
    3. [dpu-agent based health monitoring](#dpu-agent-based-health-monitoring)
2. In band health checks: These health checks run at certain well-defined points in time during the host lifecycle. Within this category, NICo provides the following types of health checks
    1. [Machine validation tests](#machine-validation-tests)
    2. [SKU validation tests](#sku-validation-tests)
3. Health status assessments by external tools and operators: NICo allows external tooling to provide health information via APIs. These APIs have the same capabilities as all health related tools that are provided by NICo. They can thereby used to extend the scope of health-monitoring as required by site operators. These APIs are described in the [Health overrides](#health-report-overrides)

The overall health of the system can be seen as the combination of all health reports
reports. If any component reports that a subsystem is not healthy, then the
overall system is not healthy. This combination of health-reports is performed
inside `nico-core` at any time the health status of a host is queried.

A more detailed list of health probes can be found in [Health Probe IDs](health/health_probe_ids.md).
A list of health alert classifications can be found in [Health Alert Classifications](health/health_alert_classifications.md).

## Overview diagram

The following diagram provides an overview about the current sources of health information within NICo, and how they are rolled up for API users:

```mermaid
flowchart TB
    classDef bmcclass fill:orange,stroke:#333,stroke-width:3px;
    classDef osclass fill:lightblue,stroke:#333,stroke-width:3px;
    classDef hostclass fill:lightgrey,stroke:#333,stroke-width:3px;
    classDef nicoclass fill:#76b900,stroke:#333,stroke-width:3px;

    subgraph Users["Users and External Systems"]
        direction TB
        extautomations["External Automation Systems"]
        siteadmin["NICo<br>Site Admin 🧑"]
        tenant["NICo<br>Bare Metal Instance<br>User (Tenant) 🧑"]
        Metrics["Site MetricsAggregation (OTEL, Prometheus, etc)"]
    end

    subgraph Deployment["NICo Deployment"]
        nico-core["<b>nico-core</b><br>- derives aggregate Health status<br>- uses aggregate health for decision making"]
        HWMON["Hardware Health Monitor"]
        class nico-core nicoclass;
        class HWMON nicoclass;
    end
   
    subgraph Host["Host"]
        direction TB
            subgraph hbmc["BMC"]
            end
            hbmc:::bmcclass;
            subgraph hostos["Host OS"]
                nico-scout("nico-scout running<br>validation tests")
            end
            class hostos osclass;
    end

    subgraph DPU["DPU"]
        direction TB
            subgraph dpubmc["BMC"]
            end
            dpubmc:::bmcclass;
            subgraph dpuos["DPU OS"]
                dpu-metrics-collector["DPU metrics collector (DTS, OTEL)"]
                nico-dpu-agent["nico-dpu-agent<br>Performs additional health checks"]
            end
            class dpuos osclass;
    end

    subgraph ManagedHostHost["NICo Managed Host"]
        direction TB
            Host
            DPU
        class DPU hostclass;
        class Host hostclass;
    end

    nico-core -- Host Inventory --> HWMON
    HWMON -- BMC metric extraction<br>via redfish --> hbmc & dpubmc
    HWMON -- Host & DPU BMC Metrics --> Metrics
    HWMON -- BMC Health Rollups --> nico-core
    nico-scout -- Validation Test Results --> nico-core
    nico-dpu-agent -- DPU Health rollup --> nico-core
    dpu-metrics-collector -- Health related DPU metrics --> nico-dpu-agent
    dpu-metrics-collector -- DPU Metrics --> Metrics
    nico-core -- Host Health Status --> siteadmin & extautomations
    siteadmin & extautomations -- overwrite Health status via API --> nico-core
    nico-core -- Instance Health Status --> tenant
```

## Health Report format

NICo components exchange and store aggregated health information internally in a datastructure called `HealthReport`. It contains a set of failed health checks (`alerts`) as well as a set of succeeded health checks (`successes`). Each check describes exactly which component had been probed (`id` and `target` fields).

The datastructure had been designed and optimized for merging health information from a variety of sources into an aggregate report. E.g. if 2 subsystems report health, and each subsystem reports 1 health alert, the aggregate health report will contain 2 alerts if the alerts are reported by different probe IDs.

A Health report is described as follows in gRPC format. Health reports are in some workflows also exposed in other formats - e.g. JSON. These formats would still follow the same schema.

```proto3
// Reports the aggregate health of a system or subsystem
message HealthReport {
  // Identifies the source of the health report
  // This could e.g. be `nico-dpu-agent`, `nico-host-validation`,
  // or an override (e.g. `overrides.sre-team`)
  string source = 1;
  // The time when this health status was observed.
  //
  // Clients submitting a health report can leave this field empty in order
  // to store the current time as timestamp.
  //
  // In case the HealthReport is derived by combining the reports of various
  // subsystems, the timestamp will relate to the oldest overall report.
  optional google.protobuf.Timestamp observed_at = 2;
  // List of all successful health probes
  repeated HealthProbeSuccess successes = 3;
  // List of all alerts that have been raised by health probes
  repeated HealthProbeAlert alerts = 4;
}

// An alert that has been raised by a health-probe
message HealthProbeAlert {
  // Stable ID of the health probe that raised an alert
  string id = 1;
  // The component that the probe is targeting.
  // This could be e.g.
  // - a physical component (e.g. a Fan probe might check various chassis fans)
  // - a logical component (a check which probes whether disk space is available
  //   can list the volume name as target)
  //
  // The field is optional. It can be absent if the probe ID already fully
  // describes what is tested.
  //
  // Targets are useful if the same type of probe checks the health of multiple components.
  // If a health report lists multiple probes of the same type and with different targets,
  // then those probe/target combinations are treated individually.
  // E.g. the `in_alert_since` and `classifications` fields for each probe/target
  // combination are calculated individually when reports are merged.
  optional string target = 6;
  // The first time the probe raised an alert
  // If this field is empty while the HealthReport is sent to nico-api
  // the behavior is as follows:
  // - If an alert of the same `id` was reported before, the timestamp of the
  // previous alert will be retained.
  // - If this is a new alert, the timestamp will be set to "now".
  optional google.protobuf.Timestamp in_alert_since = 2;
  // A message that describes the alert
  string message = 3;
  // An optional message that will be relayed to tenants
  optional string tenant_message = 4;
  // Classifications for this alert
  // A string is used here to maintain flexibility
  repeated string classifications = 5;
}

// A successful health probe (reported no alerts)
message HealthProbeSuccess {
  // Stable ID of the health probe that succeeded
  string id = 1;
  // The component that the probe is targeting.
  // This could be e.g.
  // - a physical component (e.g. a Fan probe might check various chassis fans)
  // - a logical component (a check which probes whether disk space is available
  //   can list the volume name as target)
  //
  // The field is optional. It can be absent if the probe ID already fully
  // describes what is tested.
  //
  // Targets are useful if the same type of probe checks the health of multiple components.
  // If a health report lists multiple probes of the same type and with different targets,
  // then those probe/target combinations are treated individually.
  // E.g. the `in_alert_since` and `classifications` fields for each probe/target
  // combination are calculated individually when reports are merged.
  optional string target = 2;
}
```

### Classification of health probe results

For failed health checks, the `HealthProbeAlert` can carry an optional set of `classifications` that describe how the system will react on the failed health check.

Not all alerts have the same effect. An unavailable secondary p1 session can
remain visible without preventing allocation or host state changes. When uplink
checks are enabled, an unavailable primary p0 session prevents allocation
because normal PXE depends on p0. Losing both sessions also prevents host state
changes. Refer to
[DPU ToR Uplink Health](../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
for the complete policy.

Most NICo decisions use classifications without matching specific alert IDs.
For example, allocation logic does not need an exhaustive check over every
health probe ID:

```rust
if alert.id == "BgpPeeringFailure" || alert.id === BmcUnreachable || lots_of_other_conditions {
    host_is_fit_for_instance_creation = false;
}
```

Instead of this, it can just scan whether any of the health alerts in the aggregate host health carries a certain condition:

```rust
if alert.classifications.contains("PreventAllocations") {
  host_is_fit_for_instance_creation = false;
}
```

The normal PXE readiness check is a narrow exception. It matches the
`BgpPeeringTor` probe ID together with `PreventAllocations`; it does not inspect
the alert target. The DPU agent uses that combination for a p0 transport
failure. A host `Replace` report takes precedence for this check. Without one,
NICo checks the primary DPU's `Replace` report, or each of its `Merge` reports
when no DPU `Replace` report exists.

During `WaitingForNetworkConfig`, a host `Replace` report can therefore supply
the PXE blocking signal even when the host has no managed DPU. During
`WaitingForRebootToReady`, NICo evaluates this special signal only for hosts
with managed DPUs. Every host still receives the separate aggregate
`PreventHostStateChanges` check immediately before the normal PXE restart.

This mechanism also allows site-administrator provided health checks via [Health report override APIs](#health-report-overrides) to trigger the same behavior as integrated health checks.

The set of classifications that are currently interpreted by NICo is described in [List of Health Alert Classifications](health/health_alert_classifications.md)

## In band health checks

### Machine validation tests

NICo will schedule the execution of validation tests via the `scout` tool on the actual host at various points
in the lifecycle of a managed host:

1. When the host is ingested into NICo
2. After an instance is released by tenant and got cleaned up
3. On demand while the host is not assigned to any tenant

The set of tests that are run on a host are defined by the site administrator.
Each test is defined as an arbitrary shell script which needs to run and is expected to return an exit code of `0`.
The framework thereby allows the execution of off-the-shelf tests, e.g. using the tools [dcgm](https://docs.nvidia.com/datacenter/dcgm/latest/user-guide/index.html), `stress-ng` or `benchpress`.

If Machine Validation fails, a Health Alert with ID `FailedValidationTest` or `FailedValidationTestCompletion` will be placed on the host to make the host un-allocatable by tenants.

In addition to that, the full test output (stdout and stderr) will be stored within nico-core and is made available to NICo users via APIs, admin-cli and admin-ui.

Details can be found in the [Machine Validation guide](../provisioning/machine-validation.md).

### SKU validation tests

SKU validation is a feature in NICo which validates that a host contains all the hardware it is expected to contain by validating it to "conform to a certain SKU".
The SKU is the definition of hardware components within the host. And the SKU validation workflow compares it to the set of hardware components that have been detected via NICo hardware discovery workflows - which utilize inband data as well as out of band data.

SKU validation can thereby detect whether a host has, for example:

- The right type of CPU installed
- The right amount of memory installed
- The right type and amount of GPUS installed
- The right type and amount of InfiniBand NICs installed, and whether they are connected to switches

SKU validation runs at the same points in the host lifecycle as machine validation tests, and can also be run on-demand while the host is not assigned to any tenant.

If SKU validation fails, a Health Alert with ID `SkuValidation` will be placed on the host
to make the host un-allocatable by tenants.

Details can be found in the [SKU Validation guide](../provisioning/sku-validation.md).

## Out of band health monitoring

### BMC health monitoring

The [`nico-hw-health`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/health) service periodically queries all Host and DPU BMCs in the system for health information. It emits the captured health datapoints as metrics on a metrics endpoint that can be scraped by a standard telemetry system (prometheus/otel).

Health metrics fetched from BMCs include:

- Fan speeds
- Temperatures
- Power supply utilization, outputs and voltages

In addition to metrics, `nico-hw-health` also extracts the values of various event-logs from the BMC and stores them on-disk in order to make them easily accessible for a standard telemetry exporter (e.g. OpenTelemetry Collector based).

Finally, `nico-hw-health` also emits a health-rollup in `HealthReport` format towards `nico-core` that contains an assessed health status of the host based on the extracted metrics.
This assessed health status is built by comparing the metrics that are emitted from BMCs against well-defined
ranges or by interpreting the `health_ok` values provided by BMCs.

**For production deployments**, `nico-hw-health` discovers machine, switch, and power-shelf BMC endpoints from NICo API via `[endpoint_sources.nico_api]`.

Machine endpoints carry the inventory metadata needed to interpret hardware health in fleet context. This includes machine ID, primary Redfish system UUID, serial number, rack ID, rack placement, and NVLink domain UUID when present.

Switch endpoints carry switch ID, serial number, rack ID, rack placement, and NVLink domain UUID when present. Power-shelf endpoints carry power-shelf ID, serial number, and rack ID when present.

**For local and test deployments**, you can configure explicit machine, switch, or power-shelf identity with `[[endpoint_sources.static_bmc_endpoints]]`. Direct switch host endpoints can use `[[endpoint_sources.static_switch_host_endpoints]]` or `[[endpoint_sources.static_bmc_endpoints]]`. Note the following:

- Static machine endpoints can include the same serial number, rack placement, and NVLink domain UUID metadata
- Static switch endpoints can include serial number, rack placement, and NVLink domain UUID metadata
- `static_switch_host_endpoints` requires switch metadata; `endpoint_role` defaults to `host`, and only the `host` role is accepted
- All static endpoints can provide `rack_id` and validated custom telemetry `labels`
- The primary Redfish system UUID remains BMC-derived and cannot be overridden by a custom label

The publishing sinks expose that inventory context using the conventions of the target backend:

- `[sinks.prometheus]` adds _machine_ metadata as metric labels named `machine_id`, `system_uuid`, `serial_number`, `rack_id`, `machine_slot_number`, `machine_tray_index`, and `nvlink_domain_uuid`. _Switch_ metadata labels are `switch_id`, `serial_number`, `rack_id`, `switch_slot_number`, `switch_tray_index`, and `nvlink_domain_uuid`. _Power-shelf_ metadata labels are `power_shelf_id`, `serial_number`, and `rack_id`; each is omitted when the corresponding metadata is unavailable. Static endpoint custom labels keep their configured names.
- `[sinks.tracing]` adds `rack_id` to every collector-event log when the endpoint supplies one. Endpoint-source, collector diagnostic, lifecycle, cancellation, and failure logs use the same optional field when they have endpoint context.
- `[sinks.log_file]` adds `rack_id` as a top-level JSONL string field when the endpoint supplies one.
- `[sinks.otlp]` adds the string resource attributes `collector.type` and either `bmc.endpoint` and `bmc.ip`, or `switch.endpoint` and `switch.ip` for host-side switch collection. Typed inventory adds the strings `component.type` and, when present, `rack.id`. _Machine_ metadata attributes are the strings `machine.id`, `system.uuid`, `machine.serial`, `driver.version`, and `nvlink.domain.uuid`, plus the integers `machine.slot_number` and `machine.tray_index`. _Switch_ metadata attributes are the strings `switch.id`, `switch.serial_number`, `switch.endpoint_role`, and `nvlink.domain.uuid`, the boolean `switch.is_primary`, and the integers `switch.slot_number` and `switch.tray_index`. _Power-shelf_ metadata attributes are the strings `power_shelf.id` and `power_shelf.serial_number`; each is omitted when the corresponding metadata is unavailable. Static endpoint custom labels are string resource attributes and keep their configured names.
- `[sinks.health_report]`, `[sinks.rack_health_report]`, `[sinks.switch_health_report]`, and `[sinks.power_shelf_health_report]` use the same event context when submitting assessed health reports back to NICo API. The persisted `HealthReport` and `HealthProbeAlert` schemas remain the probe success/alert model described above.

Collector runtime metrics and gNMI stream metrics include a `rack_id` label when the endpoint supplies one. The label is omitted when discovery does not supply a rack ID. Existing `collector_type` and `endpoint_key` label semantics remain unchanged.

#### OTLP health-report log contract

OTLP health-report logs keep the existing human-readable summary body and add a
versioned structured attribute contract. Match `health_report.schema_version`
against `v1` before decoding `health_report.successes`. Per-alert detail keeps
the existing opt-in JSON representation controlled by the target's
`include_alert_details` setting, which defaults to `false`; consequently,
`health_report.alerts` and `health_report.alerts.dropped` are absent unless
details are enabled. If the schema version is missing or unsupported, consumers
retain the human-readable summary body, ignore the structured `health_report.*`
attributes, and do not reject the record. This fallback emits no warning.
Additional fields may be present besides the minimum ones listed below.

| Attribute | OTLP type | Presence | Value |
| --------- | --------- | -------- | ----- |
| `event.type` | string | Always | Always `health_report`. |
| `health_report.alerts` | string | Conditional | JSON array containing the first 64 alert objects in report order. Present only when `include_alert_details = true` and the report has alerts. |
| `health_report.alerts.dropped` | int | Conditional | Number of alerts after the first 64 that were omitted from `health_report.alerts`; present only when details are enabled and the report has more than 64 alerts. |
| `health_report.alert_count` | `int_value` (signed 64-bit) | Always | Number of alerts in the report, including any omitted from `health_report.alerts`, bounded to `0..=i64::MAX`. |
| `health_report.observed_at` | string | Optional | Observation time as RFC 3339 UTC with nanosecond precision, for example `2026-07-31T12:34:56.000000000Z`. Omitted when the report carries no observation time. |
| `health_report.schema_version` | string | Always | `v1` for the contract documented here. |
| `health_report.source` | string | Always | The collector that assessed the report: `bmc-sensors`, `bmc-events`, `bmc-leak-detectors`, `nmxc-domain-state`, `tray-leak-detection`, `rack-leak-detection`, `nvue-leakage`, or `gpu-inventory`. |
| `health_report.success_count` | `int_value` (signed 64-bit) | Always | Number of entries in `health_report.successes`, bounded to `0..=i64::MAX`. |
| `health_report.successes` | array of `kvlist` | Always | One entry per succeeded probe, empty when the report has none. |
| `health_report.target` | string | Optional | The kind of inventory object assessed: `machine`, `nvlink-domain`, `power-shelf`, `rack`, or `switch`. Omitted when the report names no target. |

`health_report.success_count` equals the length of `health_report.successes`
while that length is representable as `i64`, and otherwise saturates at
`i64::MAX`. `health_report.alert_count` applies the same bound and is available
regardless of the alert-detail setting. For representable counts, when
`health_report.alerts` is present, the alert count equals the JSON array length
plus `health_report.alerts.dropped`, treating an absent dropped count as zero.

Every `health_report.successes` entry carries at least these fields:

| Field | OTLP type | Presence | Value |
| --------- | --------- | -------- | ----- |
| `probe_id` | string | Always | The probe that ran. See [Health probe IDs](health/health_probe_ids.md) for the supported IDs. |
| `target` | string | Optional | The probed component, such as a sensor or leak-detector ID. Omitted when the probe ID fully describes what was tested. |

When `health_report.alerts` is present, every JSON object carries the same
`probe_id` and optional `target` fields, plus at least these fields:

| Field | JSON type | Presence | Value |
| --------- | --------- | -------- | ----- |
| `message` | string | Always | Human-readable description of the alert. |
| `classifications` | array of string | Always | Zero or more of `SensorOk`, `SensorWarning`, `SensorCritical`, `SensorFatal`, `SensorFailure`, `PreventAllocations`, `Leak`, and `LeakDetector`. Refer to [Health alert classifications](health/health_alert_classifications.md) for the classifications NICo interprets. Unlike reports for the NICo API, this array carries only the classifications the collector raised, without the `Hardware` marker. |

The two record timestamps are set by different clocks. `time_unix_nano` is never zero: it is the report's own observation time if it is representable as Unix nanoseconds, and otherwise the export time. `observed_time_unix_nano` is always the export time. These keep export order recoverable for reports whose observation time is older or absent.

### BMC inventory monitoring

The Site Explorer process within NICo Core periodically queries all Host and DPU BMCs in order to record certain BMC properties (e.g. components within a host and firmware versions).

In certain conditions the scraping process will place a health alert on the host:

- If the host BMC is not reachable
- If any of the host properties indicates the host is not fit for instance creation.

### dpu-agent based health monitoring

[`dpu-agent`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/agent) collects health information directly on the DPU and sends a health-**rollup** towards `nico-core`. The agent monitors a variety of health conditions, including

- whether BGP sessions are established to peers according to the current configuration of the DPU
- whether all required services on the DPU are running
- whether the DPU is configured in restricted mode
- whether the disk utilization is below a threshold

## Health report overrides

Site administrators can inspect and update the health state of a NICo-managed host
through these REST operations:

- `GET /v2/org/{org}/nico/machine/{machineId}/health-report` lists all reports.
- `PUT /v2/org/{org}/nico/machine/{machineId}/health-report` creates or updates an override.
- `DELETE /v2/org/{org}/nico/machine/{machineId}/health-report/{source}` removes an override.

The override API offers 2 different modes of operation:

`Merge` - In this mode, any health probe alerts indicated in the override
will get merged with health probe alerts reported by built-in NICo tools in order
to derive the aggregate host health status. **This mode is meant to augment the internal health monitoring mechanism with additional sources of health data**

`Replace` - In this mode, the health probe alerts reported by built-in NICo
monitoring tools will be ignored. Only alerts that are passed as part of the
override will be taken into account. If the override list is empty, the system
will behave as if the Host would be fully healthy. **This mode is meant to bypass the internal health data in case the site operator desires a different behavior**

The API allows multiple `Merge` overrides on a host at the same time by using a different `source` identifier for each report.
This allows you to integrate health information from multiple external systems and users which are not at risk of overriding each other's data. For example, health information from an external fleet health monitoring system and from SREs can be stored independently.

If a ManagedHost's health is overridden, the remaining behavior is exactly the same
as if the overridden Health report would have been directly derived from monitoring
hardware health:

- The host will be allocatable depending on whether any `PreventAllocations` classification is present in the aggregate host health
- State transitions behave as if NICo integrated monitoring would have detected
  the same health status:
  - A ManagedHost whose health status is overridden from healthy to not-healthy
    will stop performing certain state transitions that require the host to be healthy.
  - A ManagedHost whose health status is overridden from not-healthy to healthy
    will perform state transitions that it would otherwise not have performed.
    This is useful for unblocking hosts in certain operational scenarios - e.g.
    where the integrated health monitoring system reported a host as non-healthy
    for an invalid reason.
- NICo API users will observe that the ManagedHost is not healthy. They
  will also observe that a health override is applied.
