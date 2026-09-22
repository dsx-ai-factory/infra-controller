# NICo Instrumentation <Badge intent="info">v2.1</Badge> <Badge intent="launch" minimal>New</Badge>

How to instrument significant events with `carbide-instrument`: this framework provides a single declaration that
produces a structured log line, a Prometheus metric, or both -- correlated, consistently
named, and with metric cardinality bounded by the type system.

This guide is for contributors adding or migrating Rust instrumentation. For the
operator-facing references, refer to [Core Metrics](core_metrics.md) and [Logging](logging.md).

---

## TL;DR

- **Just logging words? Keep using `tracing::`.** `info!`/`warn!`/`error!` with structured
  fields stays the right tool for plain logs; nothing migrates for its own sake.
- **Need a count, rate, or duration? Declare an `Event` and `emit()` it.** The event's two
  options -- `log = error|warn|info|debug|trace|off` and `metric = counter|histogram|none` --
  cover metric-only, log-only, or both from the same call.
- **Every Event has a stable identity.** `event_name` is a unique, flat `lower_snake_case`
  category for searches and schemas. Metric-backed Events separately declare `metric_name`;
  Event-generated logs include both names when both sides emit.
- **Cardinality is enforced by the types.** `#[label]` fields must be bounded via
  `LabelValue` -- usually a fieldless enum, with a manual impl on a bounded newtype as the
  reviewed escape hatch; high-cardinality detail (`machine_id`, IPs, error text) goes in
  `#[context]` fields, which appear only when the Event emits a log line and *cannot* become
  metric labels.
- **A checked `metric_name` in the attribute is the exposed name, verbatim** -- what you grep
  on a dashboard is the string in the source. The derive validates it at compile time
  (`carbide_` prefix, `_total` for counters, a unit suffix for histograms).
- **Point-in-time state (gauges) is unchanged.** The framework models *occurrences*; observable
  gauges and `SharedMetricsHolder` snapshots stay exactly as they are.

Part of the instrumentation-coherency initiative
([#3169](https://github.com/dsx-ai-factory/infra-controller/issues/3169)).

---

## When to use carbide-instrument

| You want | Use | Why |
|---|---|---|
| A plain log line | `tracing::info!(%machine_id, "...")` | No metric required. Most log sites stay exactly like this. |
| A failure you'd alert on | `emit(...)` with `log = warn, metric = counter` | The counter is the alert; the log line (same labels + context) provides the details. |
| A hot-path rate (per packet / per request) | `emit(...)` with `log = off, metric = counter` | The rate is the signal; no log line is built at all -- zero logging cost, and the noise is gone. |
| A duration or size distribution | `emit(...)` with `metric = histogram` | `#[observation]` supplies the value; the unit comes from the metric name. |
| "How many are in state X right now" | An observable gauge (existing pattern) | State is not an occurrence. Keep `SharedMetricsHolder` + `u64_observable_gauge`. |

**Adoption is opt-in** and call-site-by-call-site. Existing `tracing::` sites and existing
metric emitter structs keep working unchanged; when a site *does* migrate, its log line
must preserve the existing level, message, and domain field keys except where a key conflicts
with the [reserved Event-log fields](#potential-hazards) described below. An existing metric
keeps its family name, kind, unit, HELP text, label keys, and label values. Labels and context
render as ordinary logfmt fields, so preserving those contracts keeps existing greps and
dashboards working. Event-generated logs also gain the stable identity fields, and a
metric-backed migration gains the declared metric.

A pre-migration `?field` uses Debug rendering, for which Events have no `#[context(debug)]`
equivalent. Keep that site on `tracing::`, or define a deliberate stable Display/value
representation before migrating it.

## Quick start

Declare the event next to the code that emits it:

```rust
use carbide_instrument::{emit, Event, LabelValue, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum Backend {
    Nsm,
    Psm,
    Rms,
}

#[derive(Event)]
#[event(
    event_name  = "power_control_failed",       // stable event category
    metric_name = "carbide_power_control_total", // exposed metric, verbatim
    component   = "component_manager",
    log         = warn,                          // error|warn|info|debug|trace|off
    metric      = counter,                       // counter | histogram | none
    message     = "power control failed",
    describe    = "Number of power control operations that failed",
)]
struct PowerControlFailed {
    #[label]
    backend: Backend, // enum -> label backend="rms" (metric AND log)
    #[label]
    outcome: Outcome, // the framework's shared ok|error vocabulary
    #[context]
    bmc_ip_address: std::net::IpAddr, // log-only, never a metric label
    #[context]
    error: String, // log-only
}

if let Err(e) = backend.power_control(&target, action).await {
    emit(PowerControlFailed {
        backend: Backend::Rms,
        outcome: Outcome::Error,
        bmc_ip_address,
        error: e.to_string(),
    });
}
```

For this Event, one `emit()` writes both the log line and the metric. The log line includes
the surrounding span's `span_id`; the metric is an aggregate with no per-request identity,
so correlation runs the other way: pivot from the moving metric to the matching log lines by
metric name and label values, and `span_id` then ties each line to its request:

```logfmt
level=WARN component=nico-api span_id=0x4f... event_name=power_control_failed metric_name=carbide_power_control_total msg="power control failed" backend=rms outcome=error bmc_ip_address=10.0.0.5 error="deadline exceeded" location="..."
```

```text
carbide_power_control_total{backend="rms",outcome="error"} 1
```

The declaration's `component` names the owning subsystem for source tooling and tests. The
declaration value is not written as a log field; runtime logfmt `component` still comes from
subscriber configuration and surrounding spans, which is why the example log line above uses
`component=nico-api`.

Install the meter provider once at startup **before the first metric-backed `emit()` or
`initialize_counter_series()` call** (every NICo binary already does this for its existing
metrics). The generated instrument resolves from the global meter once per Event type and is
then cached, so a first use against the default no-op provider leaves that Event type on the
no-op instrument for the rest of the process. Production NICo binaries follow an install-once
provider contract; cached Event instruments do not rebind after a later provider replacement.

## Log and metric options

Every event declares its log side and its metric side independently:

| `#[event(...)]` | Log line? | Metric? | Use for |
|---|---|---|---|
| `log = warn, metric = none` | Yes | No | A typed structured log (rare; plain `tracing::` is usually fine) |
| `log = warn, metric = counter` | Yes | Yes | A failure you alert on *and* read |
| `log = off, metric = counter` | No | Yes | Hot paths where the rate is the signal |
| `log = off, metric = histogram` | No | Yes | High-frequency latency as a distribution only |

`log = off` constructs no `tracing` event at all -- it is not "logged then filtered".
It still has a declared `event_name` for source-level identity and future cataloguing, but
there is no log record on which to render that field.

For per-instance control (count everything, log only failures), declare `log = dynamic`
and implement `DynamicLog` -- the derive routes `Event::log_at()` through it:

```rust
impl DynamicLog for CallFinished {
    fn log_at(&self) -> LogAt {
        match self.outcome {
            Outcome::Error => LogAt::Level(tracing::Level::WARN),
            Outcome::Ok => LogAt::Off, // counted, never logged
        }
    }
}
```

When the per-case *wording* matters, not just the level, declare `message = dynamic`
and implement `DynamicMessage`; the derive routes `Event::message()` through it:

```rust
impl DynamicMessage for CallFinished {
    fn message(&self) -> &'static str {
        match self.outcome {
            Outcome::Error => "outbound call failed",
            Outcome::Ok => "outbound call finished",
        }
    }
}
```

The level and the message are independent: an event can pair a dynamic level with a static
message, or the reverse. Prefer a static `message` plus a `#[label]` where the label already
names the case. Use `message = dynamic` only when the wording says something the label does not.

## Outbound calls

Every generated gRPC client method is already wrapped: it records
`carbide_external_call_duration_milliseconds{backend, operation, outcome}` on every
completion (the histogram's `_count` is the request and error rate) and writes one WARN --
with the error as log-only context -- on failure. For other outbound boundaries
(Redfish, HTTP, IPMI), wrap the call directly:

```rust
let response = carbide_instrument::red::instrumented("redfish", "power_control",
    client.power_control(&target, action)).await?;
```

The `backend`/`operation` labels are `&'static str` on purpose: compile-time literals
only, never values from the wire -- the type is the cardinality guard. Streaming calls
record time to the stream handle, not the stream's lifetime.

## Rules for labels and context

A metric's time-series count is the product of its label domains, so every label domain
must be small and closed. The framework makes that structural instead of a review checklist:

- **`#[label]` fields must implement `LabelValue`**, which is derivable **only for
  fieldless enums**. A derived label value is the variant's snake_case name.
  `String` never implements it.
- **A frozen metric label key that collides with a reserved Event-log field** can use the
  narrow `#[label(name = "component")] publisher: PublishComponent` compatibility form.
  `name` changes only the Prometheus label key; the generated log uses the Rust field name
  (`publisher`). Use a bare `#[label]` everywhere else, and do not use this to rename
  context or observation fields.
- **`#[context]` fields take anything `Display`** and appear only when the Event emits a log
  line. This is where `machine_id`, addresses, and error text belong. A context field cannot
  become a metric label. Use **`#[context(value)]`** only for `bool`, `i64`, `f64`, or
  `String` fields that must retain their native structured type instead of being rendered
  through `Display`; convert other numeric widths only with a checked, lossless conversion.
  When that cannot be guaranteed, keep the default `Display` formatting.
- **Bounded-but-not-enumerated values** such as vendor strings or SKUs can go through a
  **manual `impl LabelValue` on a newtype** -- the deliberate, greppable escape hatch, and
  the place to justify boundedness at review. The deciding factor should be real boundedness *at the call
  site*: a raw request-path segment is not bounded even when a proto surface suggests it
  should be -- caller-supplied values mint unbounded series. When in doubt, keep the value
  in `#[context]` and count without it.
- Per-object metric series remain the exception, and they stay on the opt-in,
  hold-time-bounded `PerObjectMetricsRegistry` -- not on event labels.

### Per-object state endpoint

State-controller object IDs are intentionally high-cardinality. Their current
state, resolved SLA, intervention status, stable traits, and machine
associations are therefore observable gauges on a dedicated, disabled-by-default
Prometheus endpoint rather than event labels or the main `/metrics` endpoint.
The metric catalog is:

- `carbide_object_state_entered_timestamp_seconds`
- `carbide_object_state_sla_seconds`
- `carbide_object_manual_intervention_required`
- `carbide_object_info`
- `carbide_machine_dpu_info`
- `carbide_machine_instance_info`

Enable and select object types with
`[observability.per_object_state_metrics]`. Scrape it slowly (normally
60–120 seconds), and scrape both endpoints into the same Prometheus before
joining these series with aggregate or health metrics.
Refer to [Per-object state progress metrics](../operations/monitoring-health.md#per-object-state-progress-metrics) for more details.

## Histograms and observations

A histogram event has exactly one `#[observation]` field: a `Duration` (converted to
the unit the metric name declares) or a plain number (recorded as-is).

```rust
#[derive(Event)]
#[event(
    event_name = "artifact_transfer_finished",
    metric_name = "carbide_artifact_transfer_duration_seconds",
    component = "artifact-transfer",
    log = info,
    metric = histogram,
    message = "Artifact transfer finished",
    describe = "Artifact transfer duration",
)]
struct ArtifactTransferFinished {
    #[label]
    outcome: Outcome,
    #[context(value)]
    duration_seconds: f64, // searchable on the log line
    #[observation]
    duration: std::time::Duration, // metric only; converted to seconds
    #[context]
    host_ip_address: std::net::IpAddr,
}

impl ArtifactTransferFinished {
    fn new(
        outcome: Outcome,
        duration: std::time::Duration,
        host_ip_address: std::net::IpAddr,
    ) -> Self {
        Self {
            outcome,
            duration_seconds: duration.as_secs_f64(),
            duration,
            host_ip_address,
        }
    }
}
```

`#[observation]` feeds the histogram and is not rendered on the log line. When the measured
value must also be searchable in logs, record it separately as a native structured
`#[context(value)]` field, as the example does. A histogram already exports a `_count` series,
so it never needs a twin counter. Construct the context and observation from the same source
value when building the Event, so the logged duration cannot drift from the recorded one.

## Naming conventions

`event_name` and `metric_name` serve different contracts:

- `event_name` identifies a reusable semantic event category. It is a unique ASCII
  `lower_snake_case` literal, starts with a letter, and has no dot namespace, component
  prefix, severity, or unit. Add a semantic qualifier only when distinct Events would
  otherwise collide. Changing it breaks saved log searches and future schemas.
- `metric_name` is required exactly when `metric != none`. A checked name is the exposed
  Prometheus name, verbatim; legacy compatibility uses `metric_name_unchecked` as described
  below.

`cargo xtask check-event-names` verifies that production Event declarations have unique
static event names. One Event declaration may still be emitted from any number of call
sites; uniqueness is about declarations, not occurrences.

For `metric_name`, the derive enforces these conventions at compile time:

- All new metrics use the `carbide_` prefix.
- Counter names have a `_total` suffix -- and only one: the Prometheus exporter appends `_total`,
  so an instrument name that already ends in `_total` (a doubled `_total_total`) is rejected.
- Histograms end in their unit: `_seconds`, `_milliseconds`, `_microseconds`, `_bytes`.
- Gauge names (existing pattern, not the framework) are mixed legacy forms; follow established
  neighboring names rather than a single suffix rule.

In every declaration, `metric_name` states the intended operator-facing family on `/metrics`,
not the internal OpenTelemetry instrument name. For a checked Event, the derive guarantees
that family round-trips verbatim. The framework strips one declared counter `_total` at
registration and the exporter restores it; the derive rejects a missing or doubled `_total`.
For a checked histogram, the derive infers the unit from its required suffix, then the
framework strips that suffix at registration before the exporter restores it and adds the
normal `_bucket`, `_sum`, and `_count` series.

**Existing metric contracts never change.** `metric_name_unchecked` bypasses the new-name
validation for a frozen declaration, but it does not disable the exporter's suffix handling.
An unchecked counter that already ends in `_total` round-trips verbatim; one without `_total`
gains that suffix. An unchecked histogram with a recognized suffix infers its unit and also
round-trips verbatim; one without a recognized suffix requires an explicit `unit = "..."` and
can gain the exporter's unit suffix. An explicit histogram `unit` overrides suffix inference,
so a conflicting unit can append another suffix. Verify the exposed `/metrics` family before
migrating, and do not accept a renamed family as part of the conversion. Use this escape
hatch only for a reviewed existing contract; new metrics use the checked form.

An Event that can log keeps a required, stable, human-readable `message`. `event_name` is
the machine contract while `message` is presentation for people and UIs; occasional wording
overlap is fine. Metric-only Events require no message because they construct no log line.

A counter documents itself: its `describe = "..."` is required and opens with "Number of ..." (the
tech-writer house rule, enforced by the derive). The text becomes the Prometheus HELP and the
Description column of the [Core Metrics](core_metrics.md) catalogue. A grandfathered describe
keeps its wording with `describe_unchecked` -- the counterpart to `metric_name_unchecked` -- and a
search for either finds every opt-out. A histogram takes any `describe` (or none); a log-only event
(`metric = none`) has no metric to document, so it must omit `describe`.

The catalogue is regenerated by `test_integration`, which scrapes the carbide-api integration
environment's `/metrics`, and checked in CI. Because a metric no test exercises is never
scraped, `cargo xtask check-metric-docs` also reads the `#[event(...)]` declarations directly
and fails if any production framework counter or histogram -- other than a
`metric_name_unchecked` compatibility metric, whose exposed family can be transformed by the
exporter -- is missing a catalogue row. An unchecked metric can enter through the current
generator only when it appears in that carbide-api scrape; compatibility metrics owned by
other binaries remain outside the catalogue. New checked metrics cannot land undocumented.
Precisely matching unchecked declarations to exported families remains tracked in
[#3221](https://github.com/dsx-ai-factory/infra-controller/issues/3221).

## Startup zero series

Most Event series appear after their first occurrence. When an existing metric contract
requires a counter's bounded label set to be exposed at zero during startup, call
`initialize_counter_series(&event)` after installing the meter provider:

```rust
let initialized = carbide_instrument::initialize_counter_series(&event);
debug_assert!(initialized, "zero-series initialization requires a counter Event");
```

The function selects the series from the Event's labels, ignores its context, and does not
write the Event's log line. It returns `false` for a non-counter Event. Keep the initialization
call outside `debug_assert!`, or release builds will skip it. Call it once for each bounded
label combination the startup contract requires. Do not fake an `emit()` or register a second
counter for the same metric.

## Derivation outputs and costs

`#[derive(Event)]` is [thiserror](https://docs.rs/thiserror/latest/thiserror/)-style: a plain struct you can construct, test, and match,
with the semantics in attributes. The generated code:

- Builds labels as a fixed-size array (no heap allocation on emit) with enum values
  rendering as `&'static str`
- Caches the OTel instrument in a per-event-type `OnceLock` -- a metric-only emit is an
  atomic load plus an `add()`
- Emits the log via `tracing::event!` with real static field names, so `logfmt`, the
  admin-UI log stream, and every other subscriber layer see an ordinary tracing event in
  the surrounding span. Its tracing metadata name and structured `event_name` field are the
  declared event identity; a metric-backed Event also emits `metric_name` on that log line.
- Never panics

## Testing the coherency

Enable the `test-support` feature in the consuming crate's development dependencies:

```toml
[dev-dependencies]
carbide-instrument = { path = "../instrument", features = ["test-support"] }
```

The feature provides capture helpers, so "this event logged at WARN *and* ticked the
counter" is a plain unit test:

```rust
use carbide_instrument::testing::{capture_logs, MetricsCapture};

let metrics = MetricsCapture::start();
let logs = capture_logs(|| emit(PowerControlFailed { ... }));

assert_eq!(logs[0].level, tracing::Level::WARN);
assert_eq!(logs[0].metadata_name, "power_control_failed");
assert_eq!(logs[0].field("event_name"), Some("power_control_failed"));
assert_eq!(logs[0].field("metric_name"), Some("carbide_power_control_total"));
assert_eq!(
    metrics.counter_delta("carbide_power_control_total", &[("backend", "rms"), ("outcome", "error")]),
    1.0,
);
```

`MetricsCapture` serializes metric-asserting tests behind a process-global registry;
`capture_logs` is per-thread. `render()` prints the raw exposition text when a test needs
inspection.

## New metric-serving binaries

A new metric-serving binary wires up the fleet-wide `carbide_log_events_total` baseline in
two places: add `carbide_instrument::LogEventsMetric::new(component).layer()` to its tracing
subscriber before logs start, then call `carbide_instrument::log_events::register(&meter)` after
installing the meter provider. The Event derive does not install this baseline automatically.

## Potential hazards

- **`LogLimiter`-gated sites**: before migration, the limiter suppresses the log call
  before any event fires, so the true rate is invisible to everything -- including the
  framework. After migration onto an `Event`, the metric ticks on every occurrence and the
  limiter gates only the log line.
- **Test provider replacement**: some integration-test processes replace the global provider.
  Because Event instruments keep the first binding described above, a later setup's registry
  scrape can omit that Event. The current catalogue path retains existing rows and checks
  checked-name declarations from source, so it does not assume one scrape is complete. Use
  `MetricsCapture` for unit-level Event assertions; when regenerating a row that depends on a
  scrape, run the provider-owning integration test by itself in a fresh test process. Late
  provider-setup hardening is tracked in
  [#4174](https://github.com/dsx-ai-factory/infra-controller/issues/4174); it does not cover test-side
  provider replacement.
- **Events are occurrences.** Do not model state with counters; keep gauges on the
  existing observable-gauge pattern.
- **Reserved Event-log fields**: `message` is always reserved. Events that can log must
  also not declare payload fields named `msg`, `level`, `location`, `component`, `span_id`,
  `event_name`, or `metric_name`. Metric-only legacy labels remain allowed because renaming
  a Prometheus label would break its metric contract. When such a metric-only Event gains a
  log, preserve the label key with `#[label(name = "component")]` on a domain-specific Rust
  field such as `publisher`; only the metric key is aliased. A reserved context key has no
  alias, so rename it and update its log consumers, or leave the site on `tracing::`.

## References

- The initiative: [#3169](https://github.com/dsx-ai-factory/infra-controller/issues/3169)
  (unify logging and metrics behind a single instrumentation standard).
- The crate: `crates/instrument` (rustdoc on `Event`, `emit`, `LabelValue`, `testing`).
- The catalogue: [Core Metrics](core_metrics.md) -- the generated and source-checked
  metric-family reference.
- Conventions: [Prometheus metric and label naming](https://prometheus.io/docs/practices/naming/).
- Neighbors: [Logging](logging.md), [Traces](tracing.md).
