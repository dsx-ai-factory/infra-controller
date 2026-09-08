# NICo Logging

How NICo components emit logs, where they go, what format they use and how to tune them.

---

## TL;DR

- All NICo components log to **stdout**. In Kubernetes the kubelet writes container stdout to
  files under `/var/log/pods/`, where a collector (typically an OpenTelemetry Collector DaemonSet)
  picks them up.
- Most components use **logfmt** (key=value pairs). **nico-dns** uses JSON. **nico-ssh-console**
  uses a compact single-line format. **nico-pxe** uses plain text.
- Default log level is **INFO**. Override at startup with `RUST_LOG` or the `--debug` flag.
- **nico-api** supports **runtime log-level changes** via `nico-admin-cli set log-filter`. Changes
  auto-expire (default 1 hour) and revert to the startup level - useful for time-boxed debugging
  without forgetting to turn verbosity back down.
- Centralized logging uses a **pull model**: a DaemonSet collector reads pod log files and ships
  them to your backend (Loki, Elasticsearch, VictoriaLogs, Datadog, etc.).

---

## 1. Where logs go

Every NICo binary writes its own service logs to **stdout**. There is no file-based logging
for service logs - the expectation is Kubernetes-native log collection.

In a Kubernetes cluster:

1. The container runtime captures stdout and writes it to a log file under
   `/var/log/pods/<namespace>_<pod>_<uid>/<container>/*.log`.
2. A log collector (DaemonSet) tails those files and forwards entries to a backend.

This is the standard Kubernetes pattern. NICo does not require any special log driver or
sidecar - just configure your collector to read pod logs.

> **Note**: nico-ssh-console is an exception - in addition to stdout, it writes **machine
> console output** (BMC serial console streams) to local files. These are not service logs
<Note>
nico-ssh-console is an exception - in addition to stdout, it writes **machine console output** (BMC serial console streams) to local files. These are not service logs but captured output from managed machines. See [Machine console logs](#25-machine-console-logs-nico-ssh-console) for details.
</Note>

---

## 2. Which components log and in what format

| Component | Binary | Log format | Notes |
|-----------|--------|------------|-------|
| **nico-api** | `carbide-api` | logfmt | Primary control plane. Supports runtime level changes. |
| **nico-dns** | `carbide-dns` | JSON | DNS resolution service. |
| **nico-dhcp** | Kea + `libdhcp.so` | Kea format | See [Kea DHCP format](#26-kea-dhcp-format-nico-dhcp). |
| **nico-pxe** | `carbide` | plain text | iPXE boot service. Hand-rolled `println!`, no log levels. |
| **nico-bmc-proxy** | `carbide-bmc-proxy` | logfmt | BMC credential proxy. |
| **nico-hardware-health** | `forge-hw-health` | logfmt | Hardware health monitoring. |
| **nico-ssh-console** | `ssh-console` | compact | SSH console access. Also captures machine/DPU console output to files (see [Machine console logs](#25-machine-console-logs-nico-ssh-console)). |
| **nico-dsx-exchange-consumer** | `carbide-dsx-exchange-consumer` | logfmt | DSX message consumer. |
| **nico-dpu-otel-agent** | `forge-dpu-otel-agent` | full | mTLS cert renewal agent on DPUs. See note below. |

<Note>
`nico-dpu-otel-agent` currently ignores `RUST_LOG` due to a source bug (`fmt().init()` instead of `fmt::init()`). It logs at INFO level only until this is fixed. See [#3151](https://github.com/dsx-ai-factory/infra-controller/issues/3151).
</Note>

### 2.1 logfmt format

Most NICo components use [logfmt](https://brandur.org/logfmt) - a line-oriented format of
space-separated `key=value` pairs, easy for both humans and machines to parse.

**Event lines** — one per log call:

```text
level=INFO component=nico-api span_id=0x4f… msg="Starting reconciliation" location="handlers/machine.rs:142"
```

**Span lines** — emitted when a unit of work closes (`level=SPAN`), carrying timing data:

```text
level=SPAN component=site-explorer span_id=0xf7… span_name=explore_site timing_elapsed_us=1523 timing_busy_ns=1200000 timing_idle_ns=323000
```

Common fields:

| Field | Description |
|-------|-------------|
| `level` | Log level: `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`, or `SPAN` for span lifecycle events. |
| `component` | Emitting component (see below). |
| `msg` | Human-readable message. |
| `span_id` | Correlation ID linking events within the same operation. |
| `location` | Source file and line number (`file.rs:line`). |
| `timing_*` | Span timing fields: `timing_elapsed_us`, `timing_busy_ns`, `timing_idle_ns`. |

The logfmt layer is implemented in the `logfmt` crate (`crates/logfmt`). It emits span lifecycle
events (open/close) as `level=SPAN` lines with timing data - useful for identifying slow operations
without enabling full distributed tracing.

#### The `component` field

On logfmt lines, NICo sets the `component` field to identify the emitting service or subsystem:

```text
nico-api                       — API handlers, DB, startup: anything not in a subsystem below
├── site-explorer
├── machine_state_controller
├── switch_controller
├── rack_controller
├── power_shelf_controller
├── network_segments_controller
├── vpc_prefix_controller
├── ib_partition_controller
└── attestation_controller
nico-bmc-proxy
nico-dhcp
nico-dsx-exchange-consumer
nico-fmds
nico-hardware-health
nico-rvs
nico-test-artifact-cache
nico-dpu-agent
nico-scout
```

State-controller lines also carry a `controller=<name>` field with the same value.

This enables filtering logs by component in your backend. For example, with Loki:

```logql
{namespace="nico-system"} | logfmt | component="site-explorer"
```

> **Convention**: NICo uses `component` for the emitting service/subsystem. Don't reuse this key
> for domain data - give those their own keys (e.g. `machine_id`, `controller`).

#### Coverage

Components that **do not** use the logfmt layer and carry no `component` field:

| Component | Binary | Format | Notes |
|-----------|--------|--------|-------|
| nico-dns | `carbide-dns` | JSON | Uses tracing-subscriber JSON formatter |
| nico-pxe | `carbide` | plain text | Hand-rolled `println!`, no log levels |
| nico-ssh-console | `ssh-console` | compact | Uses tracing-subscriber compact formatter |
| nico-dpu-otel-agent | `forge-dpu-otel-agent` | full | Default tracing-subscriber formatter. Ignores `RUST_LOG`. |
| nico-dhcp | Kea + `libdhcp.so` | Kea | Production uses Kea logger, not Rust tracing (see [Kea DHCP format](#26-kea-dhcp-format-nico-dhcp)) |

### 2.2 JSON format (nico-dns)

nico-dns uses `tracing-subscriber`'s JSON formatter. Each line is a self-contained JSON object:

```json
{"timestamp":"2026-01-15T10:23:45.123Z","level":"INFO","target":"carbide_dns","message":"DNS query","query_type":"A","name":"host1.example.com"}
```

### 2.3 Compact format (nico-ssh-console)

nico-ssh-console uses `tracing-subscriber`'s compact formatter - a human-readable single-line
format similar to traditional log output:

```text
2026-01-15T10:23:45.123Z  INFO carbide_ssh_console: Session started session_id=abc-123
```

### 2.4 Plain text (nico-pxe)

nico-pxe uses `println!` for startup messages and request logging middleware. Output is
unstructured plain text.

### 2.5 Machine console logs (nico-ssh-console)

In addition to its own service logs (compact format on stdout), nico-ssh-console captures
**machine and DPU serial console output** to local files. This is separate from the service's
tracing output.

When a BMC console session is established, nico-ssh-console streams the serial output to a
per-machine log file:

```text
/var/log/consoles/<machine-id>_<bmc-ip>.log
```

Key details:

| Setting | Default | Description |
|---------|---------|-------------|
| `console_logs_path` | `/var/log/consoles` | Directory for console log files. |
| `console_logging_enabled` | `true` | Enable/disable console capture. |
| `log_rotate_max_size` | 10 MiB | Rotate when file exceeds this size. |
| `log_rotate_max_rotated_files` | 4 | Keep up to 4 rotated files (`.log.0` through `.log.3`). |

The console logger strips ANSI escape sequences and adds timestamps at session start/stop.
Rotation happens automatically - old logs are renamed with numeric suffixes and the oldest
is deleted when the limit is reached.

These files contain raw machine boot output, kernel messages and anything else that appears
on the serial console. They are useful for debugging boot failures, kernel panics and
hardware issues.

> **Centralizing console logs**: The nico-ssh-console Helm chart includes an optional
> OpenTelemetry Collector sidecar (`lokiLogCollector.enabled: true`) that ships console logs
> to Loki. The sidecar reads from `/var/log/consoles/*.log`, extracts the machine ID and BMC
> IP from filenames, and exports to your Loki endpoint. To use a different backend, customize
> the `configFiles.otelcolConfig` value in the chart. See [Machine and DPU Logs](machine-dpu-logs.md)
<Tip title="Centralizing console logs">
The nico-ssh-console Helm chart includes an optional OpenTelemetry Collector sidecar (`lokiLogCollector.enabled: true`) that ships console logs to Loki. The sidecar reads from `/var/log/consoles/*.log`, extracts the machine ID and BMC IP from filenames, and exports to your Loki endpoint. To use a different backend, customize the `configFiles.otelcolConfig` value in the chart. See [Machine and DPU Logs](machine-dpu-logs.md) for detailed collection workflows.
</Tip>

### 2.6 Kea DHCP format (nico-dhcp)

The nico-dhcp Helm chart runs [Kea DHCP](https://www.isc.org/kea/), with NICo's DHCP logic
loaded as a Kea hook library (`libdhcp.so`). All logging from the hook routes through Kea's
logger, meaning:

- Log format follows **Kea's format**, not logfmt
- Log levels are configured via **Kea's `loggers` config**, not `RUST_LOG`
- Output goes to Kea's configured destinations (stdout by default in the container)

Example Kea log output:

To adjust log levels, modify the Kea configuration in the Helm chart's ConfigMap. The
`loggers` section controls verbosity:

```json
{
  "Dhcp4": {
    "loggers": [
      {
        "name": "kea-dhcp4",
        "severity": "INFO",
        "debuglevel": 0,
        "output_options": [
          { "output": "stdout" }
        ]
      }
    ]
  }
}
```

Set `severity` to `DEBUG` and increase `debuglevel` (0-99) for more verbose output from
the NICo hook. The hook maps Rust log levels to Kea severity: `info` → INFO, `debug` →
DEBUG/10, `trace` → DEBUG/99.

<Tip title="Dev environment">
The standalone `forge-dhcp-server` binary uses logfmt output and respects `RUST_LOG`. Use the standalone binary for local development and testing only.
</Tip>

---

## 3. How to tune log levels

### 3.1 At startup: RUST_LOG and --debug

Most tracing-based components respect the `RUST_LOG` environment variable, which uses
`tracing-subscriber`'s
[`EnvFilter`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
syntax:

```bash
# Global level
RUST_LOG=debug

# Per-module levels
RUST_LOG=info,carbide_api_core::handlers=debug,sqlx=warn

# Combined
RUST_LOG=info,carbide=debug,hyper=error
```

**Exceptions**: nico-pxe uses hand-rolled `println!` with no level concept. nico-dpu-otel-agent
currently ignores `RUST_LOG` due to a source bug (logs at INFO only). nico-dhcp uses Kea's
logger config instead (see section 2.6).

Three binaries accept a `--debug` flag that sets the default level to DEBUG:

```bash
carbide-api --debug run --config-path /etc/carbide/config.toml
carbide-bmc-proxy --debug --config-path /etc/carbide/bmc-proxy.toml
ssh-console --debug --config /etc/ssh-console/config.toml
```

**Default log level** is INFO for all components.

#### Noisy dependencies

NICo components automatically suppress verbose output from common dependencies. For example,
nico-api applies these directives by default:

```text
sqlxmq::runner=warn,sqlx::query=warn,rustify=off,hyper=error,rustls=warn,h2=warn,vaultrs=error
```

Your `RUST_LOG` directives are merged with these defaults, so you don't need to manually
silence framework noise.

### 3.2 At runtime: nico-admin-cli (nico-api only)

nico-api supports **live log-level changes** without a restart. This is useful for debugging
production issues - you can increase verbosity temporarily, then let it automatically revert.

```bash
# Increase verbosity for 1 hour (default expiry)
nico-admin-cli set log-filter -f "debug"

# Target specific modules for 30 minutes
nico-admin-cli set log-filter -f "info,carbide_api_core::handlers::machine=debug" --expiry 30min

# Very verbose, short window
nico-admin-cli set log-filter -f "trace,h2=warn,hyper=warn" --expiry 5min
```

When the expiry elapses, the log filter **automatically reverts** to the startup value. This
prevents accidentally leaving debug logging on and filling your storage.

<Note>
Expiry is checked every 15 minutes by an internal poll. A filter with `--expiry 5min` will actually revert at the next 15-minute poll cycle, not exactly at 5 minutes.
</Note>

#### How it works

The admin CLI calls a gRPC endpoint on nico-api that updates the `EnvFilter` in the running
process. The new filter applies immediately to all subsequent log events - no restart, no
config file change. Under the hood, `tracing-subscriber`'s `reload::Handle` swaps the active
filter.

**Scope**: runtime log-level changes affect **nico-api only**. Other components (nico-dns,
nico-dhcp, etc.) require a restart to change log levels.

### 3.3 Kubernetes deployment

Set `RUST_LOG` in your pod spec or Helm values:

```yaml
# Pod spec
spec:
  containers:
    - name: nico-api
      env:
        - name: RUST_LOG
          value: "info,carbide=debug"

# Or via Helm values
env:
  RUST_LOG: "info,carbide=debug"
```

For temporary debugging, use `nico-admin-cli` to avoid redeploying:

```bash
kubectl exec -it deploy/nico-api -- nico-admin-cli set log-filter -f "debug" --expiry 15min
```

---

## 4. Centralizing logs with OpenTelemetry Collector

The recommended approach for centralized logging is an **OpenTelemetry Collector DaemonSet** that
reads pod log files and exports them to your backend. This is the standard Kubernetes pattern
and works with any OTLP-compatible backend: Grafana Loki, Elasticsearch, VictoriaLogs, Datadog,
Splunk, etc.

```mermaid
flowchart TB
    subgraph Node["Node"]
        subgraph NicoPod["NICo Pod"]
            nicoApi["nico-api"]
        end

        logs["/var/log/pods/..."]

        subgraph OtelDS["otel-collector (DaemonSet)"]
            filelog["filelog receiver"]
        end

        nicoApi -->|"stdout"| logs
        logs --> filelog
    end

    subgraph Backend["Logs Backend (Loki, ES, VL...)"]
        storage["Log storage"]
    end

    filelog -->|"OTLP / Loki API / etc."| storage
```

### 4.1 Collector configuration

Below is a reference Helm values file for deploying the
[OpenTelemetry Collector Helm chart](https://opentelemetry.io/docs/platforms/kubernetes/helm/collector/) to collect NICo logs.
Adapt the exporter section for your backend.

```yaml
# values.yaml for opentelemetry-collector Helm chart
# Install: helm install otel-collector open-telemetry/opentelemetry-collector -f values.yaml
mode: daemonset
image:
  repository: otel/opentelemetry-collector-k8s

presets:
  kubernetesAttributes:
    enabled: true      # Adds k8s metadata (pod name, namespace, etc.)
  logsCollection:
    enabled: true      # Enables filelog receiver for pod logs

config:
  receivers:
    filelog:
      include:
        # Collect only NICo namespace logs
        - /var/log/pods/nico-*/*/*.log
      exclude:
        # Exclude collector's own logs to avoid feedback loops
        - /var/log/pods/*/opentelemetry-collector*/*.log
      # The logsCollection preset configures parsing of container log format

  processors:
    memory_limiter:
      check_interval: 1s
      limit_percentage: 75
      spike_limit_percentage: 20

    batch:
      send_batch_size: 1024
      timeout: 5s

    # Add component label for NICo logs
    resource:
      attributes:
        - action: insert
          key: component
          value: "nico"
        - action: insert
          key: service.name
          from_attribute: k8s.container.name

  exporters:
    # === Choose your backend ===

    # Option A: OTLP over HTTP (VictoriaLogs, etc.)
    otlphttp:
      logs_endpoint: http://victorialogs.monitoring.svc.cluster.local:9428/insert/opentelemetry/v1/logs

    # Option B: OTLP over gRPC (Datadog, Splunk, etc.)
    # otlp:
    #   endpoint: <your-otlp-endpoint>:4317
    #   tls:
    #     insecure: false

    # Option C: Grafana Loki
    # loki:
    #   endpoint: http://loki.loki.svc.cluster.local:3100/loki/api/v1/push
    #   default_labels_enabled:
    #     exporter: false

    # Option D: Elasticsearch / OpenSearch
    # elasticsearch:
    #   endpoints: ["https://elasticsearch.elastic.svc.cluster.local:9200"]
    #   logs_index: "nico-logs"
    #   tls:
    #     insecure: false
    #     ca_file: /etc/otel/certs/ca.crt

  service:
    pipelines:
      logs:
        receivers: [filelog]
        processors: [memory_limiter, resource, batch]
        exporters: [otlphttp]   # Change to your exporter
```

### 4.2 Extracting structured fields

Since most NICo components use logfmt, you can parse structured fields at the collector level.
This makes fields like `level`, `msg`, `machine_id`, etc. available for filtering and querying
in your backend.

Add a transform processor to parse logfmt:

```yaml
processors:
  transform/parse-logfmt:
    log_statements:
      - context: log
        statements:
          # Parse logfmt body into attributes
          # This regex extracts key=value pairs, handling quoted values
          - merge_maps(attributes, ParseKeyValue(body, "=", " "), "upsert")
            where IsMatch(body, "^level=")
```

Or use the `key_value_parser` operator in the filelog receiver for NICo components specifically:

```yaml
receivers:
  filelog:
    include:
      - /var/log/pods/nico-*/*/*.log
    operators:
      - type: container
      - type: key_value_parser
        if: 'body matches "^level="'
        parse_to: attributes
```

### 4.3 Filtering and sampling

For high-volume deployments, filter or sample logs at the collector to reduce noise and
storage costs.

#### Dropping noisy log messages

Some log messages are expected but not useful for debugging. For example, DHCP relay requests
that don't match a defined network segment (because switches relay DHCP device-wide, not
per-network), or DPU kernel messages about mlx5_core module state changes during normal
operation. These can flood logs without adding signal.

Use the [filter processor](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/processor/filterprocessor)
to drop specific messages by pattern:

```yaml
processors:
  # Drop known noisy messages
  filter/noise:
    error_mode: silent
    logs:
      log_record:
        # Drop messages matching these patterns
        - IsMatch(body, "Module ID not recognized")
        - IsMatch(body, "No network segment defined for relay address")

  # Or drop by log level
  filter/drop-debug:
    error_mode: silent
    logs:
      log_record:
        - IsMatch(body, "^level=DEBUG")
        - IsMatch(body, "^level=TRACE")
```

The `error_mode: silent` setting prevents the filter processor from logging errors when a
record doesn't match - otherwise you'd get noise about the noise filtering.

#### Managing filter patterns with Helm

When deploying via Helm, keep filter patterns separate from the main collector config for
easier maintenance. There are two approaches:

**Option A: Patterns in chart files**

Place patterns in a file within your chart package at `files/logs-drop-patterns.txt`:

```text
# files/logs-drop-patterns.txt
# Noisy DHCP messages from cross-network relay
No network segment defined for relay address
# DPU mlx5_core noise
Module ID not recognized
```

Then use `.Files.Lines` in your template to load them:

```yaml
# templates/configmap.yaml (collector config section)
config:
  processors:
    filter/noise:
      error_mode: silent
      logs:
        log_record:
          {{- range .Files.Lines "files/drop-patterns.txt" }}
          {{- $line := . | trim }}
          {{- if and $line (not (hasPrefix "#" .)) }}
          - IsMatch(body, {{ $line | quote }})
          {{- end }}
          {{- end }}
```

This reads patterns at `helm install` time from the chart's `files/` directory. To update
patterns, you must update the chart and redeploy.

**Option B: Patterns in values.yaml**

For patterns that operators can change without modifying the chart, use values:

```yaml
# values.yaml
filterPatterns:
  - "Module ID not recognized"
  - "No network segment defined for relay address"
```

```yaml
# templates/configmap.yaml (collector config section)
config:
  processors:
    filter/noise:
      error_mode: silent
      logs:
        log_record:
          {{- if .Values.filterPatterns }}
          {{- range .Values.filterPatterns }}
          - IsMatch(body, {{ . | quote }})
          {{- end }}
          {{- end }}
```

This approach lets operators add or remove filter patterns by updating Helm values
(`--set-json` or a values file override) without modifying the chart itself.

#### Routing logs to different pipelines

For more control, use a [routing connector](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/connector/routingconnector)
to send different log types to different pipelines. This lets you apply different filters
or export to different backends:

```yaml
connectors:
  routing/logs:
    default_pipelines:
      - logs/default
    table:
      # Route console logs to a separate pipeline
      - statement: route() where attributes["component"] == "nico-ssh-console-rs"
        pipelines:
          - logs/console-local    # Keep all locally
          - logs/console-remote   # Filter before remote export

service:
  pipelines:
    logs/input:
      receivers: [filelog]
      exporters: [routing/logs]

    logs/console-local:
      receivers: [routing/logs]
      processors: [batch]
      exporters: [loki/local]

    logs/console-remote:
      receivers: [routing/logs]
      processors: [filter/noise, batch]
      exporters: [loki/remote]

    logs/default:
      receivers: [routing/logs]
      processors: [batch]
      exporters: [loki/local]
```

#### Probabilistic sampling

For extremely high-volume logs where you only need a statistical sample:

```yaml
processors:
  probabilistic_sampler:
    sampling_percentage: 10   # Keep 10% of logs
```

Use sampling cautiously - you may miss the one error message that matters.

### 4.4 Backend-specific notes

#### VictoriaLogs

VictoriaLogs accepts OTLP logs. Use the `otlphttp` exporter with `logs_endpoint`:

```yaml
exporters:
  otlphttp:
    logs_endpoint: http://victorialogs.monitoring.svc.cluster.local:9428/insert/opentelemetry/v1/logs
```

#### Grafana Loki

Loki works well with logfmt - it can parse key=value pairs natively in queries using
`| logfmt`. Set `loki.format: raw` in the resource processor to send logs unparsed, letting
Loki handle parsing at query time:

```yaml
processors:
  resource:
    attributes:
      - action: insert
        key: loki.format
        value: raw
      - action: insert
        key: loki.resource.labels
        value: k8s.namespace.name, k8s.pod.name, k8s.container.name
```

Query example:

```text
{k8s_container_name="nico-api"} | logfmt | level="ERROR"
```

#### Elasticsearch / OpenSearch

Parse logfmt at ingest time (in the collector or an ingest pipeline) to get structured
documents. This enables efficient field-based queries and aggregations.

#### Datadog

Use the Datadog exporter or OTLP endpoint. Datadog automatically parses common log formats.

---

## 5. Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| No logs from a component | Container not running, or stdout not captured | `kubectl logs <pod>` to verify. Check container status. |
| Logs not reaching backend | Collector not running, or exporter misconfigured | Check collector logs: `kubectl logs -l app=opentelemetry-collector`. Verify exporter endpoint. |
| Missing fields in backend | logfmt not parsed | Add a transform processor to parse logfmt, or use `\| logfmt` operator in Loki queries. |
| Too many DEBUG logs | `RUST_LOG` set too verbose, or runtime filter left on | Check `RUST_LOG` env var. For nico-api, runtime filter auto-expires; wait or set a less verbose filter. |
| Log level change didn't take effect | Changed wrong component, or typo in filter | Runtime changes only work for nico-api. Verify filter syntax matches `EnvFilter` rules. |
| Logs are truncated | Log line too long for collector buffer | Increase `max_log_size` in filelog receiver config. |

### Verifying log output

To see raw logs from a NICo component:

```bash
# Stream logs from nico-api
kubectl logs -f deploy/nico-api

# Last 100 lines from nico-dns
kubectl logs --tail=100 deploy/nico-dns

# All containers in a pod
kubectl logs <pod-name> --all-containers
```

For more advanced log tailing across multiple pods, use [stern](https://github.com/stern/stern):

```bash
# Stream logs from all nico-api pods
stern nico-api

# Filter by log level (logfmt)
stern nico-api --include 'level=ERROR'

# Stream logs from multiple components
stern 'nico-(api|dns|dhcp)'

# Include timestamps and pod name
stern nico-api -t
```

stern is particularly useful when you have multiple replicas or want to watch several
components at once.

### Checking current log level (nico-api)

The startup log shows the initial log level. Look for lines like:

```text
level=INFO msg="current log level: INFO" location="setup.rs:142"
```

Note that this shows the coarse `LevelFilter` (such as INFO), not the full directive string.
There is currently no `get log-filter` command in nico-admin-cli to query the active filter
at runtime.

---

## 6. References

- [tracing-subscriber EnvFilter syntax](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html)
- [OpenTelemetry Collector filelog receiver](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/receiver/filelogreceiver)
- [logfmt specification](https://brandur.org/logfmt)
- [stern - Multi-pod log tailing for Kubernetes](https://github.com/stern/stern)
