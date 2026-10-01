# Ingestion tuning plan: one knob at a time

Goal: make machine-ingestion time scale linearly with fleet size, by measuring
the effect of each throughput knob in isolation. Companion to
[machine-a-tron-scale-testing.md](machine-a-tron-scale-testing.md).

> Developer working notes, deliberately not registered in `docs/index.yml` -
> same treatment as the scale-testing companion above. Tracked under epic
> dsx-ai-factory/infra-controller#3738; per-knob results are recorded on the
> subtickets #3758-#3763.
>
> The Default column lists the values at the time of the plan. The explorer
> defaults have since been raised to 360 explorations, 100 machines, and 100
> concurrent explorations per iteration. Refer to
> [Large Site Sizing and Settings](large-site-sizing-and-settings.md).

## Knob inventory

| # | Knob (TOML path) | Default | Scale value | Consumed in |
|---|------------------|---------|-------------|-------------|
| K1 | `site_explorer.run_interval` | 120s | 30s | `crates/site-explorer/src/lib.rs` - period of the explore→identify→create cycle |
| K2 | `site_explorer.concurrent_explorations` | 30 | 400 | `lib.rs:2294` - semaphore width for parallel Redfish probes per cycle |
| K3 | `site_explorer.explorations_per_run` | 90 | 360 | `lib.rs:2218` - cap on routine endpoints selected per cycle |
| K4 | `site_explorer.machines_created_per_run` | 4 | 100 | `machine_creator.rs:106` - hard cap on managed hosts created per cycle |
| K5 | `firmware_global.concurrency_limit` | 16 | *(not overridden)* | preingestion-manager - concurrent endpoint transactions (NOT a batch cap; all eligible endpoints are processed each 30s run, ≤N at a time). Also caps firmware flashing concurrency. |
| K6 | `firmware_global.run_interval` | 30s | *(not overridden)* | preingestion loop period |
| K7 | `machine_state_controller.controller.max_concurrency` (nico-api chart value `machineStateController.maxConcurrency`) | 10 (chart default) | 100 (`nico-core-simulation.yaml`) | `state-controller/src/controller/processor.rs`: parallel object state-machine tasks (hostinit/dpuinit advancement). Measured on a 250-rack site with 4,500 hosts and 13,500 machines. At 10 each host gets one step per 20 min or so and takes about 17 h from creation to ready. At 40 a machine takes 4.1 h and 250 racks are ready in 7.4 h. At 80 the figures are 1.8 h and 4.5 to 5.0 h. At 120 they are 1.3 to 1.4 h and 3.9 to 4.9 h. At 160 the run is slower (11.7 h) because the handlers contend for the admin segment advisory lock. 80 to 120 is the recommended range. |

Throughput model (creation phase): `hosts_per_hour ≈ K4 × (3600 / K1_secs)`,
provided the cycle actually completes within `K1` (K3 too high breaks this -
refer to the K3 row below and dsx-ai-factory/infra-controller#3758). Defaults give 120 hosts/h. The scale values give
12,000 hosts/h, so at 4,500 hosts creation is no longer the ceiling. The scale
values are the `[site_explorer]` and `[machine_state_controller.controller]`
blocks of `helm-prereqs/values/nico-core-simulation.yaml`.

## Method

- **Fleet**: 1000 hosts × 2 DPUs (3000 endpoints) for iteration speed; confirm
  the winning combination at 4500×2 = 13,500.
- **One knob per run.** Reset the site between runs so every run starts from
  an identical state: `helm uninstall`, then the inventory reset in
  [Teardown and Reset](machine-a-tron-deployment.md#teardown-and-reset) of the
  deployment guide, or a site reprovision.
- **Instrumentation first (run 0)**: `helm-prereqs/ingestion-rate-report.sh --csv`
  derives the per-minute machine and interface creation curves from the
  database's own timestamps, so each run yields creation rate curves, not
  just total wall clock.
- **Record per run**: knob values, end-to-end wall clock, per-phase windows
  (DHCP / exploration / preingestion / creation / init), postgres CPU, any
  AvoidLockout or error storms, whether explore cycles complete within
  `run_interval`.

## Run matrix

Baseline B0 = the scale settings at the start of the campaign (K1=120s,
K2=100, K3=120, K4=40, K5=16, K6=30s, K7=10). Change a knob for a run in the
Core values file and apply it with a Core `helm upgrade`.

| Run | Change vs B0 | Hypothesis / watch for |
|-----|--------------|------------------------|
| E1 | K1 120s → 30s | ~4× cycle rate → creation and sweep cadence up. Watch: does a cycle finish in <30 s, or do iterations back up? DB load. |
| E2 | K4 40 → 100 | Creation per cycle up 2.5×. Watch: cycle-time inflation (creation runs inside the cycle - the known "big gating factor" of raising it too high). |
| E3 | K3 120 → 240 → 360 | Faster sweep of unexplored endpoints. Known cliff: 400 stopped cycles completing. Find the knee. |
| E4 | K2 100 → 200 → 400 | More parallel probes. Watch: bmc-mock/proxy saturation, exploration error rate. |
| E5 | K5 16 → 32 → 64 | Preingestion width; matters most in the preingestion-heavy window at 13.5k endpoints. |
| E6 | K7 10 → 50 → 100 | Faster hostinit/dpuinit advancement. Measured from 10 to 160 on a 250-rack site, refer to the K7 row: 80 to 120 is the useful range, and 160 was slower. The shipped scale value is 100. Watch: DB contention, admin segment lock waits. |
| E7 | Combine winners | Verify effects compose; then rerun at 4500 hosts. |

Order rationale: E1/E2 first because K1×K4 sets the hard creation ceiling;
E3/E4 shape the exploration tail; E5/E7 target the phases that dominate only
at 13.5k scale.

## Open items

- `COMMAND_BUFFER_SIZE = 100` (`work_lock_manager.rs`) does not bound K7: the
  state controller takes one work lock per controller iteration, not per
  handler.
- `site_explorer.run_interval`'s doc comment says "5 Minutes" but the default
  is 120 s - fix the comment while we're in there.
