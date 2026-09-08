# NICo Tracing

How NICo component tracing works, what it covers, how to turn it on and off and what it costs.

---

## TL;DR

- **nico-api** (the `carbide-api` binary) is NICo's primary tracing source and the subject of this
  document. **nico-dns** also emits traces, but with a separate simpler always-on setup.
  **nico-bmc-proxy** emits traces for each proxied BMC request when configured (see
  [nico-bmc-proxy tracing](#nico-bmc-proxy-tracing)).
- **nico-api traces are off by default**; two things must both be true before any spans are emitted:
  - An OTLP endpoint is configured at startup, either in the nico-api config TOML:

      ```toml
      [tracing]
      otlp_endpoint = "http://<otel_endpoint_host>:4317" # gRPC (default port 4317)
      ```

    or with `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, which overrides the TOML value.
  - Tracing is enabled, either in the same config section with `enabled = true`, or at runtime
    with `nico-admin-cli set tracing-enabled true` when `tracing.allow_runtime_changes = true`.
- Tracing is **resource-intensive when on**, so turn it on for a debugging session and then off after.

    ```bash
    # Once the endpoint is configured and runtime changes are allowed:
    nico-admin-cli set tracing-enabled true     # start capturing

    # ... reproduce the issue, examine traces in your backend ...
    nico-admin-cli set tracing-enabled false    # stop capturing traces
    ```

  Leaving the OTLP endpoint configured while tracing is disabled costs almost nothing.
- Transport is **OTLP/gRPC, plaintext**; nico-api cannot do OTLP/HTTP or originate TLS
- nico-api **propagates W3C trace context** at its network boundaries: it reads `traceparent`/
  `tracestate` from inbound REST and gRPC requests and continues that trace, injecting the same
  headers into its outbound requests. Propagation links traces across services, but does not by itself
  enable recording (see [W3C trace-context propagation](#w3c-trace-context-propagation)).

---

## 1. How tracing works

### Which components emit traces

The following binaries build an OTLP span exporter:

- **nico-api** (`crates/api-core/src/logging/setup.rs`) - the rich, control-plane tracing this
  document is mostly about, off by default behind endpoint plus enabled-flag configuration
- **nico-dns** (`crates/dns/src/main.rs`) - a separate, much simpler **always-on** setup.
- **nico-bmc-proxy** (`crates/bmc-proxy/src/setup.rs`) - one span per proxied BMC request, off by
  default behind endpoint plus `[tracing] enabled` (see
  [nico-bmc-proxy tracing](#nico-bmc-proxy-tracing)).

The other binaries (nico-pxe, nico-dhcp, nico-hardware-health, nico-ssh-console-rs, and
nico-dsx-exchange-consumer) carry the OpenTelemetry crates in the workspace but do not build a span
exporter, so they do not emit traces.

Unless noted otherwise, the rest of this document describes **nico-api** tracing.
nico-dns differs as described in [NICO DNS tracing](#nico-dns-tracing-separate-and-always-on).

### What operations are covered

nico-api links many library crates in-process and the `#[tracing::instrument]` spans live in
those crates. When tracing is enabled, the instrumented operations are:

| Area | Crate | Operations (span sites) |
|---|---|---|
| **Hardware component management** | `component-manager` | `power_control`, `update_firmware` / `queue_firmware_updates`, `get_firmware_status`, `list_firmware(_bundles)` across three backends - **NSM**, **PSM** (power-shelf), **RMS** (rack). Each span carries `backend="nsm\|psm\|rms"`. |
| **Reconcile controllers** | `machine-controller`, `switch-controller`, `power-shelf-controller` | `handle_object_state` (fields `object_id`, `state`). |
| **Discovery / infra** | `site-explorer`, `api-db` (migrations) | one span each. |
| **Database queries** | `sqlx-query-tracing` | wraps SQLx queries as spans. |

There is also a metric, `carbide_api_tracing_spans_open`, that reports the number of currently
open spans (exported by the `spancounter` crate) - useful for spotting span leaks or runaway
trace volume.

These cover the control-plane paths an operator most often needs to debug: machine
provisioning/reconcile loops, power control and firmware updates against the BMC/power/rack
backends, plus the database work underneath them - which maps directly to the EPIC's
"time on a given state of the machine, nodes stuck" need.

### How spans are selected (sampler)

nico-api uses a custom `CarbideSpanSampler`:

- A **root span** is recorded only if both are true:
  - the in-process `tracing_enabled` flag is on, from `[tracing] enabled = true` at startup or
    from the dynamic `tracing-enabled` setting
  - the span carries the `carbide.trace_root` marker attribute, set explicitly on the request span and a few
    deliberate roots (the state-controller reconcile loops and site-explorer)
- **In-process child spans inherit the root's decision**, so once a trace is sampled the whole call tree beneath
  it is captured - **except tokio spans, which are always dropped** (they leak and would exhaust memory).
- For a span parented to a **remote** (ingress-extracted) trace, the decision stays local: an inbound `sampled`
  flag does not override `tracing_enabled` (see
  [W3C trace-context propagation](#w3c-trace-context-propagation)).
- The exporter resource is `service.name = carbide-api`; the tracer is named `carbide`.

### How traces leave nico-api

nico-api pushes spans over **OTLP/gRPC** to a collector endpoint you configure. It does not
discover or get injected with anything - it simply connects out to the endpoint from
`[tracing] otlp_endpoint` or, if set, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. The environment
variable overrides the TOML value. The transport details: gRPC-only, plaintext.

### nico-dns tracing (separate and always-on)

nico-dns has its own tracing setup (`crates/dns/src/main.rs`), independent of and simpler than
nico-api's:

- **Always on.** nico-dns builds the span exporter unconditionally at startup - there is no
  endpoint env-var check and no `tracing-enabled` switch. If the process runs, it is exporting.
- **Endpoint from config, with a default.** The target is the `otlp_endpoint` config field
  (`crates/dns/src/config.rs`), which defaults to
  `http://opentelemetry-collector.otel.svc.cluster.local:4317`. Because of that default, nico-dns
  tries to export out of the box
- **Default sampler.** It uses the OpenTelemetry SDK's default sampler (no `CarbideSpanSampler`),
  so it records broadly, filtered only by the log-level directives in its `EnvFilter`. It
  instruments `retrieve_records`, among others.
- **Resource / output:** `service.name = carbide-dns`; logs are JSON on stdout (not logfmt).
- **Same transport constraints:** OTLP/gRPC, plaintext (`with_tonic`, no `tls` feature)

### nico-bmc-proxy tracing

nico-bmc-proxy traces each proxied Redfish request through the BMC credential proxy
(`crates/bmc-proxy/src/bmc_proxy.rs`). It follows the same W3C propagation model as nico-api
(issue [#2438](https://github.com/dsx-ai-factory/infra-controller/issues/2438)) so a call from nico-api or
DPS stays one trace across the proxy hop (issue
[#2355](https://github.com/dsx-ai-factory/infra-controller/issues/2355)).

- **Off by default.** Spans are exported only when an OTLP endpoint is configured **and**
  `[tracing] enabled = true` (or the process is started with `--debug`). There is no runtime
  toggle on this binary.
- **Environment variable override.** Tracing can be enabled via environment variable using the
  `NICO_BMC_PROXY__TRACING__ENABLED=true`. The double underscore (`__`) maps to nested TOML sections,
  so `NICO_BMC_PROXY__TRACING__ENABLED` overrides `[tracing] enabled`. This prefix takes precedence
  over TOML configuration.
- **Endpoint.** Set the standard `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, or
  `OTEL_EXPORTER_OTLP_ENDPOINT` to cover every signal at once; the trace-specific variable wins when
  both are set. `[tracing] otlp_endpoint` in the proxy TOML is the fallback for when neither variable
  is set. The remaining standard OTLP transport settings (`OTEL_EXPORTER_OTLP_TIMEOUT`,
  `OTEL_EXPORTER_OTLP_COMPRESSION`, `OTEL_EXPORTER_OTLP_HEADERS`, ...) are read by the exporter
  itself and apply as well. A malformed endpoint is rejected when the
  exporter is built; the proxy logs a warning and serves BMC traffic without tracing rather than
  refusing to start.
- **Ingress.** Each proxied request opens a `bmc_proxy_request` span and adopts any inbound
  `traceparent`/`tracestate` via `trace_propagation::set_span_parent_from_headers`.
- **Egress to BMC.** Upstream Redfish calls use a `reqwest-tracing` client so the active proxy
  span's W3C context is injected on the BMC leg. The inbound headers are dropped before the upstream
  request is assembled — `trace_propagation::is_propagated_header` asks the configured propagator
  which headers are its own — so the BMC parents under the proxy's span rather than the caller's.
- **Egress to nico-api (gRPC).** Credential lookup uses the shared `ForgeApiClient`, which already
  wraps the transport with `TraceInjectService`.
- **Resource / tracer:** `service.name = nico-bmc-proxy`, tracer name `nico-bmc-proxy`.
- **Span fields:** HTTP method and request path, the status the proxy answered its caller with (not
  the BMC's — a request the proxy rejects never reaches one), and BMC target IP (span attribute, not
  a Prometheus label). Only a 5xx sets the span status to error; a 4xx is the caller's error.

Example config:

```toml
[tracing]
enabled = true
otlp_endpoint = "http://otel-collector.observability.svc.cluster.local:4317"
```

Point this at the same collector nico-api uses: spans only join into one trace if every
hop's exporter reaches the same backend. The components stay distinguishable by their
`service.name`.

### W3C trace-context propagation

nico-api accepts and produces **W3C Trace Context** headers (`traceparent` and `tracestate`) at its
network boundaries, so a request already traced by another service stays one trace as it passes
through nico-api. The standard `TraceContextPropagator` is installed once at startup
(`crates/api-core/src/logging/setup.rs`); there is no custom header parsing.

- **Ingress (REST + gRPC).** The shared per-request layer (`crates/api-core/src/logging/api_logs.rs`)
  extracts any inbound `traceparent` or `tracestate` and makes the upstream span the parent of nico-api's
  request span. REST and gRPC flow through this single layer, so both are covered. A missing or
  malformed `traceparent` leaves the request span a fresh root.
- **Egress.** When nico-api makes an outbound call from within a traced request, it injects the
  current `traceparent` and `tracestate` so the downstream service can continue the trace. Covered:
  - **gRPC** - Forge and NMX-C (`crates/rpc`), the NSM and power-shelf (PSM) backends
    (`crates/component-manager`), and the NMX-C client pool (`crates/libnmxc`), through a shared tower
    layer applied to every request.
  - **HTTP** - the BMC/Redfish handler, machine-identity token exchange, admin-UI OAuth2, NRAS,
    the MQTT OAuth2 token provider, and firmware downloads.
- **Interaction with the enable flag.** `tracing-enabled` is the master switch for what nico-api
  *records*: an inbound `sampled` flag never turns recording on here. When `tracing-enabled` is on, the inbound
  `trace_id` is inherited, so nico-api's spans join the caller's trace.
- **Forwarding vs. recording.** Forwarding the context is separate from recording it, but both currently
  depend on the exporter being built:
  - *Exporter built, tracing off:* records nothing, yet still forwards the inbound `trace_id` and `tracestate`
    marked **not sampled** (`sampled=0`).
  - *No endpoint configured (exporter not built):* does **not** forward at all, so the trace **breaks** at
    this hop. **This is a known limitation.**
- **Scope.** Trace context only (`traceparent` or `tracestate`).

### Adding a new network client

Propagation is automatic on ingress but opt-in on egress. Keep the following in mind when adding code:

- **New ingress (a REST route or gRPC method): nothing to do.** Every inbound request flows through the
  shared per-request layer (`crates/api-core/src/logging/api_logs.rs`), which extracts the inbound context
  for you.
- **New outbound gRPC client (tonic/hyper):** wrap its channel/service with
  `trace_propagation::TraceInjectService` at construction. Better yet, build through an existing shared
  client that already wraps the transport (see `crates/rpc/src/forge_tls_client.rs`).
- **New outbound HTTP client (`reqwest`):** build it through the `reqwest-tracing` middleware instead of using a
  bare `reqwest::Client`. The wrapped client injects the current `traceparent` and `tracestate` into every request
  automatically, so there is no per-call code:

  ```rust
  let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new())
      .with(reqwest_tracing::TracingMiddleware::default())
      .build(); // -> reqwest_middleware::ClientWithMiddleware, a drop-in for request-building
  let resp = client.get(url).send().await?;
  ```

  See `crates/nras/src/client.rs` for a real example.
- **When another crate owns the HTTP call (manual fallback):** if the request is built and sent by code you
  don't control (for example, the `oauth2` client (`crates/api-web/src/auth.rs`), which owns its own `reqwest`
  request) inject into that request's headers directly:

  ```rust
  trace_propagation::inject_current_context(request.headers_mut());
  ```

Injection is always a no-op when no trace is active, so it is safe to add it unconditionally.

---

## 2. How to enable and disable tracing

Enabling tracing has **two parts**: startup configuration for the exporter endpoint, and an enabled
flag that can come from startup config or, when allowed, the runtime switch. An endpoint without the
enabled flag emits no traces. The enabled flag without an endpoint also emits no traces because no
OTLP exporter is built.

```text
 Startup configuration                             Enable/disable policy
 ┌───────────────────────────────┐                  ┌─────────────────────────────────────┐
 │ a. a traces backend           │                  │ [tracing] enabled = true|false      │
 │ b. a collector to receive     │   ── then ──▶    │ and optionally:                     │
 │    OTLP from nico-api         │                  │ nico-admin-cli set tracing-enabled  │
 │ c. [tracing] otlp_endpoint    │                  │   true|false                        │
 │    or OTEL_EXPORTER... env    │                  │ if allow_runtime_changes = true     │
 └───────────────────────────────┘                  └─────────────────────────────────────┘
```

### Deploy-time configuration

**(a) A traces backend.** Anything that accepts OTLP traces: e.g. Tempo, Jaeger, Grafana Cloud,
Datadog, Elastic APM or another OTEL collector acting as a gateway.

**(b) A collector to receive OTLP from nico-api.** nico-api should send to a collector, not
straight to the backend - the collector is where you do sampling, batching, attribute
normalization and (importantly) TLS for anything leaving the cluster. There are two common
ways to give nico-api a collector to talk to:

*Option A - a shared collector* (Deployment or DaemonSet) that many workloads send to. A minimal
**otel-collector** `traces` pipeline:

```yaml
receivers:
  otlp:
    protocols:
      grpc: { endpoint: 0.0.0.0:4317 }   # nico-api connects here

processors:
  memory_limiter:
    check_interval: 1s
    limit_percentage: 75
    spike_limit_percentage: 20
  tail_sampling:              # optional but recommended; keeps trace volume sane
    decision_wait: 10s
    policies:
      - name: errors
        type: status_code
        status_code: { status_codes: [ERROR] }
      - name: slow
        type: latency
        latency: { threshold_ms: 500 }
      - name: probabilistic-baseline
        type: probabilistic
        probabilistic: { sampling_percentage: 5 }
  batch/traces:
    send_batch_size: 1024     # keep batches small if the backend is Tempo (gRPC msg-size limits)
    send_batch_max_size: 2048

exporters:
  otlp/traces:
    endpoint: <backend-host>:4317   # Tempo / Jaeger / Grafana Cloud / Datadog / Elastic OTLP
    tls: { insecure: true }         # in-cluster plaintext; set real TLS/mTLS per backend
    retry_on_failure: { enabled: false }   # best-effort; don't queue traces if backend is down

service:
  pipelines:
    traces:
      receivers:  [otlp]
      processors: [memory_limiter, tail_sampling, batch/traces]
      exporters:  [otlp/traces]
```

With Option A, nico-api's endpoint is the collector's in-cluster Service, e.g.
`http://otel-collector.observability.svc.cluster.local:4317`.

*Option B - a per-pod sidecar collector injected by the OpenTelemetry Operator.* If your cluster
runs the [OpenTelemetry Operator](https://github.com/open-telemetry/opentelemetry-operator), you can have it inject a collector container into the nico-api
pod via a pod annotation. nico-api then talks to the collector over `localhost` (same pod, same network namespace)

The annotation value follows the form **`<namespace>/<collector-name>`**:

```yaml
# nico-api pod template
metadata:
  annotations:
    sidecar.opentelemetry.io/inject: "observability/otel-sidecar"
spec:
  template:
    spec:
      containers:
        - name: nico-api
          env:
            - name: OTEL_EXPORTER_OTLP_TRACES_ENDPOINT
              value: http://localhost:4317   # overrides [tracing] otlp_endpoint
```

**(c) Point nico-api at the collector.** nico-api builds its OTLP span exporter **only if** an
endpoint is configured at startup. If no endpoint is configured, no tracing layer is constructed at
all and nothing is ever emitted - regardless of the enabled flag.

Preferred config-file form:

```toml
[tracing]
# Option A (shared collector): the collector's Service
otlp_endpoint = "http://otel-collector.observability.svc.cluster.local:4317"

# Option B (injected sidecar): the in-pod collector on localhost
# otlp_endpoint = "http://localhost:4317"
```

The deployment environment variable form is still supported and takes precedence over the TOML
endpoint:

```yaml
# nico-api container env (e.g. via the nico-api Helm values)
env:
  OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: http://otel-collector.observability.svc.cluster.local:4317
```

Notes:

- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is the **only** trace-related setting nico-api reads from
  the environment. Other standard OTEL env vars are **ignored**.
- The endpoint must be a **plaintext gRPC** target (`http://…`, h2c); 4317 is the default
  OTLP/gRPC port. Do not point it at a 4318 HTTP receiver and do not use `https://`.
- Configuring only the endpoint puts the plumbing in place but does **not** start emission on its
  own. `enabled` must also be true.

### Enable / Disable Policy

With the endpoint configured, emission is controlled by `[tracing] enabled`, which defaults
**off**:

```toml
[tracing]
otlp_endpoint = "http://otel-collector.observability.svc.cluster.local:4317"
enabled = true
allow_runtime_changes = true  # default; permits nico-admin-cli set tracing-enabled
```

When `allow_runtime_changes = true`, toggle tracing live without a restart:

```bash
# start capturing traces (e.g. while reproducing an issue)
nico-admin-cli set tracing-enabled true

# stop capturing, turn it back off when done
nico-admin-cli set tracing-enabled false
```

Under the hood this sets the dynamic config `ConfigSetting::TracingEnabled`, which flips the
in-process `tracing_enabled` flag that `CarbideSpanSampler` reads. If
`allow_runtime_changes = false`, the `SetDynamicConfig` call is rejected with `PermissionDenied`;
the startup value from `[tracing] enabled` remains authoritative until nico-api restarts with a new
config.

Leaving tracing **off** in steady state is the intended operating mode. If you need startup-only
control, set `allow_runtime_changes = false` and change `[tracing] enabled` through the config file
plus a pod roll.

### Do I need to restart nico-api?

It depends on which part you are changing:

| What you're doing | Restart needed? |
|---|---|
| Endpoint already set at startup and runtime changes allowed, want traces now | **No** - `nico-admin-cli set tracing-enabled true` |
| Turning tracing back off when runtime changes are allowed | **No** - `nico-admin-cli set tracing-enabled false` |
| Changing `[tracing] enabled` in config | **Yes** - startup config is read on process start |
| Changing `tracing.allow_runtime_changes` | **Yes** - runtime policy is read on process start |
| Adding or changing `[tracing] otlp_endpoint` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` | **Yes** - roll the nico-api pod once |
| Adding the OTEL sidecar-injection annotation | **Yes** - pod-spec change; injected only at admission |

Why: `[tracing] otlp_endpoint`, `[tracing] enabled`, `[tracing] allow_runtime_changes`, and
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` are read at process startup (`crates/api-core/src/logging/setup.rs`).
If no endpoint was configured when nico-api started, the OTLP exporter and tracing layer were never
constructed and there is no way to add them at runtime. The runtime switch, when allowed, only flips
an in-process flag and **never** needs a restart.

**Recommendation:** set `[tracing] otlp_endpoint` at deploy time and leave it in place permanently -
the plumbing is cheap while tracing is toggled off. Keep `enabled = false` and
`allow_runtime_changes = true` for debug-on-demand environments, or set
`allow_runtime_changes = false` when the config file should be the only control plane for tracing.

### Verifying it works

1. `[tracing] otlp_endpoint` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set on nico-api and points
   at the collector's gRPC endpoint.
2. The collector has a `traces` pipeline and its logs show the OTLP receiver listening on 4317.
3. `[tracing] enabled = true` is configured, or `nico-admin-cli set tracing-enabled true` has been
   run while `tracing.allow_runtime_changes = true`.
4. Exercise a traced operation (e.g. a machine power/firmware action), then look in your backend
   for spans with `service.name = carbide-api`.
5. Watch `carbide_api_tracing_spans_open` to confirm spans are being opened.

---

## 3. Downsides and overhead

Tracing has real cost, which is the reason it defaults off. The cost depends on
which of three states nico-api is in:

| State | nico-api overhead | I/O / network | Notes |
|---|---|---|---|
| Endpoint **unset** | **None** | None | No tracing layer is built at all. |
| Endpoint **set**, tracing **disabled** | **Near-zero** (small per-span bookkeeping) | None | Layer is installed but the sampler drops everything; nothing is recorded or exported. |
| Endpoint set, tracing **enabled** | **Significant** | Yes | Full recording + serialization + export. This is the "resource-intensive" mode. |

### When tracing is ON

This is the expensive mode the dev team warns about:

- Because a span's in-process children inherit its sampling decision, a sampled root span pulls in its **entire child subtree**
  (the component-manager, controller, and DB spans beneath it). A single traced
  operation can therefore produce many spans.
- Costs land in several places: extra **CPU and memory** on nico-api, added **latency** on
  instrumented hot paths, **network egress** to the collector and **storage** in the backend.
- Mitigate with `tail_sampling` at the collector (keep errors/slow traces, sample the rest) and -
  most importantly - **only enable it during an active investigation**, then turn it back off.

### When the endpoint is set but tracing is OFF

This is the common steady state if you follow the recommendation to leave the endpoint configured
with `[tracing] enabled = false`, or after disabling tracing dynamically. The overhead here is
**near-zero but not exactly zero**:

- At startup, because the endpoint is set, nico-api builds the OTLP exporter, a tracer provider
  with a batch span processor and installs the OpenTelemetry tracing layer into its subscriber
  stack. That layer stays present.
- Per span, the layer is invoked on each (non-tokio) instrumented span and does a little
  bookkeeping/allocation before the sampler returns "drop". A background batch task exists but
  idles.
- What does **not** happen: no span recording, no attribute serialization, no batches to flush,
  **no network or gRPC export**. There is no I/O.
- Net: a small, roughly constant per-span CPU cost - negligible next to the "on" mode, but not
  the literal zero you get with the endpoint unset.

### Practical guidance

- Leave `[tracing] otlp_endpoint` configured and keep tracing **off** in steady state - cheap and
  avoids a pod roll when you need traces.
- Treat "on" as a temporary debugging state. Turn it off when done; watch
  `carbide_api_tracing_spans_open` and nico-api CPU/latency while it is on.

---

## 4. How traces are sent (transport & security)

- nico-api speaks **OTLP/gRPC only** (no OTLP/HTTP).
- nico-api **cannot originate TLS or mTLS** for traces. The endpoint must be plaintext
- Therefore keep the **nico-api → collector hop local** (in-cluster Service, or the in-pod
  sidecar) and make the **collector the TLS boundary** for anything leaving the cluster.
- Traces are **push-based**: nico-api connects out to the collector. There is no scrape/discovery
annotation involved for traces

---

## 5. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| No traces at all, endpoint **is** set | Tracing is disabled | Set `[tracing] enabled = true` and roll nico-api, or run `nico-admin-cli set tracing-enabled true` if runtime changes are allowed |
| `nico-admin-cli set tracing-enabled ...` returns `PermissionDenied` | `tracing.allow_runtime_changes = false` | Change `[tracing] enabled` in config and roll nico-api, or set `allow_runtime_changes = true` and roll once |
| No traces at all, tracing **is** enabled | Endpoint not configured, so no exporter was built | Set `[tracing] otlp_endpoint` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` and roll the pod |
| nico-api can't connect / TLS errors | Endpoint uses `https://` or points at the 4318 HTTP port | Use plaintext `http://…:4317` (gRPC); nico-api has no TLS and no HTTP |
| Sidecar injected but still no traces | Endpoint not set, or points somewhere other than `localhost:4317` | Set `[tracing] otlp_endpoint = "http://localhost:4317"` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT: http://localhost:4317` on nico-api |
| Traces reach the collector but not the backend | Collector exporter endpoint/TLS wrong | Check the exporter config; for remote backends configure TLS/mTLS on the collector |
| Sudden resource/latency spike on nico-api | Tracing left on | `nico-admin-cli set tracing-enabled false`, or set `[tracing] enabled = false` and roll nico-api if runtime changes are disabled |
| Spans arrive but request trees look sparse | Only spans marked with `carbide.trace_root` start a recorded trace (see [Span Sampler](#how-spans-are-selected-sampler)) | Confirm the operation starts at a marked root span |

---

## 6. DPU workload tracing

DPU workloads (such as ovnkube-node) can emit OpenTelemetry tracing spans through the DPU's
otelcol-contrib collector. The collector provides a localhost-only OTLP/gRPC receiver that
forwards spans to the site-level OpenTelemetry receiver using the DPU's mTLS credentials.

### Endpoint configuration

| Setting | Value |
|---------|-------|
| Protocol | OTLP/gRPC |
| Endpoint | `127.0.0.1:4317` |
| TLS | Not required (loopback only) |

### Workload configuration

Configure your workload's OpenTelemetry exporter to send spans to the local collector:

```bash
# Environment variables (standard OTLP configuration)
export OTEL_EXPORTER_OTLP_ENDPOINT="http://127.0.0.1:4317"
export OTEL_EXPORTER_OTLP_PROTOCOL="grpc"
```

For Kubernetes workloads running on DPUs managed by DPF:

```yaml
# Pod spec environment variables
env:
  - name: OTEL_EXPORTER_OTLP_ENDPOINT
    value: "http://127.0.0.1:4317"
  - name: OTEL_EXPORTER_OTLP_PROTOCOL
    value: "grpc"
```

<Note>
The nico-otelcol DaemonSet runs with `hostNetwork: true`, so the loopback endpoint
`127.0.0.1:4317` is reachable only from the host network namespace. This works for
workloads like ovnkube-node that also use `hostNetwork: true`. Workloads in pod
network namespaces cannot reach this endpoint.
</Note>

### Security

- The OTLP receiver binds only to loopback (`127.0.0.1`), preventing access from outside the node
- Loopback does not authenticate callers - any process in the host network namespace can send spans
- Workloads do not need access to DPU mTLS credentials
- The collector authenticates to the site-level receiver using existing mTLS configuration

### Resource attributes

Spans exported through this pipeline include:

| Attribute | Source | Description |
|-----------|--------|-------------|
| `host.name` | `resourcedetection` | DPU hostname |
| `machine.id` | `fileresource` | NICo machine ID |
| `host.machine.id` | `fileresource` | Host machine ID |
| `component` | `resource/traces-workloads` | Set to `dpu-workloads` |

---

## 7. References

- [NICo core metrics catalogue](core_metrics.md) - includes `carbide_api_tracing_spans_open`.
