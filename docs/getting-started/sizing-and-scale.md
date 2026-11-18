# Sizing and Scale Guidance <Badge intent="info">v2.4</Badge> <Badge intent="launch" minimal>New</Badge>

This page is the canonical statement of how large a fleet one NVIDIA Infra
Controller (NICo) site controller has managed, how each figure was validated,
and how to size the site controller for a given fleet. It was requested in
[issue 4234](https://github.com/dsx-ai-factory/infra-controller/issues/4234).
The detailed record of the 250-rack measurement and the ingestion settings it
needed stay on
[Large Site Sizing and Settings](../development/large-site-sizing-and-settings.md).
Rows marked to be determined (`TBD`) have no measurement. The scaling
area owns them, and a figure is published through the rule in
[Release Notes](#release-notes) when one exists.

## Guidance, Not a Hard Limit

NICo does not enforce a configured or hard-coded maximum on the number of
hosts, data processing units (DPUs), NVLink switch trays, power shelves, or
racks that one site controller manages. The figures on this page are
guidance: they are the largest fleets that have been measured, with the site
controller resources and settings that carried them. A fleet larger than any
measured figure is not rejected by NICo, but it is outside the measured range,
so plan a measurement before relying on it.

## How to Read the Table

Every figure carries a status and a validation method. In the host rows, a
host is a compute tray: a GB200 NVL72 rack has 18.

| Status | Meaning |
|---|---|
| Field deployment | Measured on sites running NICo. Figures come from the site controller's own metrics over a 7-day window, collected on 2026-10-13 from 33 sites of 1 to 1,930 hosts each. The dataset includes a one-host demo site and a 31-host lab site. 25 of the sites run the v0.10 line, 6 the v2.0 line, and 2 the v2.1 line. The dataset did not collect rack, switch tray, or power shelf counts, so those field rows are `TBD`. |
| Hardware lab | Measured against real hardware in a lab. No published figure has this status. |
| Simulation | Measured on a 3-node site controller against a simulated fleet: machine-a-tron baseboard management controllers (BMCs) and agents behind the protocol gateway, with the DOCA Platform Framework (DPF) simulator. Refer to [Machine-a-tron Scale Testing](../development/machine-a-tron-scale-testing.md). A simulation exercises the control plane: discovery, machine creation, the state controllers, rack setup through the Rack Management Service (RMS) mock, and NVLink partition creation through the NMX Controller (NMX-C) mock. It does not exercise device timing, firmware flashing, the DPU data plane, or tenant workloads. |
| Simulation, pre-release build | A simulation on a build that is not a release. The figure cannot be reproduced from a release. Refer to the note that follows the legend. |
| Planned certification | A scale that a repeatable release benchmark is planned to gate on. The certification plan has not been published, so no row carries this status. The scaling test history is in the closed [issue 3723](https://github.com/dsx-ai-factory/infra-controller/issues/3723), [issue 3290](https://github.com/dsx-ai-factory/infra-controller/issues/3290), and [issue 4009](https://github.com/dsx-ai-factory/infra-controller/issues/4009), and the open work is owned by the scaling area. |
| Roadmap | A target with no measurement. |

Times count from fleet-up, the point at which every simulator pod is Ready,
the protocol gateway answers, and the controller has published every pod's
Services, so every simulated BMC address is reachable. The simulation rows and
the sections that follow use that clock. Refer to
[Machine-a-tron Scale Testing](../development/machine-a-tron-scale-testing.md).

The NICo version column names the release line the figure was measured on. The field rows name the line the measured site ran, and the
field fits in [Site Controller Sizing by Fleet
Size](#site-controller-sizing-by-fleet-size) mix the three lines of the
dataset, with 25 of the 33 sites on the v0.10 line. The simulated fleets of
400 and 556 racks ran on pre-release builds. The 400-rack run and the earlier
556-rack run ran a build of the v2.4 line: `main` before the v2.4 release
branch was cut, with scaling fixes applied. The later 556-rack run ran a build
of the v2.5 line, taken from `main` after the v2.4 release branch was cut,
with the scaling fixes named on this page and pull request 8142 applied. The
556-rack fleet ran to completion twice, on the earlier and the later build,
and its figures below are from the later run unless the earlier run is named.
The scaling fixes are [pull request
7847](https://github.com/dsx-ai-factory/infra-controller/pull/7847), [pull
request 7848](https://github.com/dsx-ai-factory/infra-controller/pull/7848),
[pull request
7849](https://github.com/dsx-ai-factory/infra-controller/pull/7849), [pull
request 7850](https://github.com/dsx-ai-factory/infra-controller/pull/7850),
[pull request
8041](https://github.com/dsx-ai-factory/infra-controller/pull/8041), [pull
request 8081](https://github.com/dsx-ai-factory/infra-controller/pull/8081),
[pull request
8094](https://github.com/dsx-ai-factory/infra-controller/pull/8094), and [pull
request 8100](https://github.com/dsx-ai-factory/infra-controller/pull/8100).
Rack-ordered machine creation in the site explorer, with its
`machine_creation_concurrency` and `switch_and_power_shelf_pass_interval`
keys and the `admin_lock_admission_permits` configuration key, which the later
556-rack run set to 32, is [pull request
8142](https://github.com/dsx-ai-factory/infra-controller/pull/8142). The v2.4
build also carried changes that were dropped before release. The v2.5 build is
`main` plus the listed fixes and pull request 8142, with nothing dropped. The
figures from either build cannot be reproduced from a release, and each
release's notes state which of these changes it includes.

## Single Site Controller Scale Table

| Dimension | Figure | Status | Validation method | NICo version |
|---|---|---|---|---|
| Hosts under management | 10,008 | Simulation, pre-release build | Two completed runs of 556 GB200 NVL72 racks with 2 DPUs per tray. In the later run all 30,024 machines were created 2.3 hours after fleet-up, and every host, DPU, switch tray, and power shelf reached `ready`, the last rack at 7.9 hours, with no nico-api restart. That run did not read the Postgres `oom_kill` counter, but the primary's working set never reset to baseline and the same pod stayed primary throughout. The earlier run of the same fleet, on the v2.4 build with 16 CPU limits on the Postgres primary and nico-api, had its last rack `ready` at 10.5 hours with no nico-api restart and no Postgres out-of-memory kill. | v2.5 pre-release build for the later run and v2.4 for the earlier run |
| Hosts under management | 7,200 | Simulation, pre-release build | One run of 400 GB200 NVL72 racks. All 21,600 machines were created 1.7 hours after fleet-up and the last rack was `ready` at 6.9 hours. | v2.4 pre-release build |
| Hosts under management | 4,500 | Simulation | 250 GB200 NVL72 racks, measured in several runs. All racks were `ready` 3.9 to 5.1 hours after fleet-up at machine state controller concurrency 80 to 140, and 20 hours at the default concurrency. Refer to [Large Site Sizing and Settings](../development/large-site-sizing-and-settings.md#time-to-ready-is-a-concurrency-setting). | v2.3 line |
| Hosts under management | 1,930 | Field deployment | The largest single site in the dataset. It is a CPU and storage site with one DPU per host and no GPUs, on 3 site controller nodes with 256 logical CPUs and 1 TiB of memory each. | v2.0 line |
| Machines (hosts plus DPUs) | 30,024 | Simulation, pre-release build | The 556-rack fleet: 10,008 hosts and 20,016 DPUs. NICo manages each host and each DPU as a machine. | v2.5 pre-release build |
| DPUs | 20,016 | Simulation, pre-release build | The 556-rack fleet. DPU provisioning went through the DPF simulator, so the DPU control path was exercised and the Host Based Networking (HBN) data plane was not. | v2.5 pre-release build |
| DPUs | 1,913 | Field deployment | DPUs reporting up at the largest single site. | v2.0 line |
| DPUs in one HBN fabric | 10,000 | Roadmap | Target of [issue 2940](https://github.com/dsx-ai-factory/infra-controller/issues/2940). The fabric-side limits are confirmed with the HBN and DPF owners before NICo publishes a figure. | Not released |
| GPUs | 40,032 | Simulation, pre-release build | The 556-rack fleet: 4 GPUs per compute tray in the simulated discovery inventory, 72 per rack, with one NVLink partition created per host through the NMX-C mock. GPU health and telemetry were not exercised. | v2.5 pre-release build |
| GPUs | 2,037 | Field deployment | GPUs reported by the largest GPU site in the dataset, on 388 hosts. | v2.0 line |
| NVLink switch trays | 5,004 | Simulation, pre-release build | The 556-rack fleet, 9 per rack, all `ready` 4.3 hours after fleet-up in the later run and 4.9 hours in the earlier run. | v2.5 pre-release build for the later run and v2.4 for the earlier run |
| NVLink switch trays | TBD | Field deployment | Not collected in the dataset. | |
| Power shelves | 4,448 | Simulation, pre-release build | The 556-rack fleet, 8 per rack, all `ready` 1.8 hours after fleet-up in both runs. | v2.5 pre-release build for the later run and v2.4 for the earlier run |
| Power shelves | TBD | Field deployment | Not collected in the dataset. | |
| Physical racks | 556 | Simulation, pre-release build | The 556-rack runs. | v2.5 pre-release build for the later run and v2.4 for the earlier run |
| Physical racks | 250 | Simulation | The 250-rack runs behind the 4,500-host row. | v2.3 line |
| Physical racks | TBD | Field deployment | Not collected in the dataset. | |
| NVLink partitions | 10,008 | Simulation, pre-release build | The later 556-rack run: one partition per host created through the NMX-C mock, 0 failed, 0 timed out, and the 556 default partitions removed. The partition monitor averaged 10.4 seconds per pass over the run and 14 to 19 seconds per pass with 164 to 505 racks `ready`, with a peak 10-minute mean of 24 seconds as the last racks went `ready`, against its 60-second interval. No pass overran the interval. The longest pass is not known because the pass-time histogram tops out at 10 seconds. In the earlier run the monitor averaged 17.1 seconds per pass, ran 27 to 33 seconds per pass with 554 to 555 racks `ready`, reached 10-minute window means of up to 42 seconds, and overran the interval 8 times. | v2.5 pre-release build for the later run and v2.4 for the earlier run |
| BMC endpoints explored | 39,476 | Simulation, pre-release build | The 556-rack fleet: 71 BMC endpoints per rack. | v2.5 pre-release build |
| Concurrent tenant VPCs | TBD | Field deployment | Not collected in the dataset, and no tenant virtual private cloud (VPC) or instance journey has run in a simulation at scale. The count is bounded by the Virtual Extensible LAN (VXLAN) network identifier (VNI) pool ranges the operator configures. Refer to [Related Limits](#related-limits). | |

## What Limits Scale

Rack count is a convenient unit, but NICo's work scales with the devices in
the racks: the BMC endpoints the site explorer probes, the machines the state
controllers advance, and the rows the database holds for them. The rack
profile sets the count. A GB200 NVL72 rack with 2 DPUs per compute tray has 18
host BMCs, 36 DPU BMCs, 9 switch tray BMCs, and 8 power shelf BMCs: 71 BMC
endpoints and 54 machines. The same rack with one DPU per tray has 53
endpoints and 36 machines. The GB300 NVL72 rack profile has 18 compute trays,
9 switch trays, and 6 power shelves, which is 51 endpoints with the one DPU
per tray that the simulator models for it. The rack profiles ship in the
nico-api chart configuration under `[rack_profiles]`. Size by endpoints and
machines, then convert to racks for the profile in use.

Three things set the pace and the ceiling in the measurements:

- **State controller concurrency** sets time to `ready`, not cores. The
  machine state controller advances at most `max_concurrency` machines at a
  time, 10 by default. Refer to
  [Time to Ready Is a Concurrency Setting](../development/large-site-sizing-and-settings.md#time-to-ready-is-a-concurrency-setting).
- **The CPU limits of the Postgres primary and nico-api** are reached before
  node CPU. At the chart defaults, the field fits reach the nico-api limit of
  3 CPUs near 1,500 hosts and the Postgres primary limit of 8 CPUs near 2,500
  hosts. In the earlier 556-rack simulation, which raised both limits to 16
  CPUs, the Postgres primary was CPU-bound from about 5.8 hours after fleet-up
  until the last rack was `ready` at 10.5 hours: it read 14 or more of its 16
  cores in 50 of the 58 samples over that window and all 16 in 7 of them.
  nico-api runs as one process and held 10 to 12 CPUs from 1.5 hours after
  fleet-up on. The later 556-rack simulation raised both limits to 32 CPUs and
  ran nico-api with 32 Tokio worker threads. The primary then averaged 14 to
  15 cores an hour from 2 hours after fleet-up on and was not the limit,
  nico-api's workers were 49 percent busy over the run and 59 to 68 percent
  while machines were going `ready`, nico-api peaked at 18.9 CPUs, and the
  last rack was `ready` at 7.9 hours. That run also carried later scaling
  fixes, so the shorter time is the combined effect of the build and the
  limits. Raise the nico-api CPU limit and its Tokio worker thread count
  together: the runs set the thread count with the `TOKIO_WORKER_THREADS`
  environment variable, through the nico-api chart's `extraEnv`, to match the
  CPU limit.
- **Postgres memory** is measured as the kubelet working set, resident memory
  plus active page cache, which grows toward the memory limit over a long run
  by design, so a working set at the limit is not by itself evidence of need.
  The chart default is a 16 GiB limit, and the sizing table in
  `helm-prereqs/values.yaml` gives 16 GiB up to 5,000 hosts and 32 GiB beyond
  that, with a measurement. The observations: in the field, the fitted 7-day
  peak working set crosses 16 GiB near 480 hosts and sits at the 16 GiB limit
  on 7 of the 33 sites. In the 400-rack simulation the primary reached 27.3
  GiB under a 32 GiB limit during the ramp, then 48.7 GiB of a 64 GiB limit 47
  hours after fleet-up. In the two 556-rack simulations it peaked at 36.5 GiB
  and 36.9 GiB of a 96 GiB limit. The 400-rack run and the earlier 556-rack
  run recorded `oom_kill` 0. The later 556-rack run did not read the counter,
  but its primary's working set never reset to baseline and the same pod
  stayed primary throughout. The first complete 250-rack run finished at the
  16 GiB default. Keep the `helm-prereqs/values.yaml` table as the memory
  guidance, and treat a rising `oom_kill` count in the Postgres cgroup, not a
  working set at the limit, as the signal to raise it.

The BMC network is a sizing input too: every endpoint needs one address, so a
250-rack GB200 site needs 17,750 BMC addresses and a 556-rack site needs
39,476. Refer to
[IP and Network Configuration](../provisioning/ip-and-network-configuration.md).

API admission control is a nico-api setting to know at this scale. It bounds
the business requests nico-api executes and queues, in total and per client.
It is enabled by default with 64 requests in flight and 1024 pending in
total, 8 in flight and 64 pending per client, and a 5-second pending timeout.
The keys are the `[api_admission_control]` table in the
[configuration reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/cfg/README.md#apiadmissioncontrolconfig),
and the nico-api chart value `apiAdmissionControl.enabled` switches it. In a
simulated fleet every agent presents one client identity, so the per-client
limits apply to the whole fleet at once. The 556-rack fleet was run on the
later build with admission control enabled at raised limits, 512 requests in
flight and 8192 pending in total, 8 and 64 per client, and the 5-second
timeout: nico-api rejected 97.4 million of 113.4 million API calls in 8 hours,
86 percent, most of them DPU agent calls, 1,386 of the 30,024 machines and
no rack reached `ready`, and the simulator pods restarted 16 times at
startup on the rejections. The same fleet on the same build with admission
control disabled is the 7.9-hour run above. Disable admission control for a
simulated fleet, and keep it enabled on real hardware, where each host and DPU
agent has its own identity. On a real fleet of this size the total limits are
shared by every client, and the polls of 30,000 host and DPU agents count
against them. The figures on this page do not measure admission control on
real hardware, so watch the `carbide_api_admission_rejected_total` counter as
a fleet grows.

## Site Controller Sizing by Fleet Size

The [hardware prerequisites](prerequisites/hardware.md#site-controller) are
the baseline: 3 nodes with two 24-core sockets and 256 GiB of memory each, 512
GiB recommended. CPU figures below are logical CPUs as Kubernetes reports
them. The three nodes the simulated runs used are baseline nodes: two Intel
Xeon Gold 6442Y sockets of 24 cores each with Hyper-Threading on, 96 logical
CPUs, and eight 32 GiB DIMMs, 251 GiB of allocatable memory. Memory figures
are the node working set against allocatable memory. The simulation nodes also carried the simulator pods and
their Services, so their sampled CPU peaks include a kube-apiserver share that
a real site does not have: 19 to 47 logical CPUs in the earlier runs and 6.5
in the later 556-rack run.

| Managed hosts | Site controller | Highest per-node peaks | Postgres primary | nico-api | Evidence |
|---|---|---|---|---|---|
| Up to 2,000 | 3 baseline nodes | About 20 logical CPUs at the 95th percentile and about 104 GiB peak at 2,000 hosts, from the fitted lines below | Chart defaults, 8 CPUs and 16 GiB. The fitted 95th percentile CPU reaches 8 near 2,500 hosts, and the fitted peak working set crosses 16 GiB near 480 hosts. Refer to [What Limits Scale](#what-limits-scale) | Chart default of 3 CPUs, which the fitted 95th percentile reaches near 1,500 hosts. The 250-rack measurement gives it 8 | Field deployment, 33 sites on the v0.10, v2.0, and v2.1 lines |
| 4,500 (250 GB200 racks) | 3 baseline nodes | 54 logical CPUs and 62 GiB, the highest per-node peaks across the runs. The CPU and memory peaks are from different runs and not necessarily the same node | 16 CPUs and 32 GiB limits, about 10 CPUs used, working set 14 to 17 GiB with a two-sample spike to 26 GiB | 8 CPUs, 5 to 6 used | Simulation, v2.3 line. Refer to [Large Site Sizing and Settings](../development/large-site-sizing-and-settings.md#site-controller-sizing) |
| 7,200 (400 GB200 racks) | 3 baseline nodes | 61 logical CPUs on one node and 76 GiB on another over the first 245 minutes of the 6.9-hour run, after which node sampling stopped | 16 CPUs, 32 GiB limit raised to 64 GiB during the run. 27.3 GiB used under 32 GiB, 48.7 GiB after 47 hours | 16 CPUs and 32 GiB limits, 12.3 CPUs and 9.7 GiB used at 245 minutes | Simulation, pre-release build |
| 10,008 (556 GB200 racks) | 3 baseline nodes | 43 logical CPUs, 40 at the 90th percentile, and 99 GiB, both on the node that carried the Postgres primary and nico-api in the later run. The earlier run peaked at 75 logical CPUs on one node and 99 GiB on another, with one sample of 97 logical CPUs on the third node, a sampling artifact on a 96 logical CPU node | Give it 32 CPUs: at that limit it averaged 14 to 15 cores an hour from 2 hours after fleet-up on, under a 96 GiB limit with a 36.9 GiB peak. At 16 CPUs in the earlier run it was CPU-bound for the last 4.7 hours, with a 36.5 GiB peak | Give it 32 CPUs and 32 Tokio worker threads with the 32 GiB chart default: 49 percent of the workers busy over the run and 59 to 68 percent in the ready ramp, 18.9 CPUs and 22.8 GiB at peak. At 16 CPUs in the earlier run it held 10 to 12 CPUs and 15 GiB | Simulation, pre-release builds of the v2.4 and v2.5 lines, two completed runs |

At 556 racks the later run's highest per-node CPU peak was 43 of 96 logical
CPUs, on the node that carried both the Postgres primary and nico-api at 32
CPU limits each, and that node also held the highest memory peak, 99 GiB. The
two limits add up to 64 of the node's 96 logical CPUs, and the earlier run of
the same fleet, with 16 CPU limits, peaked at 75 logical CPUs on one node. A
fleet of that size therefore leaves little CPU headroom on 3 baseline nodes
once both limits are raised, which supports the existing recommendation in
the hardware prerequisites of 5 nodes, or nodes with more cores, for large
GB200-class sites.

The field dataset gives straight-line fits across the 33 sites, 25 of them on
the v0.10 line, 6 on the v2.0 line, and 2 on the v2.1 line. They interpolate
inside 1 to 1,930 hosts and extrapolate above it with no headroom, and 10,000
hosts is 5 times the largest measured site:

- Busiest controller node CPU at the 95th percentile: about 4 logical CPUs
  plus 8.2 per 1,000 hosts. At 4,500 hosts that is 41 and at 10,000 hosts 86.
- Busiest controller node peak memory: about 34 GiB plus 35 GiB per 1,000
  hosts, with wide scatter around the line. At 4,500 hosts that is 193 GiB and
  at 10,000 hosts 387 GiB. Most of the slope is the Postgres primary working
  set, which adds 24 GiB per 1,000 hosts.
- NICo database size: about 31 GB per 1,000 hosts, in decimal gigabytes as the
  dataset measures it, and the largest database in the dataset is about 92 GB,
  almost all of it one index, so disk is not the scaling constraint.

The simulated 556-rack fleet's highest per-node memory peak was 99 GiB against
a 387 GiB field extrapolation. The two figures measure different things: the
field fit is a 7-day peak of a working set that grows toward the Postgres
memory limit, extrapolated 5 times past the largest measured site, and the
simulation is an ingestion of 7.9 to 10.5 hours. Use the field fits for the
long-running steady state of a site, and the simulated rows for the ingestion
of a large fleet with the settings on
[Large Site Sizing and Settings](../development/large-site-sizing-and-settings.md#settings-changed-from-the-defaults).
The 400-rack and 556-rack runs changed the site explorer budget from that
page's 2000, 1000, and 300: `explorations_per_run` 3200 and
`concurrent_explorations` 480 are higher, and `machines_created_per_run` 300
is lower, because the pre-release builds create machines in rack order and
spend each run's budget on racks it can complete. All of the runs set
`max_concurrency` to 80. The later 556-rack run kept that page's
`max_database_connections` 900 and hardware-health rate limiter, and it also
set `machine_creation_concurrency` 8, so one explorer iteration creates 8
hosts at a time, `admin_lock_admission_permits` 32, and
`switch_and_power_shelf_pass_interval` 5m, so switches and power shelves are
created every 5 minutes during the host creation phase. Those three keys are
in [pull request 8142](https://github.com/dsx-ai-factory/infra-controller/pull/8142).

## Multiple Site Controllers

One site controller manages one site. NICo REST can be deployed centrally, and
several site controllers connect to it through their Site Agents, so one REST
endpoint can front more than one site. Refer to
[What is NICo?](../overview/what-is-nico.md). Tenancy objects are scoped to a
site: a VPC, an InfiniBand partition, and an allocation are created against
one `siteId`, and nothing is shared between site controllers. A tenant that
needs capacity in two sites gets a VPC in each.

Splitting a fleet across site controllers is guidance. No multi-controller
scale test has run, and the scaling area owns the open items. Consider a
split in these cases:

- The fleet exceeds the largest simulated figure above, or you want headroom
  below it.
- The Postgres primary runs at its CPU limit at steady state after the sizing
  above is applied.
- A data hall has its own BMC network, InfiniBand fabric, or HBN fabric, and
  you want the failure domain of the site controller to match it.

## Related Limits

| Limit | Value | Reference |
|---|---|---|
| BMC endpoints and machines per rack | 71 and 54 for GB200 NVL72 with 2 DPUs per tray, from the rack profile | [What Limits Scale](#what-limits-scale) |
| `max_concurrency` of the machine state controller | 10 by default, 80 to 120 recommended for large sites | [Large Site Sizing and Settings](../development/large-site-sizing-and-settings.md#time-to-ready-is-a-concurrency-setting) |
| `max_database_connections` of nico-api | 1000 by default, within the Postgres `max_connections` of 1024 that helm-prereqs sets | [Settings Changed From the Defaults](../development/large-site-sizing-and-settings.md#settings-changed-from-the-defaults) |
| API admission control | Enabled by default, with 64 requests in flight and 1024 pending in total and 8 in flight and 64 pending per client. Disabled for the simulated fleet runs that converged. The run with it enabled at raised limits stalled, refer to [What Limits Scale](#what-limits-scale) | [What Limits Scale](#what-limits-scale) and the [configuration reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/crates/api-core/src/cfg/README.md#apiadmissioncontrolconfig) |
| VPCs per site | One active VNI per VPC from the `vpc-vni` or `external-vpc-vni` pool, so the configured ranges bound the count. A VPC with an unreleased routing-profile transition holds two allocations, one in each pool, until the operator releases the previous one. Grow an established pool with `nico-admin-cli resource-pool grow`. | [VNI Resource Pools](../manuals/vpc/vni_resource_pools.md) |
| Network security group size | `max_network_security_group_size` 200 expanded rules per group, with a DPU agent ceiling on the order of 10,000 | [Network Security Groups](../manuals/networking/network_security_groups.md) |
| NVLink domain | One NVL72 rack or two NVL36 racks cabled together. Partition monitor pass time grows with configured racks. | [NVLink Partitioning](../manuals/nvlink_partitioning.md) |
| HBN route and VXLAN tunnel endpoint (VTEP) limits | Not published by NICo. Refer to the [DOCA HBN Service documentation](https://docs.nvidia.com/doca/sdk/doca-hbn-service-guide/index.html). | [Glossary](../glossary.md#hbn-in-nico) |

## Release Notes

A change to the scale table is announced in the
[Release Notes](https://docs.nvidia.com/infra-controller/documentation/release-notes)
entry of the release that ships it: a new figure, a status change such as
simulation to field deployment, a removed row, or a change to the sizing
rows. The NICo version column is updated in the same change. Release notes
are published from the changelog entries under `fern/changelog/` in the
repository. Refer to the [Release Policy](../../RELEASE.md) for how releases
are versioned and supported.
