# Large Site Sizing and Settings <Badge intent="info">v2.3</Badge> <Badge intent="launch" minimal>New</Badge>

This page records what a 250-rack ingestion measured on a 3-node site controller
and which settings it needed. Use it to size a site controller and to set the
ingestion knobs before bringing up a large site. The measurements answer
[issue 5206](https://github.com/dsx-ai-factory/infra-controller/issues/5206),
which asked for site controller sizing guidance for large sites, and
[issue 5974](https://github.com/dsx-ai-factory/infra-controller/issues/5974),
which asked for cluster sizing for large simulated fleets. This page is a
companion to
[Scaling NICo with Machine-a-Tron](machine-a-tron-scale-testing.md), which
describes the simulator setup.

## What Was Measured

The fleet was simulated with Machine-a-Tron: 250 GB200 NVL72 racks, each a
72-GPU NVLink domain. It had 4,500 compute trays with 2 data processing units
(DPUs) each, 2,250 NVLink switches, and 2,000 power shelves. That is 17,750
baseboard management controller (BMC) endpoints, of which the 13,500 trays and
DPUs become machines. Ten Machine-a-Tron instances of 25 racks each ran in
controller mode behind the protocol gateway on the same three site controller
nodes as NICo. The runs covered ingestion only: discovery, preingestion, machine
creation, and the machine state controller up to `ready`. Provisioning, firmware
updates, and tenant workflows were not exercised. Storage input/output
operations per second (IOPS) were not measured, and no node failed during a run.
Hours are counted from fleet-up, the point at which every simulated BMC endpoint
is deployed and reachable. The fleet-up burst is the elevated kube-apiserver
load in the first 2 to 3 hours after that point.

## Site Controller Sizing

The three nodes had 96 cores and 251 GiB of allocatable memory each. They
carried NICo, its Postgres, and the simulator. The figures below are from the
runs at controller concurrency 80 to 140, the measured range around the
recommended 80 to 120.

| Resource | Highest per-node peak | Highest per-node 95th percentile (p95) over the run |
|---|---|---|
| CPU | 38 to 54 cores, of which kube-apiserver was up to 39 at the peak sample during the fleet-up burst | 31 to 50 cores |
| Memory | 46 to 60 GiB | 44 to 47 GiB |

At steady state NICo itself used 19 cores and 31 GiB. Postgres memory below is
the kubelet working set: resident memory plus active page cache. Working set
counts the file cache, so over a long run it grows toward the memory limit. The
first complete run took 54 hours because of faults fixed before the later runs,
and it ran Postgres at the helm-prereqs defaults of 8 cores and 16 GiB. The
primary sat at its 8-core limit, its working set reached 15 GiB of the 16 GiB
limit, and the run completed. The 20-hour run at the default concurrency and the
runs in the table above ran Postgres at 16 cores and 32 GiB. At that limit the
primary used about 10 of 16 cores. Its working set peaked at about 14 GiB in the
5-hour runs. In the 20-hour run it sat at 14 to 17 GiB and spiked to 26 GiB for
two samples. nico-api used 3 to 6 cores and 6 GiB, and hardware-health used 6
cores and 4.6 GiB. The ten simulator pods used 2 cores and 15 GiB together. Most
of the remaining CPU was platform load from the simulator's 17,750 BMC Services,
a load a real site does not have. At steady state that was kube-proxy IP Virtual
Server (IPVS) at 2 to 3 cores per node. During the fleet-up burst it was
kube-apiserver, at up to 39 cores on the busiest node at its peak sample,
measured at controller concurrency 140.

Memory fits the 256 GiB minimum node with room to spare. The highest reading was
62 GiB on one node, during the two-sample Postgres spike late in the 20-hour run
at the default concurrency. That run is outside the table's range. These runs do
not show the CPU headroom of a node with two 24-core CPUs (48 cores). At
controller concurrency 80 to 140 the highest per-node 95th percentile was 31 to
50 cores and the highest per-node peak was 38 to 54 cores. At the busiest
samples kube-apiserver was anywhere from a small share to three quarters of
that, and the rest was Postgres, nico-api, hardware-health, and kube-proxy. For
a site of this size, plan nodes with more cores than the 48-core minimum, or 5
nodes. The measurements cover three 96-core nodes only, so validate the sizing
for each deployment. The 512 GiB memory recommendation is growth headroom. Give
Postgres a 16-core limit: at its 8-core default it was throttled in 88 percent
of Completely Fair Scheduler (CFS) periods. Its memory needed no raise: the
54-hour run completed at the 16 GiB default, and the 32 GiB of the later runs
was headroom. The sizing table in `helm-prereqs/values.yaml` stays the memory
guidance. Give nico-api 8 cores: it used 5 to 6 of them at controller
concurrency 80 and above.

## Time to Ready Is a Concurrency Setting

Time to ready does not scale with cores. With the default `max_concurrency` and
the explorer settings in
[Settings Changed From the Defaults](#settings-changed-from-the-defaults), all
13,500 machines were created within 2.3 hours of fleet-up. The median machine
then took 17.2 hours from creation to `ready`, and the slowest took 18.1 hours.
nico-api used 6 of 8 cores without throttling. The machine state controller runs
at most `max_concurrency` machine handlers at a time (default 10) and dispatches
more as handlers finish. With 4,500 hosts queued, each host got one pipeline
step per 20 minutes or so and needed about 47 steps. The per-machine pipeline
time scales with hosts divided by `max_concurrency`. End-to-end time stops
improving above 120 and more than doubles at 160.

| `max_concurrency` | Creation to ready per machine, median (p50) | 250 racks ready after fleet-up |
|---|---|---|
| 10 (default) | 17.2 h | 20.0 h |
| 40 | 4.1 h | 7.4 h |
| 80 | 1.8 h to 2.1 h | 4.5 h to 5.0 h |
| 100 | 1.5 h | 4.9 h |
| 120 | 1.3 h to 1.4 h | 3.9 h to 4.9 h |
| 140 | 1.2 h | 5.1 h |
| 160 | 1.1 h | 11.7 h |

The controller side is monotonic in the setting. Above 80 the handlers contend
more for the admin network segment advisory lock that machine creation also
takes. The mean wait was 53 ms at 80, 84 ms at 100, and about 100 ms at 120 and
140. Between 80 and 140 the end-to-end time stays within run-to-run variance,
and the 3.9-hour best case at 120 did not reproduce on a repeat (4.9 hours). At
160 machine creation starves and the run takes 11.7 hours. Use 80 to 120 with
nico-api at 8 cores. To set it, use the nico-api chart value
`machineStateController.maxConcurrency`, or put the TOML table
`[machine_state_controller.controller]` with `max_concurrency` in the site
config overlay `siteConfig.nicoApiSiteConfig`, which nico-api merges over the
base configuration.

## Settings Changed From the Defaults

| Setting | Used | Default and why it was changed |
|---|---|---|
| `[site_explorer]` `explorations_per_run`, `machines_created_per_run`, and `concurrent_explorations` | 2000, 1000, and 300 | 360, 100, and 100: the defaults cap how many endpoints each explorer iteration probes and how many machines it creates, so 13,500 machines would need many more iterations |
| `[site_explorer]` `switches_created_per_run` and `power_shelves_created_per_run` | 1000 and 1000 | 9 switches and 1 power shelf per iteration. Even at 100 each, 2,250 switches and 2,000 shelves needed more than 20 iterations |
| `max_database_connections` | 900 | 1000. The DPU agents poll their network configuration on a shared interval, and at 9,000 DPUs one poll burst can take most of the pool while every other database user in nico-api waits. 900 keeps idle headroom under the Postgres `max_connections` of 1024 that helm-prereqs sets, which nico-api shares with the other services |
| nico-hardware-health `[rate_limit]` (`CARBIDE_HEALTH__RATE_LIMIT__BUCKET_BURST` and `CARBIDE_HEALTH__RATE_LIMIT__BUCKET_REPLENISH` in the chart's `env`) | `bucket_burst` 100 and `bucket_replenish` 30ms, the limiter's own defaults | Off. Without the limiter every simulated BMC re-authenticates through nico-api every 120 s, about 100 credential mints per second and about 20 percent of the agent request time |
| nico-api CPU limit | 8 cores | 3 cores: nico-api used 5 to 6 cores at controller concurrency 80 and above. The 32 GiB memory limit is the chart default and was not changed |
| Postgres CPU and memory limits | 16 cores and 32 GiB | 8 cores and 16 GiB: throttled in 88 percent of CFS periods at 8 cores. The memory raise was headroom only, refer to the sizing section |
| `[machine_state_controller.controller] max_concurrency` (chart value `machineStateController.maxConcurrency`) | 80 to 120 recommended. The runs covered 10 to 160 | 10, refer to the table above |
| `[api_admission_control]` `enabled` (chart value `apiAdmissionControl.enabled`) | `false` for the simulated fleet | `true`, with per-client limits of 8 requests in flight and 64 pending. All Machine-a-Tron agents share one client key and retry rejected calls at once, which rejected 819,130 calls in 11 minutes and restarted nico-api three times. Keep admission control on for real hardware |
| Vault server probes (`helm-prereqs/operators/values/vault.yaml`) | `timeoutSeconds` 10, `failureThreshold` 5 | 3 seconds and 2 failures: the active node missed two probes under load, was killed, and came back sealed |

The TOML keys are nico-api configuration: the chart's base file merged with the
site config overlay `siteConfig.nicoApiSiteConfig`. Set them in the overlay.
The two settings that name a chart value can be set through the nico-api chart
instead, and an overlay entry for the same key takes precedence over the chart
value. The hardware-health `[rate_limit]` row is that chart's configuration,
set through its `env` map as `nico-hardware-health.env` in the same values
file.

To reproduce the fleet, run Machine-a-Tron as ten instances of 25 racks each in
controller mode behind the protocol gateway. Keep them on one BMC segment with
the BMC addresses published as Service externalIPs. The fleet needs one BMC
address per endpoint, 17,750 in total, so the segment must be at least a `/17`.
Refer to
[Replicating the 250-Rack Fleet](machine-a-tron-scale-testing.md#replicating-the-250-rack-fleet)
for the ordered steps. The simulation overlay's `simulated-oob` segment is a
`/18`, which holds 16,384 addresses, so the runs used one `10.200.0.0/17`
segment instead.

## Reading the Numbers

- All figures come from simulated BMCs with a single nico-api replica.
- The admin segment advisory lock decides the creation side. Its mean wait grew
  from 53 ms at controller concurrency 80 to about 100 ms at 120 and 140.
- The per-machine pipeline (creation to `ready`) is the part that scales with
  the setting. The creation side (site explorer iterations) is what the
  remaining time is made of.
