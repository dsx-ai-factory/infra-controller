# Ingestion tuning plan: one knob at a time

Goal: make machine-ingestion time scale linearly with fleet size, by measuring
the effect of each throughput knob in isolation. Companion to
[machine-a-tron-scale-testing.md](machine-a-tron-scale-testing.md).

> Developer working notes, deliberately not registered in `docs/index.yml` —
> same treatment as the scale-testing companion above. Tracked under epic
> dsx-ai-factory/infra-controller#3738; per-knob results are recorded on the
> subtickets #3758-#3763.

## Knob inventory

| # | Knob (TOML path) | Default | Scale-script value | Consumed in |
|---|------------------|---------|--------------------|-------------|
| K1 | `site_explorer.run_interval` | 120s | *(not overridden)* | `crates/site-explorer/src/lib.rs` — period of the explore→identify→create cycle |
| K2 | `site_explorer.concurrent_explorations` | 30 | 100 | `lib.rs:2294` — semaphore width for parallel Redfish probes per cycle |
| K3 | `site_explorer.explorations_per_run` | 90 | 120 | `lib.rs:2218` — cap on routine endpoints selected per cycle |
| K4 | `site_explorer.machines_created_per_run` | 4 | 40 | `machine_creator.rs:106` — hard cap on managed hosts created per cycle |
| K5 | `firmware_global.concurrency_limit` | 16 | *(not overridden)* | preingestion-manager — concurrent endpoint transactions (NOT a batch cap; all eligible endpoints are processed each 30s run, ≤N at a time). Also caps firmware flashing concurrency. |
| K6 | `firmware_global.run_interval` | 30s | *(not overridden)* | preingestion loop period |
| K7 | `state_controller.max_concurrency` | 10 | *(not overridden)* | `state-controller/src/controller/processor.rs:194` — parallel object state-machine tasks (hostinit/dpuinit advancement). Effective ceiling ≈100: `COMMAND_BUFFER_SIZE = 100` in `api-db/src/work_lock_manager.rs:33` is a hard-coded constant every work unit round-trips through. |

Throughput model (creation phase): `hosts_per_hour ≈ K4 × (3600 / K1_secs)`,
provided the cycle actually completes within `K1` (K3 too high breaks this —
see the K3 row below and dsx-ai-factory/infra-controller#3758). Defaults give 120 hosts/h; current scale settings give 1,200
hosts/h → ~3.75 h floor for 4,500 hosts, consistent with observed runs.

## Method

- **Fleet**: 1000 hosts × 2 DPUs (3000 endpoints) for iteration speed; confirm
  the winning combination at 4500×2 = 13,500.
- **One knob per run.** Full `cleanup-machine-a-tron.sh -y` between runs so
  every run starts from an identical state.
- **Instrumentation first (run 0)**: sample the four pipeline counters
  (explored / preingestion-complete / hosts / machines) plus site-explorer
  cycle duration every 30–60 s into a CSV from the verification loop, so each
  run yields per-phase rate curves, not just total wall clock.
- **Record per run**: knob values, end-to-end wall clock, per-phase windows
  (DHCP / exploration / preingestion / creation / init), postgres CPU, any
  AvoidLockout or error storms, whether explore cycles complete within
  `run_interval`.

## Run matrix

Baseline B0 = current scale settings (K1=120s, K2=100, K3=120, K4=40,
K5=16, K6=30s, K7=10).

| Run | Change vs B0 | Hypothesis / watch for |
|-----|--------------|------------------------|
| E1 | K1 120s → 30s | ~4× cycle rate → creation and sweep cadence up. Watch: does a cycle finish in <30 s, or do iterations back up? DB load. |
| E2 | K4 40 → 100 | Creation per cycle up 2.5×. Watch: cycle-time inflation (creation runs inside the cycle — the known "big gating factor" of raising it too high). |
| E3 | K3 120 → 240 → 360 | Faster sweep of unexplored endpoints. Known cliff: 400 stopped cycles completing. Find the knee. |
| E4 | K2 100 → 200 → 400 | More parallel probes. Watch: bmc-mock/proxy saturation, exploration error rate. |
| E5 | K5 16 → 32 → 64 | Preingestion width; matters most in the preingestion-heavy window at 13.5k endpoints. |
| E6 | K7 10 → 50 → 100 | Faster hostinit/dpuinit advancement. Do NOT exceed 100 (work-lock channel). Watch: DB contention, work-lock channel pressure. |
| E7 | Combine winners | Verify effects compose; then rerun at 4500 hosts. |

Order rationale: E1/E2 first because K1×K4 sets the hard creation ceiling;
E3/E4 shape the exploration tail; E5/E7 target the phases that dominate only
at 13.5k scale.

## Open items

- Decide whether `COMMAND_BUFFER_SIZE = 100` (`work_lock_manager.rs`) should
  become configurable before pushing K7 toward 100.
- `site_explorer.run_interval`'s doc comment says "5 Minutes" but the default
  is 120 s — fix the comment while we're in there.
- Add env-var overrides in `setup-machine-a-tron.sh` for K1/K5/K6/K7 (K2–K4
  already have them) so runs are scriptable.
