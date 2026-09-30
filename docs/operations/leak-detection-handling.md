# Leak Detection and Handling

## Overview

In a rack-scale system, there are two sets of leak sensors: NICo and the BMS each manage one set, and detect leaks based on them. Additionally, the BMS controls the rack AC power breaker and coolant valve for rack isolation remediation, while NICo provides infrastructure-management health reporting, allocation protection, and safe handling.

NICo uses the sensors it manages in compute and NVSwitch trays to detect leak events. NICo also listens via the [DSX Exchange](https://docs.nvidia.com/dsx-exchange) MQTT event bus for BMS-issued leak events from BMS-managed sensors such as CDU, rope, and dripping pan. A leak event results in tray or rack health warnings and alerts in NICo, which prevent the machines from being allocated. Depending on the leak type and severity, NICo takes automated handling actions based on configuration and policy.

## Current capability: automated e2e leak detection, reporting, allocation protection, and handling

### Compute and NVSwitch tray sensor path

NICo hardware-health monitoring discovers configured BMC endpoints and queries them through Redfish. When compute or NVSwitch tray BMCs expose leak-related sensor data and the relevant health collector is configured, NICo considers that data as part of hardware health.

```text
Compute/NVSwitch-tray leak sensor
        ↓
Compute/NVSwitch-tray BMC
        ↓ Redfish
NICo hardware-health service
        ↓
NICo tray health report
```

NICo generates health reports with leakage warnings or alerts for the leaking trays. The alert classification and operational effect for this path depend on the site's health-processor configuration.

### BMS leak-event integration

NICo also supports a BMS leak event path (based on BMS-managed sensors such as CDU, rope, dripping pan, and others) through the `nico-dsx-exchange-consumer` service.

```text
BMS detects / clears a leak condition
        ↓
BMS publishes MQTT metadata and value events
        ↓
NICo DSX Exchange consumer
        ↓
NICo rack health report
```

The consumer supports these BMS event types:

- Rack leak detection (`LeakDetectRack`)
- Rack leak-sensor fault (`LeakSensorFaultRack`)
- Rack-tray leak detection (`LeakDetectRackTray`)

For an active supported BMS event, NICo creates a rack health report with a leak alert. When the BMS publishes a clear event, NICo removes the corresponding rack health report.

### Deployment requirements for BMS event integration

The BMS event path is not enabled automatically in every NICo deployment. To use the BMS event path, a deployment must have all of the following:

1. `nico-dsx-exchange-consumer` enabled (this Helm subchart is disabled by default)
1. Connectivity and configuration for the BMS MQTT broker and event topics
1. BMS metadata and value events that use supported point types and identify the affected rack
1. The consumer configured to call the NICo API
1. Health aggregation configured for the relevant racks and hosts

Without these prerequisites, BMS events do not create NICo rack health reports.

### Health Reporting and Allocation Protection

NICo provides **health visibility and allocation protection** for leak-related conditions. For BMS-based leak detection, rack leak health alerts come with these classifications:

- `PreventAllocations`
- `SensorCritical`

`PreventAllocations` blocks new allocations for hosts affected by the active tray- or rack-health condition. When the health alert clears later, NICo recalculates aggregate health; allocation eligibility can recover when no other active health condition prevents allocation.

Current operational visibility is provided through NICo's health data, health alert details, logs, and metrics. To obtain a machine's current health reports, use the `GET /v2/org/{org}/nico/machine/{id}/health-report` [REST API endpoint](api:GET/v2/org/:org/nico/machine/:machineId/health-report).

### Three Tiers of Automated Leak Handling

NICo and the BMS together automatically take leak handling actions in three tiers.

#### Critical Leakage Handling

Critical leaks are those detected by the BMS based on BMS-managed sensors. Because these sensors are often at rack or even larger scope, any leaks detected from them can have a very large and serious impact. Critical leaks often require immediate rack electric and liquid isolation; the BMS does this using its control of AC power breakers and coolant valves.

#### Severe Leakage Handling

If in-tray NICo-managed sensors indicate that multiple trays in a rack are leaking, and the number of leaking trays exceeds a configured threshold (the default for NVL72 racks is two leaking trays), the rack is considered to have a severe leakage. To prevent more trays in the rack from being impacted by the leak, NICo immediately requests the BMS, using DSX Exchange, to perform electric and liquid isolation for the rack.

Upon receiving the isolation request, the BMS triggers the AC power breaker and coolant shutoff valve, and broadcasts the isolation result over DSX Exchange.

#### General Leakage Handling

When there are leaking trays but not enough to trigger a critical or severe leak, NICo handles the general leakage via policy-based automation.

The default handling policy is to shut down all leaking trays. For a leaking tray that is still powered on, NICo forcefully shuts it down.

Use the `GET /nico/tray/{id}/task` [REST API endpoint](api:GET/v2/org/:org/nico/tray/:id/task) to see the handling operation task status

## Future work: API-manageable, customizable, full-lifecycle leak detection and handling

The overall focus for the next phase of NICo leak detection and handling is to make the feature manageable using APIs, support customized polices, and cover the full lifecycle of liquid-cooled hardware.

### Dedicated leak status and leak handling status API

Instead of relying on low-level APIs for health report and task, [issue #5018](https://github.com/dsx-ai-factory/infra-controller/issues/5018) aims to provide a set of dedicated, easier-to-use leak status and leak handling status reporting API, such as

```http
GET /nico/rack/{id}/leak
GET /nico/tray/{id}/leak
GET /nico/leak
```

These endpoints should report:

- Tray leak sensor status
- Tray leak sensor leakage status
- BMS rack leak sensor status
- BMS rack leak handling status
- Ongoing leak handling actions (graceful tray shutdown, forceful tray shutdown, rack isolation)
- If there is no ongoing leak handling, last leak-handling actions and results

### Customizable leak handling policies

NICo's current default general leak handling uses the NICo policy and automation engine. [Issue #2076](https://github.com/dsx-ai-factory/infra-controller/issues/2076) aims to provide a set of APIs to allow you to create and manage customized general leakage-handling policies at runtime. This is critical for fine-tuning for site-specific needs, especially as hardware becomes denser and more complex.

The following are examples of _potential_ customized future policies:

- Preemptively turn off all trays located below a leaking tray (based on rack topology)
- Turn off a rack if trays are leaking and flapping rapidly (time series)
- Turn off a rack based on a combination of BMS sensor metrics and NICo tray detection (more sensitive yet robust detection)
- Turn off racks in a coolant loop with two or more leaking racks (based on site topology)

### Full-lifecycle leak detection and handling

Currently, NICo only detects from in-tray BMC sensors of _ingested_ machines and switches. This does not cover the full lifecycle of liquid-cooled hardware, which not only exposes risks, but will eventually become a blocker for scaling AI factories.

For trays with a BMC powered on and visible from NICo, but that have not yet been fully ingested, [issue #5391](https://github.com/dsx-ai-factory/infra-controller/issues/5391) aims to expand the collection, detection, reporting, and handling of leakage to those trays. This not only expands leak detection and handling coverage, but is also a dependency for the following enhancement.

For hosts known to be leaking before being turned off, and for hosts in a previously known-leaking rack, [issue #5510](https://github.com/dsx-ai-factory/infra-controller/issues/5510) aims to prevent these hosts from being automatically turned on until their BMCs (or the whole rack's BMCs) can give a definitive clearance of leakage. This will complete the coverage of the full lifecycle of liquid-cooled hardware.

## Related documentation and implementation

- [Monitoring and Health](monitoring-health.md)
- [`crates/dsx-exchange-consumer/README.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dsx-exchange-consumer/README.md)
- [`crates/dsx-exchange-consumer/src/health_updater.rs`](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/dsx-exchange-consumer/src/health_updater.rs)
- [`helm/README.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/README.md)
- [Issue #2076](https://github.com/dsx-ai-factory/infra-controller/issues/2076)
- [Issue #5018](https://github.com/dsx-ai-factory/infra-controller/issues/5018)
- [Issue #5391](https://github.com/dsx-ai-factory/infra-controller/issues/5391)
- [Issue #5510](https://github.com/dsx-ai-factory/infra-controller/issues/5510)
