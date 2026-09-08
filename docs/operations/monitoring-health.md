# Monitoring and Health

This page covers monitoring and health workflows for NICo sites after
deployment: hardware health, DPU health, aggregate host health, health
overrides, Prometheus scraping, Grafana dashboards, and Loki queries.

Use aggregate host health as the starting point for operational decisions. NICo
combines hardware health, DPU health, validation and discovery checks, rack
health, and health overrides into a single host-level result. Component health
explains which source is responsible for the aggregate result.

Use this page as the entry point for health triage. It gives the primary
inspection path, commands, metrics, dashboards, and log queries needed to start
an investigation. For subsystem-specific behavior, follow the linked hardware,
DPU, health aggregation, and classification references rather than treating this
page as a replacement for those manuals.

For reference, see:

- [Health Checks and Health Aggregation](../architecture/health_aggregation.md)
- [Health Probe IDs](../architecture/health/health_probe_ids.md)
- [Health Alert Classifications](../architecture/health/health_alert_classifications.md)
- [Redfish Workflow](../architecture/redfish_workflow.md)

## Health Sources

NICo builds health from health reports. A health report contains successes and
alerts from a reporting source. Common health sources are:

| Source | What it reports |
|---|---|
| Hardware health | BMC and Redfish hardware state, including sensors, chassis status, and leak-related signals when configured. |
| DPU agent | DPU service health, DPU networking health, BGP state, DHCP service health, and agent heartbeat. |
| Validation and discovery | SKU validation, host validation, endpoint discovery, and inventory checks. |
| Rack health | Rack-level health input when rack health reporting is configured. |
| NVLink domain health | NMX-C controller health for an NVLink domain when domain health reporting is configured. |
| Health overrides | Manual or service-created health reports used for maintenance, repair, validation, or other controlled workflows. |

Each alert has an ID, an optional target, a message, a start time, and one or
more classifications. Classifications define operational impact. For example,
`PreventAllocations` blocks new allocations while the alert is active, and
`ExcludeFromStateMachineSla` excludes the host from state-machine SLA
evaluation.

## Hardware Health Monitoring

NICo monitors hardware through the hardware health service. The Helm chart is
`nico-hardware-health`.

The service discovers BMC endpoints from NICo and queries them through Redfish.
It monitors host BMCs, DPU BMCs, and configured switch or power-shelf BMCs. The
primary monitoring path is sensor collection. Additional collectors can gather
entity metrics (see [Hardware Entity Metrics](#hardware-entity-metrics)),
firmware, log, NMX-T, NMX-C, NVUE REST, and leak-related data when configured.

### Helm Configuration

Enable hardware health in Helm values:

```yaml
nico-hardware-health:
  enabled: true
```

Enable metrics scraping with its ServiceMonitor:

```yaml
nico-hardware-health:
  enabled: true
  replicas: 1

  serviceMonitor:
    enabled: true
    interval: 30s
    scrapeTimeout: 25s
```

By default, the chart exposes hardware health metrics on port `9009`. Log
collection is disabled by default:

```yaml
env:
  NICO_HEALTH__COLLECTORS__LOGS__ENABLED: "false"
```

Enable log collection only through the target site's deployment values.

### Hardware Health Service Configuration

The hardware health service config example,
`crates/health/example/config.example.toml`, documents endpoint discovery,
sinks, rate limits, collectors, processors, and metrics.

Production endpoint discovery uses the NICo API source. The checked-in
hardware-health example config currently names that source
`[endpoint_sources.nico_api]`:

```toml
[endpoint_sources.nico_api]
root_ca = "/var/run/secrets/spiffe.io/ca.crt"
client_cert = "/var/run/secrets/spiffe.io/tls.crt"
client_key = "/var/run/secrets/spiffe.io/tls.key"
api_url = "https://nico-api.nico-system.svc.cluster.local:1079"
```

Static BMC endpoints are supported for local, mock, or special deployments:

```toml
[[endpoint_sources.static_bmc_endpoints]]
ip = "10.0.0.1"
port = 443
mac = "aa:bb:cc:dd:ee:ff"
username = "admin"
password = "secret"
labels = { site = "rno-dev7", cluster = "cluster-01", environment = "development" }
```

Configure direct switch host endpoints separately when desired:

```toml
[[endpoint_sources.static_switch_host_endpoints]]
ip = "10.0.1.2"
port = 443
mac = "11:22:33:44:55:77"
username = "admin"
password = "secret"
switch = { serial = "SN-SWITCH-HOST-001", is_primary = true }
```

This list is optional. Each entry requires `switch` metadata. The
`endpoint_role` field defaults to `host`, and only `host` is accepted in this
list. Switch host entries under `static_bmc_endpoints` use the same `host`
default when `endpoint_role` is omitted. MAC addresses in
`static_switch_host_endpoints` must be unique across both static lists.

For a switch host entry, `port` selects the NVUE REST HTTPS port and defaults
to `443`. The gNMI and NMX-C ports use the global
`collectors.nvue.gnmi.gnmi_port` and `collectors.nmxc.grpc_port` settings,
respectively. NMX-T uses its fixed port `9352`.

Static endpoints can define up to 32 custom telemetry labels. Label names must
match `[a-zA-Z_][a-zA-Z0-9_]*`. Names owned by the health service, such as
`machine_id`, `system_uuid`, `endpoint_ip`, and `collector_type`, are reserved.
Custom labels are exported as Prometheus labels and OTLP resource attributes,
as a `labels` object in JSONL logs, and as a structured map in tracing events.

Collector defaults from the example config:

| Area | Parameter | Example value | Meaning |
|---|---:|---|---|
| Redfish | `bmc_request_concurrency` | `4` | Maximum number of concurrent Redfish operations per BMC. |
| Rate limiting | `bucket_burst` | `200` | Burst size for outbound requests. |
| Rate limiting | `bucket_replenish` | `"35ms"` | Token replenish interval. |
| Sensor collector | `sensor_fetch_interval` | `"1m"` | Sensor polling cadence. |
| Sensor collector | `rediscover_interval` | `"5m"` | Sensor inventory rediscovery cadence. |
| Sensor collector | `state_refresh_interval` | `"30m"` | Broader BMC state refresh cadence. |
| Sensor collector | `include_sensor_thresholds` | `true` | Include BMC threshold data when available. |
| Entity discovery | `refresh_interval` | `"5m"` | Redfish entity inventory rediscovery cadence. |
| Entity discovery | `discovery_concurrency` | `1` | Concurrent endpoint identity resolutions. |
| Entity metrics collector | `fetch_interval` | `"2m"` | Entity metrics polling cadence. |
| Firmware collector | `firmware_refresh_interval` | `"30m"` | Firmware refresh cadence. |
| Logs collector | `mode` | `"sse"` | Preferred BMC log collection mode. |
| NMX-C collector | `grpc_port` | `9370` | Switch-host NMX-C gRPC endpoint port. |
| NMX-C collector | `heartbeat_rate` | `30` | Subscribe heartbeat for NMX-C `DomainStateInfo` updates. |
| NMX-C collector | `connect_timeout` | `"10s"` | TCP connect timeout for the NMX-C gRPC endpoint. |
| NMX-C collector | `rpc_timeout` | `"10s"` | Timeout for NMX-C Hello, Subscribe, and initial Subscribe acknowledgement. |
| NMX-T collector | `scrape_interval` | `"1m"` | Switch telemetry scrape cadence. |
| NVUE REST collector | `poll_interval` | `"1m"` | NVUE REST polling cadence. |
| Leak processor | `minimum_alerts_per_report` | `1` | Leak alert threshold for health reports. |
| Rack leak processor | `leaking_tray_threshold` | `2` | Rack-level leak threshold. |
| Metrics | `endpoint` | `"0.0.0.0:9009"` | Metrics listener. |
| Metrics | `prefix` | `"carbide_hardware_health"` | Hardware-health metric prefix. |

NMX-C connects directly to eligible primary switch-host gRPC endpoints whose
switch config has NMX-C enabled; it does not use BMC or NICo API TLS material.
For static switch-host endpoints, `switch.nmxc_enabled` controls this target
eligibility after the `endpoint_role = "host"` and `is_primary = true` checks;
it defaults to `switch.is_primary` when omitted.
NMX-C notifications emit log events for tracing, log-file, and OTLP log sinks.
With `[collectors.nmxc]` and `[sinks.nvlink_domain_health_report]` enabled,
supported `DomainStateInfo` controller health states also produce NVLink domain
health reports. Both settings are disabled by default. Configuration validation
rejects enabling the sink with `[collectors.nmxc.schema_override]`. See
[NVLink Domain Health Reports](./nvlink-domain-health-reports.md) for state
mapping, identity checks, report persistence, and configuration behavior.

NMX-C collection uses plaintext gRPC over HTTP/2. TLS, certificate
bypass, custom certificate loading, and mTLS are intentionally separate scope; do not model them with the NICo API SPIFFE certificate fields.

### BMC Proxy

The full Helm example enables `nico-bmc-proxy`, the authenticating proxy for BMC
Redfish access. The proxy chart exposes proxy traffic on port `1079` and metrics
on port `1080`.

Example proxy settings:

```toml
listen = "[::]:1079"
metrics_endpoint = "[::]:1080"
allowed_principals = ["spiffe-service-id/nico-api", "spiffe-service-id/nv-dps"]
```

The BMC proxy ServiceMonitor follows the same `serviceMonitor.enabled`,
`interval`, and `scrapeTimeout` pattern as other NICo services.

### Sensor Alerts

Hardware sensor alerts are derived from BMC-reported health, sensor readings,
and thresholds. Sensor classifications include:

| Classification | Meaning |
|---|---|
| `SensorWarning` | Sensor crossed a caution threshold. |
| `SensorCritical` | Sensor crossed a critical threshold. |
| `SensorFailure` | Sensor value is outside the valid range or otherwise invalid. |

If numeric threshold data indicates a problem but the BMC reports the sensor as
healthy, NICo treats the sensor as healthy. In that case the BMC health state is
the authority.

### Hardware Entity Metrics

Beyond sensors, BMCs expose scalar values on Redfish `*Metrics` resources —
error counters, throttle durations, bandwidth utilization, power figures — that
have no sensor backing. The entity metrics collector polls these and exports
them as Prometheus series.

The entity metrics collector is **disabled by default**. Add the `[collectors.metrics]`
section to the hardware health service config to enable it:

```toml
[collectors.metrics]
fetch_interval = "2m"     # default
```

What it collects, per entity type discovered on the BMC:

| Redfish source | Examples |
|---|---|
| `ProcessorMetrics` | Core/other error counters, PCIe error counters (fatal, non-fatal, correctable, replay, NAK, bad TLP/DLLP), power/thermal throttle durations, bandwidth, frequency, temperature, consumed power, core voltage. |
| `MemoryMetrics` | Corrected volatile/persistent errors, current-period and lifetime ECC counters, dirty shutdowns, bandwidth, operating speed, capacity utilization. |
| `DriveMetrics` | Correctable/uncorrectable read and write I/O errors, bad blocks, power-on hours, read/write volume. |
| `PowerSupplyMetrics` | Input voltage/current/power, output power, energy, frequency, temperature, fan speed. |

Sensor-backed values (carrying a Redfish `DataSourceUri`) are skipped:
the sensor collector already publishes them as `hw_sensor` series, so nothing
is double-reported.

Exported series are named
`{prefix}_hw_metric_{metric_type}_{unit}` — with the default
`carbide_hardware_health` prefix, for example:

```text
carbide_hardware_health_hw_metric_correctable_core_errors_count
carbide_hardware_health_hw_metric_pcie_fatal_errors_count
carbide_hardware_health_hw_metric_bandwidth_percent
carbide_hardware_health_hw_metric_input_power_watts
```

On the Prometheus endpoint, series carry entity labels (`processor_id`,
`memory_id`, `drive_id`, `powersupply_id`, `system_id`, `model`, ...) plus the
standard identity labels added by the sink (`machine_id`, `endpoint_ip`,
`serial_number`, `rack_id`, ...), with `collector_type="metrics_collector"`.

The OTLP sink (`[sinks.otlp]`) emits the same metric *names*, but places the
identity context on OTLP resource attributes rather than datapoint labels;
whether those appear as query labels depends on the backend (VictoriaMetrics,
for example, flattens resource attributes onto every series).

Entity discovery runs as its own periodic task (`[collectors.discovery]`,
always on) that walks each BMC's Redfish Systems and Chassis trees and
publishes an inventory snapshot; the metrics collector only reads that
snapshot. Until the first discovery pass completes, the metrics collector
emits nothing.

### Hardware Health Logs

Use Loki or Grafana Explore to inspect hardware health logs for a host:

```logql
{k8s_container_name="nico-hardware-health"} |= "<machine-id>"
```

Health report events include fields such as:

```text
collector=sensor_collector
report_source=bmc-sensors
machine_id=Some(<machine-id>)
alert_count=<n>
success_count=<n>
```

For leak-related events, look for:

```text
report_source=tray-leak-detection
```

Health report records exported over OTLP carry versioned routing fields, counts, and structured success entries by default. Refer to the [OTLP health-report log contract](../architecture/health_aggregation.md#otlp-health-report-log-contract) for the complete attribute schema.

Keep the following in mind when configuring health report records:

- The per-target `include_alert_details` setting defaults to `false`. This omits `health_report.alerts` and `health_report.alerts.dropped`. Set `include_alert_details` to `true` on a `[[sinks.otlp.targets]]` entry to add `health_report.alerts` when the report has alerts.

- *The setting is per-target*. This means that, for example, a debugging destination can receive detail, while a long-term store receives the routing, count, and success evidence without needing to store free-form alert messages.

- The JSON array in `health_report.alerts` contains the first 64 alerts in report order, each with `probe_id`, `message`, `classifications`, and `target` if the alert names one.

- `health_report.alerts.dropped` appears only when details are enabled *and* the report has more than 64 alerts. It contains the number of omitted alerts beyond those first 64.

- Probe IDs use health API names: for example, OOB GPU inventory alerts appear as `SkuValidation` to deduplicate with the in-band SKU alerts.

## DPU Health Checks

`dpu-agent` runs on managed DPUs and reports DPU health to NICo. The BlueField
chart is named `nico-dpu-agent`. A systemd deployment uses
`forge-dpu-agent.service`. A DPF deployment runs the `nico-dpu-agent` container,
and centralized logs identify it as `nico-dpu-agent`.

The agent checks DPU service health, networking state, HBN/NVUE configuration,
DHCP behavior, BGP status, and heartbeat. DPU health is part of aggregate host
health, so an otherwise healthy host can still be unavailable when its DPU is
unhealthy.

See also:

- [DPU Lifecycle Management](../dpu-management/dpu-lifecycle-management.md)
- [DPU Configuration](../dpu-management/dpu_configuration.md)
- [Stuck in WaitingForNetworkConfig and DPU Health](../playbooks/stuck_objects/waiting_for_network_config.md)

### DPU Agent Configuration

Key `nico-dpu-agent` chart values:

| Config area | Value | Meaning |
|---|---|---|
| `certsDir` | site certificate directory | Host certificate directory mounted into the agent container. |
| `securityContext.capabilities.add` | `NET_ADMIN`, `SYS_ADMIN`, `DAC_OVERRIDE`, `NET_RAW` | Linux capabilities used for network and system operations. |
| `hbn.nvue_https_address` | `nvue` | NVUE service name used by the agent. |
| `hbn.nvue_credentials_secret_name` | site value | Secret containing NVUE credentials. |
| `hbn.nvue_password_key` | `password` | Secret key for the NVUE password. |
| `dhcp_server.interface_prepend` | empty by default | Optional DHCP interface prefix argument. |
| `dhcp_server.service_name` | set by DPF service integration | DHCP gRPC service name. |
| `fmds.service_name` | set by DPF service integration | FMDS gRPC service name. |
| `lldpSidecar.resources.requests` | `10m` CPU, `64Mi` memory | Default scheduler request for DPF LLDP collection. |
| `lldpSidecar.resources.limits` | `250m` CPU, `128Mi` memory | Default resource limit for DPF LLDP collection. |

The DaemonSet renders these core arguments:

```text
nico-dpu-agent run
  --hbn-config-mode=nvue-rest
  --agent-platform-type=containerized
  --dhcp-grpc-server=http://<dhcp-grpc-service>:10079
  --fmds-grpc-server=http://<fmds-grpc-service>:50052
```

If `dhcp_server.interface_prepend` is set, the chart also adds:

```text
--dhcp-server-interface-prepend=<prefix>
```

The pod sets these runtime environment variables:

| Environment variable | Source / value |
|---|---|
| `POD_IP` | Kubernetes pod IP field reference. |
| `NODE_NAME` | Kubernetes node name field reference. |
| `POD_NAME` | Kubernetes pod name field reference. |
| `POD_NAMESPACE` | Kubernetes namespace field reference. |
| `IGNORE_MGMT_VRF` | `1`. |
| `NVUE_HTTPS_ADDRESS` | DPF cluster NVUE endpoint. |
| `NVUE_USERNAME` | NVUE user configured for the deployment. |
| `NVUE_PASSWORD` | Secret key from `hbn.nvue_credentials_secret_name`. |
| `RUST_LOG` | `info`. |

### DPF LLDP Collection

A DPF-managed DPU pod includes a `nico-lldp-sidecar` container. It captures
LLDP-MED data through the DPU host's `lldpcli` and publishes `/data/lldp` for
the `nico-dpu-agent` container. A successful capture is refreshed every 120
seconds; a failure is retried after 30 seconds. The previous successful file is
retained across a collection failure, but the agent rejects it after five
minutes.

When physical uplink discovery is missing or stale, inspect both containers in
the DPU pod. Confirm that the sidecar can execute the host `lldpcli`, that
`/data/lldp` is being refreshed, and that the agent has not rejected the file as
too old. Centralized DPF logs identify the sidecar with
`systemd.unit=nico-lldp-sidecar`. A systemd-managed DPU does not use the
snapshot; its agent queries the local `lldpd` service directly.

The deployment and freshness contract is documented in [DPU LLDP Collection](../dpu-management/dpu_configuration.md#dpu-lldp-collection).

### Common DPU Alerts

Common DPU alert IDs include:

- `ContainerExists`
- `ServiceRunning`
- `DhcpServer`
- `BgpStats`
- `BgpPeeringTor`
- `BgpPeeringRouteServer`
- `Ifreload`
- `BgpDaemonEnabled`
- `PostConfigCheckWait`
- `DpuDiskUtilizationCritical`
- `HeartbeatTimeout`

`HeartbeatTimeout` means NICo has not received a recent health report
from the DPU agent. Check whether the DPU is powered, the agent is running, DPU
time is correct, and the DPU can reach NICo.

For `BgpPeeringTor`, start with the alert target and message. A p0 transport
failure includes `PreventAllocations` and blocks normal PXE readiness. A lone
p1 transport failure does not prevent allocation or block host state
transitions. An FRR message that states the session did not negotiate IPv6
unicast is an address family warning, not evidence that the transport session
is down.

`PostConfigCheckWait` is expected in one report after the agent changes HBN or
reloads local DHCP in ContainerExec mode. It includes `PreventAllocations` and
`PreventHostStateChanges`, then clears when the agent sends a fresh sample. If
it appears in consecutive reports, check the DPU agent logs for repeated
configuration applications.

Refer to [DPU ToR Uplink Health](../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
for the configuration values, classifications, and complete transport matrix.

### DPU Logs

Use Loki to inspect DPU agent logs. Select the query for the deployment path:

```logql
{systemd_unit="forge-dpu-agent.service", machine_id="<machine-id>"}
{systemd_unit="nico-dpu-agent", machine_id="<machine-id>"}
```

Use the hostname label when the DPU has not learned its machine ID:

```logql
{systemd_unit="forge-dpu-agent.service", host_name="<host-name>"}
{systemd_unit="nico-dpu-agent", host_name="<host-name>"}
```

On a systemd DPU, use `journalctl` for direct service logs:

```bash
journalctl -u forge-dpu-agent.service -e --no-pager
```

Restart the systemd agent when required:

```bash
systemctl restart forge-dpu-agent.service
```

## Health Alert Lifecycle

NICo health alerts are source-based. A health source submits a fresh report, and
NICo uses the latest report from each source to calculate aggregate health.

A typical alert flow:

1. A source reports an alert such as `PoweredOff` with target `<bmc-ip>`.
2. NICo adds the alert to aggregate host health.
3. Classifications such as `PreventAllocations` define the operational effect.
4. The health view shows the alert ID, target, message, start time, and
   classifications.
5. Metrics and logs identify the responsible source.
6. After remediation, the source submits a fresh report that marks the check
   successful or omits the previous alert.
7. NICo merges the fresh report and aggregate host health returns to healthy.

If a health override created the alert, remove or replace the override after the
operational reason ends.

## Inspect Current Health

Start with the host health page in the Admin Web UI:

```text
https://<nico-api-hostname>/admin/machine/<machine-id>/health
```

Inspect the aggregate health table first. For each alert, note:

- `ID`
- `Target`
- `In Alert Since`
- `Message`
- `Tenant Message`
- `Classifications`

Then inspect component health to identify the source: hardware health, DPU
health, validation, discovery, rack health, or health override.

Use the health history table to review recent transitions. This helps identify
whether an alert is new, recurring, or already cleared by a later health report.

Admin CLI examples:

```bash
nico-admin-cli machine show <machine-id>
nico-admin-cli machine health-override show <machine-id>
```

## Health Overrides

Health overrides add manual or service-created health reports into the same
aggregate health model as automated checks. Overrides are shared health
mechanisms; they are not specific to hardware health.

Use overrides for controlled states such as maintenance, validation, repair,
break-fix, or temporary automation control. Do not use an override as a
substitute for resolving the underlying condition.

### Merge and Replace

| Override mode | Use |
|---|---|
| Merge | Adds a specific health condition while preserving automated health sources. Use this for most manual workflows. |
| Replace | Replaces aggregate health for the target. Use only as a tightly controlled exception because it can hide automated health sources. |

DPU replace overrides are rejected by the API.

### Override Templates

The `nico-admin-cli machine health-override add` command supports templates
for common workflows:

| Template | Use |
|---|---|
| `HostUpdate` | Mark host as in DPU reprovision or host update. |
| `InternalMaintenance` | Internal maintenance window. |
| `OutForRepair` | Host removed from service for repair. |
| `Degraded` | Mark host as degraded. |
| `Validation` | Mark host for validation. |
| `SuppressExternalAlerting` | Suppress external alerting behavior. |
| `MarkHealthy` | Force healthy. |
| `StopRebootForAutomaticRecoveryFromStateMachine` | Block automatic recovery reboots during manual work. |
| `TenantReportedIssue` | Tenant-reported issue while releasing an instance. |
| `RequestRepair` | Tenant-reported issue requiring repair. |

Examples:

```bash
nico-admin-cli machine health-override show <machine-id>

nico-admin-cli machine health-override add <machine-id> \
  --template RequestRepair \
  --message "Manual repair trigger for tenant-reported issue"

nico-admin-cli machine health-override add <machine-id> \
  --template OutForRepair \
  --message "Automated repair failed, requires manual investigation"

nico-admin-cli machine health-override remove <machine-id> repair-request
nico-admin-cli machine health-override remove <machine-id> tenant-reported-issue
```

Before creating an override, identify the current aggregate health, choose the
smallest effect that matches the workflow, include a clear message, and define
the removal condition. After remediation, remove or replace the override and
verify aggregate health.

## Prometheus Metrics

NICo charts expose Prometheus scraping through `ServiceMonitor` resources.
ServiceMonitors are disabled by default in chart values and enabled in the full
example for selected services.

Example:

```yaml
nico-api:
  serviceMonitor:
    enabled: true
    interval: 30s
    scrapeTimeout: 25s

nico-hardware-health:
  serviceMonitor:
    enabled: true
    interval: 30s
    scrapeTimeout: 25s
```

ServiceMonitors from the charts:

| Component | ServiceMonitor | Metrics port |
|---|---|---:|
| `nico-api` | `nico-api-metrics` | `1080` |
| `nico-dsx-exchange-consumer` | `nico-dsx-exchange-consumer-metrics` | `9009` |
| `nico-hardware-health` | `nico-hardware-health-metrics` | `9009` |
| `nico-bmc-proxy` | `nico-bmc-proxy-metrics` | `1080` |
| `nico-dhcp` | `nico-dhcp-metrics` | `1089` |
| `nico-pxe` | `nico-pxe-metrics` | `8080` |
| `nico-ssh-console-rs` | `nico-ssh-console-rs-metrics` | `9009` |
| `unbound` | `unbound-metrics` | `9167` |

Check rendered ServiceMonitors:

```bash
kubectl get servicemonitor -n nico-system
kubectl get servicemonitor -n nico-system nico-api-metrics
kubectl get servicemonitor -n nico-system nico-hardware-health-metrics
kubectl get servicemonitor -n nico-system nico-dsx-exchange-consumer-metrics
```

Core health metrics currently use `carbide_*` metric names. Some dashboards and
site configurations also expose host-health rollups with the `forge_*` prefix.
Use the literal metric name that exists in the target site.

Useful core metric families:

| Metric | Use |
|---|---|
| `carbide_hosts_health_status_count` | Host health counts split by `healthy` and `in_use`. |
| `carbide_hosts_health_overrides_count` | Active merge and replace health overrides. |
| `carbide_hosts_unhealthy_by_probe_id_count` | Active unhealthy hosts by probe ID and probe target. |
| `carbide_hosts_unhealthy_by_classification_count` | Active unhealthy hosts by health-alert classification. |
| `carbide_machines_per_state` | Fleet distribution by machine state. |
| `carbide_machines_per_state_above_sla` | Machines above state-machine SLA. |

Use the Host Health dashboard panels for fleet-level rollups, including host
health status, health overrides, probe alerts, and alert classifications.
Example dashboard queries:

```promql
sum by (healthy, in_use) (
  max by(healthy, in_use) (
    forge_hosts_health_status_count{fresh="true"}
  )
)
```

```promql
sum by(override_type) (
  max by(override_type, in_use) (
    forge_hosts_health_overrides_count{fresh="true"}
  )
)
```

```promql
sum by(probe_id) (
  max by(probe_id, probe_target) (
    forge_hosts_unhealthy_by_probe_id_count{fresh="true"}
  )
)
```

```promql
sum by(classification) (
  max by(classification, in_use) (
    forge_hosts_unhealthy_by_classification_count{fresh="true"}
  )
)
```

### Per-Object Health Metrics

The aggregate metrics above report *counts* of unhealthy objects. To identify
*which* objects carry a given health-alert classification, NICo can emit one
additional time series per affected object:

```text
carbide_object_unhealthy_by_classification_count{object_type="machine",object_id="fm100...",classification="Hardware",in_use="true"} 1
```

Labels:

| Label | Values |
|---|---|
| `object_type` | `machine`, `switch`, `rack`, `power_shelf` |
| `object_id` | The object's NICo id. |
| `classification` | The health-alert classification. |
| `in_use` | Machines only: whether a tenant instance uses the host. |

Emission is opt-in per classification to contain cardinality: series count
still scales with fleet size (one series per matching object per listed
classification — an object carrying two enabled classifications emits two
series), but only for the classifications you list. It is disabled by
default; enable it in the NICo API config by listing the classifications to
emit:

```toml
[observability]
per_object_metrics_for_classifications = ["Hardware", "PreventAllocations"]
```

With an empty list (the default) the metric is not registered at all; the
aggregate health metrics are unaffected either way. Series disappear
automatically when the object becomes healthy, loses the classification, or
is deleted — entries are retained for the registry's hold period, which is
configured slightly longer than the state controllers' `metric_hold_time`.

For example, use the following PromQL query to list hosts blocked from allocations by a hardware problem, or alert when hardware-unhealthy machines accumulate fleet-wide:

```promql
carbide_object_unhealthy_by_classification_count{object_type="machine",classification="Hardware",in_use="false"}

count(carbide_object_unhealthy_by_classification_count{object_type="machine",classification="Hardware"}) > 10
```

### Per-object state progress metrics

NICo can expose current state progress for individual machines, switches,
power shelves, and racks (plus network segments, VPC prefixes, SPDM
attestation, and IB partitions) from a dedicated Prometheus listener. This
endpoint is disabled by default and uses a separate registry, so enabling it
does not add high-cardinality series to the existing `/metrics` endpoint.

```toml
[observability.per_object_state_metrics]
enabled = true
listen_address = "[::]:9091"
# Defaults to all supported object types; also valid: "network_segment",
# "vpc_prefix", "spdm_attestation", "ib_partition".
object_types = ["machine", "switch", "power_shelf", "rack"]
```

Scrape `/metrics` on the configured address at a relatively slow interval
(60–120 seconds is normally sufficient). Queries joining these metrics with
aggregate or health metrics require both the main and per-object endpoints to
be scraped into the same Prometheus.

When deploying with the Helm chart, set
`nico-api.service.perObjectStateMetrics.enabled=true`; this configures the
application listener, container port, and Service together. Enable its
ServiceMonitor with `nico-api.perObjectStateMetricsServiceMonitor.enabled=true`.
If `configFiles.nicoApiConfig` replaces the chart's bundled application
configuration, that custom TOML must include the section above explicitly.

| Metric | Meaning |
|---|---|
| `carbide_object_state_entered_timestamp_seconds` | One series per live object, labeled with its normalized current `state` and `substate`. Subtract it from `time()` to calculate state age. |
| `carbide_object_state_sla_seconds` | The resolved SLA for the current state. States without an SLA emit no series. |
| `carbide_object_manual_intervention_required` | Value `1` while the latest handler result or a terminal failed/error state requires operator action; `reason` is a bounded token (`error` for object types whose stored cause is free text). |
| `carbide_object_info` | Stable traits used for joins: rack, SKU, vendor, and model where known. |
| `carbide_machine_dpu_info` | One series for each host-to-DPU association. |
| `carbide_machine_instance_info` | The current machine-to-instance and tenant association. |

For example, this query finds objects that have exceeded their own resolved
SLA:

```promql
(time() - carbide_object_state_entered_timestamp_seconds)
  > on(object_type, object_id, state, substate) group_left()
    carbide_object_state_sla_seconds
```

DPU metrics:

| Metric | Use |
|---|---|
| `carbide_dpus_up_count` | DPUs with health reports newer than the DPU up threshold. |
| `carbide_dpus_healthy_count` | DPUs whose latest health report is healthy. |
| `carbide_dpu_health_check_failed_count` | Failed DPU health checks by probe. |
| `carbide_dpu_agent_version_count` | DPU-agent version distribution. |
| `carbide_dpu_firmware_version_count` | DPU firmware version distribution. |
| `forge_dpu_agent_network_reachable` | DPU-to-DPU reachability. |
| `forge_dpu_agent_network_latency` | DPU-to-DPU latency. |
| `forge_dpu_agent_network_loss_percentage` | Packet loss in a DPU network check cycle. |
| `forge_dpu_agent_network_monitor_error` | Network monitor errors unrelated to connectivity. |
| `forge_dpu_agent_network_communication_error` | Communication errors to a destination DPU. |

## API Health and Availability

The NICo API is required for health inspection, health report ingestion,
administrative workflows, and state-machine visibility. Check API health before
debugging a host-specific health issue.

Check Kubernetes status:

```bash
kubectl get deploy -n nico-system nico-api
kubectl get pods -n nico-system -l app.kubernetes.io/name=nico-api
kubectl get svc -n nico-system nico-api
```

Check API metrics scraping:

```bash
kubectl get servicemonitor -n nico-system nico-api-metrics
```

Use Loki or Grafana Explore to inspect API logs:

```logql
{k8s_container_name="nico-api"} |= "<machine-id>"
```

```logql
{k8s_container_name="nico-api"} |= "<bmc-ip>" != "SPAN"
```

## Grafana, Loki, and Logs

Use Grafana dashboards for fleet-level triage and Loki for source-specific logs.
Start from aggregate host health, identify the alert source and `inAlertSince`,
then query logs around that time.

Common Loki patterns:

```logql
{systemd_unit="forge-dpu-agent.service", machine_id="<machine-id>"}
{systemd_unit="nico-dpu-agent", machine_id="<machine-id>"}
```

```logql
{k8s_container_name="nico-hardware-health"} |= "<machine-id>"
```

Some sites expose machine identity as a log label. When that label is present,
prefer a label filter over a free-text match:

```logql
{machine_id="<machine-id>"}
```

Console logs are shipped by the `nico-ssh-console-rs` OpenTelemetry Collector
sidecar when enabled:

```yaml
nico-ssh-console-rs:
  lokiLogCollector:
    enabled: true
    image:
      repository: ghcr.io/open-telemetry/opentelemetry-collector-releases/opentelemetry-collector-contrib
      tag: "0.81.0"
```

The sidecar tails:

```text
/var/log/consoles/{machineid}_{bmc_ip}.log
```

It labels console logs with `machineid` and an SSH console exporter label:

```logql
{machineid="<machine-id>", exporter="nico-ssh-console-rs"}
```

Labels vary by log source. Use the Loki label browser to choose the most
specific label available. Common labels include:

- `k8s_container_name`
- `k8s_namespace_name`
- `k8s_pod_name`
- `machine_id`
- `machineid`
- `host_machine_id`
- `host_name`
- `systemd_unit`
- `exporter`
- `level`

`logcli` can be used for repeatable terminal-based Loki queries when direct Loki
access is configured for the site. Use the same LogQL selectors shown above.
For example:

```bash
logcli query --since=1h '{k8s_container_name="nico-hardware-health"} |= "<machine-id>"'
logcli query --since=1h '{systemd_unit="forge-dpu-agent.service"} |= "<machine-id>"'
logcli query --since=1h '{systemd_unit="nico-dpu-agent"} |= "<machine-id>"'
```

### Dashboard Starting Points

Use the site-level health dashboard for fleet triage before drilling into logs.
Start with these panels when they are available:

| Dashboard area | Use |
|---|---|
| Host Health | Site-level host health, probe alerts, classifications, and overrides. |
| DPU Status | DPU health, heartbeat, version, and firmware distribution. |
| Hardware Health Monitor Service Metrics | Hardware-health scrape and collector behavior. |
| Site Explorer | Endpoint discovery and exploration behavior. |
| Machine Update Manager | Update workflow health and state-machine interaction. |

For host-health triage, the highest-value panels are Healthy Host Percentage,
Health Status, Health Overrides, Health Probe Alerts, and Health Alert
Classifications.

## Troubleshooting

| Symptom | Check | Next action |
|---|---|---|
| Host is unhealthy with `PoweredOff` | Admin Web UI health page and hardware-health logs around `inAlertSince`. | Confirm BMC power state and whether the alert target is the expected BMC IP. |
| Host is unhealthy with `HeartbeatTimeout` for the DPU agent | For systemd, use `journalctl -u forge-dpu-agent.service -e --no-pager`. For DPF, query the `nico-dpu-agent` logs in Loki. | Confirm the DPU is powered, its time is synchronized, and it can reach NICo. Restart the systemd service or DPF pod only when service remediation requires it. |
| Host has active overrides | `nico-admin-cli machine health-override show <machine-id>` and the Health Overrides dashboard panel. | Verify the override reason is still valid. Remove temporary overrides after the condition ends. |
| Health metrics are missing | `kubectl get servicemonitor -n nico-system` and the component-specific ServiceMonitor. | Enable the chart `serviceMonitor` block or fix the Prometheus selector/namespace match. |
| Hardware-health logs do not show reports for a host | Loki query for `k8s_container_name="nico-hardware-health"` and the machine ID. | Confirm hardware-health is running, BMC discovery found the endpoint, and the collector is enabled for the source. |
| DPU health probes fail for BGP, DHCP, or ifreload | DPU-agent logs and DPU Status dashboard panels. | Use the DPU health alert ID to choose the subsystem-specific runbook or service check. |
| API health inspection or admin pages are unavailable | `kubectl get deploy`, `pods`, and `svc` for `nico-api`; query API logs. | Restore API availability before debugging host-specific health state. |

## Triage Workflow

1. Open aggregate host health.
2. Record the alert ID, target, message, `inAlertSince`, and classifications.
3. Identify the source: hardware health, DPU health, validation, discovery, rack
   health, or override.
4. Use the source-specific metrics and logs for that alert.
5. Remediate the underlying condition.
6. Wait for a fresh health report from the responsible source.
7. Confirm aggregate host health returns to healthy.
8. Remove temporary overrides used during the investigation.
