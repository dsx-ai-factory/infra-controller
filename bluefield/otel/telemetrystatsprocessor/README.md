The telemetry stats processor generates metrics about processed logs and
metrics.

- Metrics about metrics are added to what is forwarded to the next stage in the
  pipeline.
- Metrics about logs are written to a configured prometheus endpoint.

If log stats scraped from that endpoint pass through this processor again, they
are ignored.

Example:

```
receivers:
  ...
  prometheus/log-stats:
    config:
      scrape_configs:
        - job_name: log-stats
          scrape_interval: 1m
          static_configs:
            - targets: ["127.0.0.1:8890"]
processors:
  ...
  telemetry_stats:
    metric_scrape_interval: 1m
    metric_groupings:
      - name: metrics_by_name
        by_metric_name: true
        by_metric_type: false
        by_label:
          names:
            - component
      - name: dts_metrics_by_type
        by_metric_name: false
        by_metric_type: true
        include:
          labels:
            - name: component
              values:
                - dts
      - name: metrics_by_label
        by_metric_name: false
        by_metric_type: false
        by_label:
          names:
            - service.name
            - deployment.environment
    log_stats_port: 8890
    log_groupings:
      - name: logs_by_component
        by_label:
          names:
            - component
      - name: logs_by_severity
        by_label:
          names:
            - severity
    labels:
      - name: component
        value: telemetry_stats
    include_telemetry_stats: false
  resource/log-stats:
    attributes:
      - key: component
        value: telemetry_stats
        action: upsert
```
If you add a label to telemetry_stats with the "labels" option that conflicts
with an existing label, the existing label is renamed by prefixing with
"metric_" or "log_" to make way for the configured label at the resource level,
and the prefixed label is saved at the datapoint level. For metrics, each
configured label is added automatically at the resource level. For logs, the
pipeline that receives the telemetry stats on the prometheus endpoint is
responsible for adding the configured labels at the resource level.

Metric groupings can be filtered using "include" and "exclude" with the
following options:

    metric_names:
      - name1
      - name2
    metric_regex: metric_name_regex
    metric_types:
      - Counter
      - Gauge
      - Histogram
    labels
      - name: label_name
        values
          - label_value1
          - label_value2
        value_regex: label_value_regex

The example configuration could generate records like the following:

### Datapoint counts by metric name

```
telemetry_stats_datapoints_total{grouping="metrics_by_name",metric_name="http_requests_total",metric_component="hostmetrics",component="telemetry_stats"} 1000
telemetry_stats_datapoints_total{grouping="metrics_by_name",metric_name="system_cpu_usage",metric_component="hostmetrics",component="telemetry_stats"} 500
telemetry_stats_datapoints_total{grouping="metrics_by_name",metric_name="memory_usage_bytes",metric_component="hostmetrics",component="telemetry_stats"} 750
```

### Datapoint counts of dts metrics by metric type

```
telemetry_stats_datapoints_total{grouping="dts_metrics_by_type",metric_type="Counter",component="telemetry_stats"} 15372
telemetry_stats_datapoints_total{grouping="dts_metrics_by_type",metric_type="Gauge",component="telemetry_stats"} 58212
```

### Datapoint counts by custom labels

```
telemetry_stats_datapoints_total{grouping="metrics_by_label",service.name="frontend",deployment.environment="production",component="telemetry_stats"} 1200
telemetry_stats_datapoints_total{grouping="metrics_by_label",service.name="backend",deployment.environment="production",component="telemetry_stats"} 800
telemetry_stats_datapoints_total{grouping="metrics_by_label",service.name="database",deployment.environment="staging",component="telemetry_stats"} 300
```

### Log record counts by component name

```
telemetry_stats_log_records_total{grouping="logs_by_component",log_component="journald",component="telemetry_stats"} 5000
telemetry_stats_log_records_total{grouping="logs_by_component",log_component="hbn",component="telemetry_stats"} 3000
```

### Log record counts by severity

```
telemetry_stats_log_records_total{grouping="logs_by_severity",severity="info",component="telemetry_stats"} 7000
telemetry_stats_log_records_total{grouping="logs_by_severity",severity="warn",component="telemetry_stats"} 1500
telemetry_stats_log_records_total{grouping="logs_by_severity",severity="error",component="telemetry_stats"} 500
```

## Caveats

While logs are counted globally, metrics are counted per instance of the
processor. Be careful that the values of your metric_groupings (name, type, and
labels) are never the same across multiple pipelines, otherwise the count for
the same time series could alternate between the counts reported for each
pipeline. Such alternations produce confusing metrics, for example when
applying the `increase` function in grafana across a metric that alternates
between a low-value timeline and a high-value timeline, the rate of increase
appears to grow over time.

In the future, metrics could be counted globally across pipelines, so multiple
pipelines would contribute to the same count rather than report different
counts for the same key. But for now, it's more efficient to avoid contention
between multiple instances of the processor.
