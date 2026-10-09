# Per-Object State Progress Metrics

- **Issue:** [#2186](https://github.com/dsx-ai-factory/infra-controller/issues/2186) · **Related:** [#191](https://github.com/dsx-ai-factory/infra-controller/issues/191), [#2168](https://github.com/dsx-ai-factory/infra-controller/pull/2168)
- **Status:** Implemented (deviations from the original proposal are noted inline)

## Problem

State-controller metrics are aggregate-only (`carbide_machines_per_state`,
`_per_state_above_sla`, transition counters, time-in-state histograms). At
100k-machine scale, SREs cannot answer via Prometheus alone:

1. **Which** machines are stuck beyond SLA (not just how many).
1. Which need **manual operator action**, and why.
1. What state a machine is in, **as a join key** (for example, suppress
   "DPU not calling home" during DPU reprovision).
1. Per-machine **custom SLAs** (hardware gen, tenant) without per-fleet rules.
1. Slicing by **traits** (rack, SKU) and **associations** (host↔DPU,
   machine↔instance).
Per-object series cost O(fleet) cardinality, so this design spends it on a
small fixed set of gauges, served from a **dedicated, opt-in endpoint**.

## What we build on

- `StateControllerIO::metric_state_names()` already maps every controller
  state to a stable `(state, substate)` pair — this *is* the "normalized
  state"; no new layer needed.
- The processor already computes per-object `time_in_state` and `StateSla`
  each iteration (`state-controller/src/metrics.rs`, `processor.rs`) — today
  it aggregates them away.
- `state_sla(state, object_state)` receives the full object state, so
  per-hardware-gen/tenant SLAs are a policy change in that function only.
- PR [#2168](https://github.com/dsx-ai-factory/infra-controller/pull/2168)'s `PerObjectMetricsRegistry` (`health-metrics/src/per_object.rs`)
  was built to generalize across object types and is **reused directly**:
  shared registry keyed by `(object_type, object_id)`, observable gauges,
  lazy stale eviction, config-gated.
- Manual-intervention signals exist per-object:
  `StateHandlerError::ManualInterventionRequired` and terminal
  `Failed { FailureDetails }` states.

## Design

### Dedicated endpoint

Per-object series live on a second Prometheus registry/meter, served on its
own listener:

```toml
[observability.per_object_state_metrics]
enabled = true                    # default: false
listen_address = "[::]:9091"      # dual-stack default
object_types = ["machine", "switch", "power_shelf", "rack"]  # default: all
```

`object_types` deserializes into an enum, so a mistyped token fails config
parsing instead of silently emitting nothing. Every state controller feeds
the endpoint, so `network_segment`, `vpc_prefix`, `spdm_attestation`, and
`ib_partition` are also valid.

This lets operators scrape it slower (60–120s — the series change only on
state transitions), route it to a different tenant/retention, or skip it —
without touching the existing `/metrics`. `api-core` already owns its metrics
handler; adding a second `(registry, meter, listener)` in
`run.rs::create_metrics()` is mechanical.

### Metric catalog

All **gauges**, existing only while the fact they state is true (registry
replaces/evicts entries, so stale series disappear at next scrape). Per-object
counters/histograms are rejected outright: they accumulate label sets forever.
Labels come only from closed sets (`&'static str` state names, enum-derived
reasons) plus the object id — never free text. Common labels: `object_type`,
`object_id`, `state`, `substate`. State label values reuse the existing
`metric_state_names` vocabulary as-is (`"ready"`, `"dpunotready"`, ...), so
per-object and aggregate metrics stay joinable; renaming that vocabulary is a
separate, breaking discussion.

#### `carbide_object_state_entered_timestamp_seconds`

One series per live object: Unix time it entered its current state.

```text
carbide_object_state_entered_timestamp_seconds{object_type="machine",object_id="fm100...",state="host_init",substate="waiting_for_discovery"} 1.7519832e+09
```

Covers three asks at once: lifecycle timestamp, state age
(`time() - ...` — a timestamp beats an age gauge: it changes only on
transition, compresses well, and is exact at query time), and **current state
as join key** via its labels. On transition the entry is replaced by the
committing iteration (including the new state's re-resolved SLA), so exactly
one series per object exists per process (see the multi-replica caveat in
the Queries section). Entry time = the controller state's version timestamp
(exact and immune to handler latency and clock-skew fallbacks).

#### `carbide_object_state_sla_seconds`

The object's resolved SLA for its current state, from `state_sla()`. Emitted
only for states that have an SLA (`no_sla()` states like `ready`/`assigned`
emit nothing — they can never fire, and steady-state fleets emit far fewer
series than fleet size).

```text
carbide_object_state_sla_seconds{object_type="machine",object_id="fm100...",state="host_init",substate="waiting_for_discovery"} 1800
```

Per-object SLAs mean alert rules never change when SLA policy does. Severity
is rule-side (`> sla` warning, `> 2*sla` critical) — one series, not two.
Value `0` keeps the existing `StateSla` "should never be here" convention.

#### `carbide_object_manual_intervention_required` (value 1, only while true)

```text
carbide_object_manual_intervention_required{object_type="machine",object_id="fm100...",state="failed",substate="",reason="bios_setup_failed"} 1
```

Emitted when the latest iteration hit `StateHandlerError::ManualInterventionRequired`
(reason = its existing `metric_label()`) or a terminal failure state: machine
`Failed` with a non-`NoError` cause (reason = stable snake_case token from
`FailureCause` — never free-text error strings), and switch/rack/power-shelf
`Error` states (reason = the fixed token `"error"`, because their stored
causes are free text). A transient non-intervention error (timeout, DB error)
leaves an existing series untouched rather than clearing it, so a stuck
object doesn't flap out of alerts on one bad iteration. Further triggers
(e.g. substate `Failed` variants with exhausted retries) are added only after
operational experience. `TimeInStateAboveSla`
is deliberately **excluded**: SLA breach is a symptom, fully expressible from
the two metrics above; mixing it in makes this set noisy during mass-slowness
incidents, exactly when it must be precise.

#### `carbide_object_info` (value 1) — stable traits

```text
carbide_object_info{object_type="machine",object_id="fm100...",rack_id="rack-12",sku="GB200-NVL72-A",vendor="Supermicro",model="SYS-421GE"} 1
```

Standard `_info` join pattern (cf. `kube_node_info`). Traits live *only* here —
on state series they'd multiply cardinality and churn on correction. Caveat:
vendor/model aren't first-class `machines` columns (they come from exploration
reports), so those labels are best-effort `""` when unknown.

#### Association info metrics (value 1, one series per pair)

```text
carbide_machine_dpu_info{machine_id="fm100...",dpu_id="fmdpu01..."} 1
carbide_machine_instance_info{machine_id="fm100...",instance_id="inst-42...",tenant_org="acme"} 1
```

One series per *relationship* resolves a concern about multiple DPU hosts:
a 4-DPU host is 4 association series, while its state series stays one. There
is also no `blocking_dpu_id` label on it; the host reports the
least-progressed DPU's substate (matching the `metric_state_names`
behavior). DPUs are **not** state-controller objects (the machine controller
excludes them from its object list), so they get no state series of their
own; the association join is for mapping DPU-level telemetry (which carries
`dpu_id`, e.g. `carbide_forge_dpu_agent_last_call`) onto the host and its
state series.

Instances get no state series of their own: they are machine substates
(`Assigned { instance_state }`), not separate state-controller objects, so
they surface as `state="assigned", substate=<instance state>` on the machine
plus `carbide_machine_instance_info` for the ID mapping — a synthetic
per-instance series would double cardinality for no new information.

### Cardinality budget (100k machines + ~5k other objects)

| Metric | Series |
|---|---|
| `state_entered_timestamp` | ~105k (1/object) |
| `state_sla` | ≪ fleet (SLA states only) |
| `manual_intervention_required` | fleet-in-trouble (~5k at the issue's 5%) |
| `object_info` | ~105k |
| associations | ~100k + #instances |
| **Total** | **~400–500k slow-moving series**, opt-in, on their own endpoint |

Churn is bounded by transition rate, observable in advance via the existing
`_state_entered_total` counters.

## Queries (issue use cases)

Two caveats apply to all joins below:

- **Multiple API replicas.** Series live in the memory of whichever replica
  last processed the object, so with `replicas > 1` a scraped fleet can
  briefly expose the same object from more than one pod (the stale copy ages
  out within the hold period). Aggregate away the scrape instance first —
  e.g. `max by (object_type, object_id, state, substate) (...)` — before
  joining, or run a single replica for this endpoint.
- **Transitions vs. scrapes.** The three state families are updated as
  separate series; a scrape landing mid-transition can see them disagree on
  `state` for one interval. Alerts on these joins should carry a `for:` of at
  least one scrape interval.

**Stuck beyond per-object SLA** (warning; critical = `2 *`):

```text
max by (object_type, object_id, state, substate)
    (time() - carbide_object_state_entered_timestamp_seconds)
  > on(object_type, object_id, state, substate) group_left()
    max by (object_type, object_id, state, substate)
        (carbide_object_state_sla_seconds)
```

**Manual-intervention ratio and triage breakdown:**

```text
count(carbide_object_manual_intervention_required{object_type="machine"})
  / scalar(carbide_machines_total) > 0.05

count by (reason, rack_id) (
  carbide_object_manual_intervention_required{object_type="machine"}
  * on(object_id) group_left(rack_id, sku) carbide_object_info)
```

**Suppress DPU-not-calling-home during reprovision** (join use case;
`DPUReprovision` maps to `state="reprovisioning"` today):

```text
(time() - carbide_forge_dpu_agent_last_call > 900)
and on(dpu_id) label_replace(carbide_machine_dpu_info, "dpu_id", "$1", "dpu_id", "(.*)")
unless on(machine_id) label_replace(
  carbide_object_state_entered_timestamp_seconds{state="reprovisioning"},
  "machine_id", "$1", "object_id", "(.*)")
```

**Stuck-and-unhealthy — is it hardware?** (join with the [#2168](https://github.com/dsx-ai-factory/infra-controller/pull/2168)metric):

```text
carbide_object_manual_intervention_required
and on(object_type, object_id)
  carbide_object_unhealthy_by_classification_count{classification="Hardware"}
```

## Non-goals

- **Per-object transition counters** (`object × from × to × result`
  cardinality; counter series never expire). Aggregate transition
  counters/histograms + the `*_state_history` DB tables already cover this.
  Prometheus needs "where is it now, since when" — provided above.
- **`expected_next_state` per object.** State machines are imperative handler
  code; the next state depends on runtime conditions. A *static* per-`(state,
  substate)` "typical next state" info metric could be added later.
- **Dependency-lag metrics** (network config sync, IB sync) — need their own
  instrumentation design. Reserved shape:
  `carbide_object_dependency_lag_seconds{object_id, dependency="network_config_sync"|...}`.
- **Hardware-cause detail** — covered by joining the existing per-object
  health classification metric (query above); probe-level detail stays on
  aggregate `_unhealthy_by_probe_id_count`.

## Implementation

**Reuse `PerObjectMetricsRegistry`** (`health-metrics/src/per_object.rs`).
It was built for exactly this generalization — a single shared registry keyed
by `(object_type, object_id)` "so the metric name stays stable as
observability generalizes across object types" (its module doc) — and already
provides everything the new metrics need: shared entry map, gauge callbacks
over live entries, `hold_period` eviction, replace/remove-on-record
semantics.

We extend the `PerObjectMetricsRegistry`, rather than adding a sibling:

- Generalize the entry payload from classification-only to per-metric series:
  a `gauge(name, help, label_names)` handle API where writers `set`/`set_all`/
  `clear`/`touch` an object's series for that metric (the existing
  classification gauge becomes the first handle, keeping its config gating
  and behavior).
- **Deviation from the proposal:** each handle registers a native Prometheus
  collector on a caller-supplied `prometheus::Registry`, not an OpenTelemetry
  `Meter`. OpenTelemetry instruments enforce a per-stream cardinality limit
  (2000 series by default), which a per-object fleet vastly exceeds; native
  pull collection has no such limit and skips the SDK's per-series
  aggregation on every scrape. The existing
  `carbide_object_unhealthy_by_classification_count` stays an OTel gauge on
  the main endpoint (moving it is a scrape-config-visible change; do it as a
  later, separately announced step).

**Feed point: the generic processor** (`processor.rs`), which already holds
object ID, (transitioned) state, the state version's timestamp, `StateSla`,
and handler error per iteration — one record call there gives **every** state
controller per-object metrics with zero per-controller code (the only
per-controller code is the optional `manual_intervention_reason` state hook).
A committed transition is published by the committing iteration, including
the new state's re-resolved SLA. Objects the handler deletes have their
series removed by that same iteration; objects deleted out-of-band are
cleared by the enqueuer's next sweep, which diffs consecutive live sets
(hold-period eviction remains the backstop). Wiring mirrors [#2168](https://github.com/dsx-ai-factory/infra-controller/pull/2168): constructed in `setup.rs`, registered on the per-object
Prometheus registry, threaded via the controller builder.
`carbide_object_info` and associations are recorded from the
machine-controller handler, which already loads rack/SKU/DPU/instance data
next to the existing per-object health call.
