# NICo Alerting

This document defines critical metrics for alerting, recommended thresholds and example
alert rules for monitoring NICo deployments. For metrics exposure and collection
details, see [metrics.md](metrics.md).

## Alert rule format

NICo alerts use the Prometheus-compatible alerting rule format.
The same rule body (groups, alert names, `expr`, `for`, `labels`, `annotations`) works
with both backends:

- **Prometheus Operator** - deploy as `PrometheusRule` resources
- **VictoriaMetrics Operator** - deploy as `VMRule` resources

All expressions, thresholds and annotations in this document are backend-neutral.

## Threshold ownership

NICo is not the source of truth for hardware alert thresholds.
The Baseboard Management Controller (BMC) on each server defines raw
threshold values for hardware telemetry (temperature, fan speed, power, voltage).
The BMC decides when a sensor reading is WARNING or CRITICAL based on its firmware
policies.

NICo's `nico-hardware-health` service collects these BMC-reported states via Redfish
and transforms them into health classifications (`SensorWarning`, `SensorCritical`),
aggregate metrics and operational impacts (`PreventAllocations`). The fleet-level
thresholds documented here (e.g., "alert when >10% of hosts are unhealthy") are NICo
policies for aggregating BMC decisions across the fleet - NICo does not define what
makes an individual host "unhealthy" at the sensor level.

## 1. Production-validated thresholds

The following thresholds are validated in production NICo deployments:

### Host health

Alert when more than 10% of hosts are unhealthy, excluding hosts with `SuppressExternalAlerting`
classification. Only applies to sites with more than 40 machines.

| Metric | Threshold | Duration |
|--------|-----------|----------|
| Hosts unhealthy | > 10% (excluding suppressed) | 5m |

### IP capacity

Use percentage-based thresholds for IP availability, not absolute counts.

| Metric | Threshold | Duration |
|--------|-----------|----------|
| Available IPs | < 15% for a given IP type | 60m |

### Machines stuck above SLA

| Severity | Threshold | Duration |
|----------|-----------|----------|
| Warning | < 10% stuck in assigned state | 30m |
| Critical | >= 10% stuck in assigned state | 30m |

### Network segments / IB partitions stuck above SLA

| Severity | Threshold | Duration |
|----------|-----------|----------|
| Warning |  < 10 stuck | 60m |
| Critical | >= 10 stuck | 60m |

### State-controller iteration latency

The `NicoStateHandlerLatency` alert fires when average iteration latency exceeds 120 seconds
(120000ms) for 10 minutes, or if any of the iteration latency metrics are absent. It monitors
`carbide_machines_iteration_latency_milliseconds`, `carbide_network_segments_iteration_latency_milliseconds`,
and `carbide_ib_partitions_iteration_latency_milliseconds` histograms using `rate(_sum)/rate(_count)`
because the default histogram buckets max at 10 seconds.

### API availability

| Alert | Expression | Duration |
|-------|------------|----------|
| NicoAPIDown | `max(carbide_api_ready) == 0 or absent(carbide_api_ready)` | 15m |
| NicoAPIFluctuating | `changes(carbide_api_ready[15m]) > 5` | 15m |

## 2. SLO targets

These are operational SLO targets, separate from alert thresholds:

| SLO | Target |
|-----|--------|
| API availability | 99.9% (max 0.1% error rate) |
| API latency | < 1 second |
| State update / reconciliation | < 60 seconds |
| External hardware monitoring latency | < 5 minutes |

### Metrics for SLO monitoring

**API availability** uses `carbide_api_grpc_server_duration_milliseconds` histogram. Compute
availability as one minus the error rate from the `_count` series split by gRPC status:

```text
1 - (
  (sum(rate(carbide_api_grpc_server_duration_milliseconds_count{grpc_status_code!="Ok"}[5m])) or vector(0))
  /
  sum(rate(carbide_api_grpc_server_duration_milliseconds_count[5m]))
)
```

When every request succeeds, there is no error series to match, so the query returns nothing.
`or vector(0)` fills in a zero error rate, so you get `1`, meaning 100% availability.

Two other results are easy to mix up. An empty result means the metric is missing, or the window
is too short to compute a rate. A `NaN` result means the counters exist but have not moved, so
no requests arrived.

**API latency** uses the same histogram. Extract p95 or p99 percentiles and convert to
seconds (metric is in milliseconds, SLO target is 1 second = 1000ms):

```text
histogram_quantile(0.95,
  sum(rate(carbide_api_grpc_server_duration_milliseconds_bucket[5m])) by (le)
) / 1000 < 1
```

**State controller iteration latency** is covered by `NicoStateHandlerLatency` described
[above](#state-controller-iteration-latency). Related metrics include
`carbide_machines_handler_latency_in_state_milliseconds` for per-state handler time and
`carbide_machines_per_state_above_sla` for objects exceeding configured per-state thresholds.

## 3. Site-related thresholds

The following metrics are useful for alerting but site-specific.
Use these as starting points and tune based on your site characteristics:

| Metric | Expression | Suggested approach |
|--------|------------|--------------------|
| Hosts usable | `carbide_hosts_usable_count` | Site-relative percentage of total hosts |
| GPUs usable | `carbide_gpus_usable_count` | Site-relative percentage of total GPUs |
| DPU unhealthy | `(carbide_dpus_up_count - carbide_dpus_healthy_count) / carbide_dpus_up_count` | Warning at > 5%, treat as warning-class with longer `for:` duration |
| DB pool exhaustion | `carbide_db_pool_idle_conns` | Alert on low idle connections or pool timeout errors |
| DPUService rollout hold age | `max by (in_use) (carbide_hosts_dpu_service_sync_oldest_pending_age_seconds)` | Compare with the rollout window the site allows, separately for hosts with and without an instance |

### DPUService rollout holds

On a DPF-managed site, a changed DPUService parks each affected DPU behind a maintenance hold, as described in [DPUService rollout gating](../manuals/dpf.md#dpuservice-rollout-gating). The gauge `carbide_hosts_dpu_service_sync_pending_count` reports the number of hosts whose hold is still outstanding. The gauge `carbide_hosts_dpu_service_sync_oldest_pending_age_seconds` reports the age in seconds of the oldest such hold. A site that does not use DPF records no holds and reports `0` for every series.

Both gauges carry two labels. The `above_sla` label is `true` when the host is past the SLA of its current state. The `in_use` label is `true` when an instance is allocated on the host. All four label combinations are exported, and a combination without a hold reports `0`. The automatic release does not apply to a host with an instance, so a non-zero count for `in_use="true"` is expected during a rollout and has no alert.

Each nico-api replica that runs background services reads the holds from the database every 60 seconds and exports its own copy of every series.

To count hosts, take `max by (above_sla, in_use)` across replicas first, and then `sum` the result. The total number of held hosts is `sum(max by (above_sla, in_use) (carbide_hosts_dpu_service_sync_pending_count))`. A `max` without a `by` clause also collapses the labels, so 2 idle hosts and 3 hosts with an instance show as 3, not 5. For the age gauge, `max` alone is correct, because the oldest hold is the largest value.

The `NicoHostsAboveSLAWithDpuServiceSyncHold` alert is a warning. It fires when `sum(max by (in_use) (carbide_hosts_dpu_service_sync_pending_count{above_sla="true"})) > 0` stays true for 30 minutes, the same duration as the alerts for machines stuck above SLA. The 30 minutes apply to the fleet, not to one host, so different hosts taking turns past their SLA keep the condition true. This rule has not been validated in production. A site has the alert only after the operator applies the updated rules file, as described in [Deploying alert rules](#4-deploying-alert-rules).

When the alert fires, at least one host that is past the SLA of its current state also has a hold outstanding. The alert does not show that the hold is the cause, because a held DPU keeps serving traffic while it waits.

The alert also does not show that a rollout is under way. NICo records a hold whenever DPF reports a DPU of the host in the `NodeEffect` phase, including during DPU provisioning. A host in the `Failed` state has an SLA of zero. A DPF host that failed with a hold recorded therefore keeps the alert firing without a rollout of its own.

To respond:

1. Run `nico-admin-cli dpf service-sync list` to list the held hosts. The list has no SLA column. A host whose `State` is `Ready` or `Assigned/Ready` has no SLA, so the alert does not count it.
2. Run `nico-admin-cli managed-host show <machine-id>` for each remaining host. A host that is past its SLA shows `In State > SLA: true`.
3. Investigate each of those hosts as you would any stuck host. Refer to the [stuck objects playbook](../playbooks/stuck_objects/stuck_objects.md).
4. If the host needs the held DPUService change to recover, release its hold by hand. The automatic release runs only for a host in `Ready`, so it does not reach a host that stays stuck. Refer to [`dpf service-sync release`](../manuals/dpf.md#dpf-service-sync-release) for the command.

The alert clears when no held host is past its SLA. A host stops counting when it leaves the state that it is stuck in, when its hold is released, or when a health alert with the `ExcludeFromStateMachineSla` classification waives its SLA. Refer to [Health alert classifications](#6-health-alert-classifications).

The waiver check has a limit. Each read checks at most 1,000 held hosts that are past the time limit of their state, longest wait first. Beyond that number a host counts as `above_sla="true"` even when its SLA is waived, and each read that leaves hosts unchecked logs the nico-api warning `Too many held hosts are past their state SLA to check each for an SLA waiver; the unchecked hosts are counted as above SLA` with the fields `checked_host_count` and `unchecked_host_count`.

The series of a replica are absent, not `0`, when that replica has not read the holds successfully in the last 150 seconds. When the series are absent from every replica, a firing alert resolves, and the 30 minutes start again after the series return. To notice that the holds are not being read, alert on `absent(carbide_hosts_dpu_service_sync_pending_count)`.

Each failed read also logs the nico-api warning `Could not read the outstanding DPU service sync holds; their gauges keep the previous read until it expires`. When the stored state of one host cannot be decoded, the read fails and the warning names that machine.

## 4. Deploying alert rules

A start set of alert rules with production-validated thresholds is available at
[`helm/observability/alerts/nico-alerts.yaml`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/observability/alerts/nico-alerts.yaml).
The group table below marks any rule that is not production-validated.

The file uses `PrometheusRule` as the CRD kind. For VictoriaMetrics Operator deployments,
change `apiVersion` to `operator.victoriametrics.com/v1beta1` and `kind` to `VMRule` -
the rule groups and expressions remain unchanged.

Apply after reviewing and adjusting thresholds for your site:

```bash
# Review thresholds
vim helm/observability/alerts/nico-alerts.yaml

# For Prometheus Operator
kubectl apply -f helm/observability/alerts/nico-alerts.yaml
kubectl get prometheusrules -n nico-system

# For VictoriaMetrics Operator (after changing apiVersion/kind)
kubectl apply -f helm/observability/alerts/nico-alerts.yaml
kubectl get vmrules -n nico-system
```

The alert rules include these groups:

| Group | Alerts |
|-------|--------|
| `nico-api` | API down, API fluctuating, state-handler latency |
| `nico-sla` | Machines stuck, network segments stuck, IB partitions stuck, hosts above SLA with a DPUService rollout hold (not validated in production) |
| `nico-capacity` | Low IP availability |
| `nico-health` | Hosts unhealthy |

## 5. Per-object alerting

The nico-api state controller can expose optional **per-object state metrics** for
identifying individual stuck or failing objects, not just fleet-wide counts. While
aggregate metrics like `carbide_machines_per_state_above_sla` tell you *how many*
machines are stuck, per-object metrics tell you *which*.

This feature is **opt-in** and exposes O(fleet) cardinality. Enable it only when your
metrics backend can handle the volume. See [metrics.md](metrics.md#4-per-object-state-metrics-endpoint)
for configuration and collection details.

### Per-object SLA alerts

These alerts identify individual objects that have exceeded their configured SLA
for the current state. The alerts are written replica-safe using `max by (...)` to
handle multi-replica deployments.

**Stuck beyond per-object SLA (warning):**

```text
max by (object_type, object_id, state, substate)
    (time() - carbide_object_state_entered_timestamp_seconds)
  > on(object_type, object_id, state, substate) group_left()
    max by (object_type, object_id, state, substate)
        (carbide_object_state_sla_seconds)
```

**Stuck beyond 2× SLA (critical):**

```text
max by (object_type, object_id, state, substate)
    (time() - carbide_object_state_entered_timestamp_seconds)
  > on(object_type, object_id, state, substate) group_left()
    (2 * max by (object_type, object_id, state, substate)
        (carbide_object_state_sla_seconds))
```

These rules automatically adapt when SLA policy changes - there's no need to update
thresholds in alert rules.

### Manual intervention alerts

**Objects requiring operator intervention:**

```text
max by (object_type, object_id, reason)
    (carbide_object_manual_intervention_required == 1)
```

This alert fires for objects in terminal failed states or where the handler has
explicitly flagged manual intervention is required. The `reason` label is a bounded
token (not free text) identifying the failure cause.

**Manual intervention ratio:**

```text
(
  count(carbide_object_manual_intervention_required{object_type="machine"} == 1)
  or vector(0)
)
  /
count(carbide_object_state_entered_timestamp_seconds{object_type="machine"})
```

The intervention metric is emitted only while operator action is needed. The
numerator falls back to zero when that metric has no matching series. The
denominator has no fallback, so missing machine-state telemetry leaves the ratio
without a result.

### Joining with object traits

Use `carbide_object_info` to add context (rack, SKU, vendor, model) to alerts:

```text
(
  max by (object_type, object_id, state, substate)
    (time() - carbide_object_state_entered_timestamp_seconds)
    > on(object_type, object_id, state, substate) group_left()
    max by (object_type, object_id, state, substate)
        (carbide_object_state_sla_seconds)
)
  * on(object_type, object_id) group_left(rack_id, sku, vendor, model)
    topk by (object_type, object_id) (1,
      max by (object_type, object_id, rack_id, sku, vendor, model)
          (carbide_object_info)
    )
```

`topk` keeps one info series per object so conflicting replica traits cannot
break the join. Since info values are all `1`, selection among conflicting
traits is arbitrary and does not identify the newest traits.

### Multi-replica considerations

With `replicas > 1`, per-object series live in the memory of whichever replica
last processed the object. A stale copy can briefly appear from a different pod
until it ages out (within the hold period). Always aggregate away the scrape
instance **before** joining:

```text
max by (object_type, object_id, state, substate) (...)
```

Alerts using these joins should carry a `for:` duration of at least one scrape
interval (60-120s recommended) to handle transient disagreements during transitions.

## 6. Health alert classifications

NICo's health system uses classifications to indicate alert severity and operational impact.
These appear in the `classification` label of `carbide_hosts_unhealthy_by_classification_count`
and affect threshold calculations.

| Classification | Impact |
|---------------|--------|
| `PreventAllocations` | Host cannot be allocated to tenants |
| `PreventHostStateChanges` | Host blocked from lifecycle transitions |
| `SuppressExternalAlerting` | Excluded from fleet-health calculations and alerts |
| `ExcludeFromStateMachineSla` | Not counted toward SLA metrics |
| `Hardware` | Broad category for hardware/BMC issues |
| `SensorWarning` | Sensor crossed caution threshold |
| `SensorCritical` | Sensor crossed critical threshold |
| `SensorFailure` | Sensor reading outside valid range |

The `SuppressExternalAlerting` classification is important for alert thresholds - hosts with
this classification should be excluded from unhealthy percentage calculations.

For detailed troubleshooting of health alerts, see the
[health alerts playbook](../playbooks/stuck_objects/health_alerts.md).

## 7. References

- [NICo alerting rules](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/observability/alerts/nico-alerts.yaml) - Prometheus-compatible rule examples
- [Health alerts playbook](../playbooks/stuck_objects/health_alerts.md)
- [Health alert classifications](../architecture/health/health_alert_classifications.md)
- [Full metrics reference](core_metrics.md)
