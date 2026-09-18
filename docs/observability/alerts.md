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

### State-handler latency

Alert when average iteration latency exceeds the threshold.

| Metric | Threshold | Duration |
|--------|-----------|----------|
| State-handler latency | > 120 seconds | 10m |

Applies to machine, network-segment and IB-partition state handlers.

### API availability

| Alert | Expression | Duration |
|-------|------------|----------|
| NicoAPIDown | `max(carbide_api_ready) == 0 or absent(carbide_api_ready)` | 15m |
| NicoAPIFluctuating | `changes(carbide_api_ready[15m]) > 5` | 15m |

### DPU metrics

| Alert | Condition | Duration |
|-------|-----------|----------|
| DPU metrics missing | Scrape target down or absent | 10m |

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
error rate from the `_count` series split by gRPC status:

```text
1 - (
  sum(rate(carbide_api_grpc_server_duration_milliseconds_count{grpc_status!="OK"}[5m]))
  /
  sum(rate(carbide_api_grpc_server_duration_milliseconds_count[5m]))
)
```

**API latency** uses the same histogram. Extract p95 or p99 percentiles and convert to
seconds (metric is in milliseconds, SLO target is 1 second = 1000ms):

```text
histogram_quantile(0.95,
  sum(rate(carbide_api_grpc_server_duration_milliseconds_bucket[5m])) by (le)
) / 1000 < 1
```

**State update / reconciliation** uses `carbide_machines_handler_latency_in_state_milliseconds`
histogram. The production alert threshold (120s) is more lenient than the SLO target (60s) to
reduce noise. Related metrics include `carbide_machines_iteration_latency_milliseconds` for
full iteration time and `carbide_machines_per_state_above_sla` for objects exceeding configured
per-state thresholds.

## 3. Site-related thresholds

The following metrics are useful for alerting but site-specific.
Use these as starting points and tune based on your site characteristics:

| Metric | Expression | Suggested approach |
|--------|------------|--------------------|
| Hosts usable | `carbide_hosts_usable_count` | Site-relative percentage of total hosts |
| GPUs usable | `carbide_gpus_usable_count` | Site-relative percentage of total GPUs |
| DPU unhealthy | `(carbide_dpus_up_count - carbide_dpus_healthy_count) / carbide_dpus_up_count` | Warning at > 5%, treat as warning-class with longer `for:` duration |
| DB pool exhaustion | `carbide_db_pool_idle_conns` | Alert on low idle connections or pool timeout errors |

## 4. Deploying alert rules

A start set of alert rules with production-validated thresholds is available at
[`helm/observability/alerts/nico-alerts.yaml`](https://github.com/dsx-ai-factory/infra-controller/blob/main/helm/observability/alerts/nico-alerts.yaml).

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
| `nico-sla` | Machines stuck, network segments stuck, IB partitions stuck |
| `nico-capacity` | Low IP availability |
| `nico-health` | Hosts unhealthy, DPU metrics missing |

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
count(carbide_object_manual_intervention_required{object_type="machine"} == 1)
  /
count(carbide_object_state_entered_timestamp_seconds{object_type="machine"})
```

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
  * on(object_type, object_id) group_left(rack, sku, vendor, model)
    max by (object_type, object_id, rack, sku, vendor, model)
        (carbide_object_info)
```

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
These appear in `carbide_hosts_health_alerts_count` labels and affect threshold calculations.

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
