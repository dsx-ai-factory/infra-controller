# Capacity Planning by Reference Architecture <Badge intent="info">v2.4</Badge> <Badge intent="launch" minimal>New</Badge>

This page is for operators and capacity planners who have an order of racks
and need to know what it asks of NVIDIA Infra Controller (NICo). It answers
three questions. How do racks of a reference architecture convert into the
units that NICo and the components it orchestrates consume? Which component
sets the limit for a site? How many site controllers does a fleet need? The
unit is the managed baseboard management controller (BMC). The measured
figures live on [Sizing and Scale Guidance](sizing-and-scale.md), and this
page is the conversion method beside them. Every figure on it comes from the
sizing page, a shipped configuration file, or the DOCA Platform Framework
(DPF) API, or is arithmetic shown from those, and names its source. The
figures change only on the sizing page, under its
[Release Notes](sizing-and-scale.md#release-notes) rule.

## The Capacity Unit: Managed BMC

A managed BMC is a BMC endpoint that the site controller holds credentials
for, probes, and reconciles. In a rack that is the host BMC of every compute
tray, the BMC of every data processing unit (DPU), the BMC of every NVLink
switch tray, and the BMC of every power shelf. A host is one compute tray.
Refer to [Managed BMC](../glossary.md#managed-bmc).

"Machine" is NICo's term for a host or a DPU, the two device types it runs a
machine state machine for. Switch trays and power shelves are managed BMCs
but not machines. Refer to [Machine](../glossary.md#machine). On the sizing
page, the BMC endpoints explored row counts managed BMCs and the Machines
(hosts plus DPUs) row counts machines. Refer to the
[Single Site Controller Scale Table](sizing-and-scale.md#single-site-controller-scale-table).

The unit is managed BMCs rather than hosts or DPUs because NICo's work
scales with the devices in the racks, not with the racks. Refer to
[What Limits Scale](sizing-and-scale.md#what-limits-scale). The number of
DPUs per compute tray differs between systems, so a host count or a DPU
count alone does not transfer between reference architectures. A managed
BMC count does, because it counts the endpoints themselves.

## Managed BMCs per Rack

The table gives the per-rack counts the sizing page states under
[What Limits Scale](sizing-and-scale.md#what-limits-scale). The GB200 NVL72
row with 2 DPUs per tray is the fleet the sizing page measured. The GB200
NVL72 row with 1 DPU per tray is the formula below applied to the same rack
and has not been simulated. The GB300 NVL72 row is the simulator's model of
that rack, with the one DPU per tray it models, and no GB300 fleet has been
measured.

| Rack type | Compute trays | DPUs per tray | Host BMCs | DPU BMCs | Switch tray BMCs | Power shelf BMCs | Managed BMCs | Machines |
|---|---|---|---|---|---|---|---|---|
| GB200 NVL72, 2 DPUs per compute tray | 18 | 2 | 18 | 36 | 9 | 8 | 71 | 54 |
| GB200 NVL72, 1 DPU per compute tray | 18 | 1 | 18 | 18 | 9 | 8 | 53 | 36 |
| GB300 NVL72, 1 DPU per compute tray | 18 | 1 | 18 | 18 | 9 | 6 | 51 | 36 |

For any system, including one without a shipped rack profile, convert from
its own BMC inventory:

```text
managed BMCs per rack      = compute trays x (1 + DPUs per tray) + switch trays + power shelves
machines per rack          = compute trays x (1 + DPUs per tray)
DPUs per rack              = compute trays x DPUs per tray
NVLink partitions per rack = the partitions the tenants create, 0 outside an NVLink domain
```

The DPUs per tray count is a property of the system configuration as
ordered. Take it from that configuration, and recompute a row when an order
differs from the table. The rack profiles under `[rack_profiles]` in the
nico-api chart configuration carry minimum counts of compute trays, switch
trays, and power shelves that the rack state machine uses to decide when a
rack's expected devices are present and ready. They do not carry the totals
above and have no DPU field. Refer to
[Rack State Machine](../architecture/state_machines/rackstatemachine.md).

A host without DPUs counts as one managed BMC, one machine, one host, 0
DPUs, and 0 NVLink partitions. Refer to the flat virtual private cloud (VPC)
page, [Flat VPCs and Zero-DPU Hosts](../manuals/vpc/flat_vpcs_zero_dpu.md).
The simulated fleets on the sizing page ran 2 DPUs per tray, and the largest
field site on that page ran one DPU per host, so a large fleet of hosts
without DPUs is outside the measured range.

## Component Limits That Can Bind

A site's capacity is the smallest of several limits, each in its own unit.
Name the binding component next to any site figure, because a figure without
it reads as a NICo limit when it may be a DPF, fabric, or network limit. The
NICo figures are the largest fleets measured, not hard limits. Refer to
[Guidance, Not a Hard Limit](sizing-and-scale.md#guidance-not-a-hard-limit).
Status words are the sizing page's, and rows marked to be determined
(`TBD`) have no measurement. Refer to
[How to Read the Table](sizing-and-scale.md#how-to-read-the-table). This
page adds one status, Component limit: a limit set by the component's own
API or configuration rather than by a NICo measurement. The racks column is
the figure divided by the per-rack count, rounded down.

| Component | Unit | Figure | Status | Per GB200 NVL72 rack, 2 DPUs per tray | Racks at the figure | Reference |
|---|---|---|---|---|---|---|
| NICo site controller | Managed BMCs | 39,476 | Simulation, pre-release build | 71 | 556 | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), BMC endpoints explored |
| NICo site controller | Machines | 30,024 | Simulation, pre-release build | 54 | 556 | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), Machines |
| NICo site controller | Hosts | 10,008 | Simulation, pre-release build | 18 | 556 | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), Hosts under management |
| NICo site controller | DPUs | 20,016 | Simulation, pre-release build | 36 | 556 | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), DPUs |
| NICo site controller | Hosts | 1,930 | Field deployment | 18 | Not a bound: the largest site observed, with one DPU per host and no rack counts collected | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), Hosts under management |
| DPF DPUCluster | DPUs per DPUCluster | 1,000 | Component limit | 36 | 27 per DPUCluster | `spec.maxNodes` of the DPUCluster API in DPF v26.4.0 and [DPF Setup, DPUCluster](../manuals/dpf.md#33-dpucluster) |
| NICo NVLink partition monitor | NVLink partitions | 10,008 | Simulation, pre-release build | 18 in the measured fleet, one per host | 556 | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), NVLink partitions |
| Host Based Networking (HBN) fabric | DPUs in one fabric | 10,000 | Roadmap | 36 | Not a bound: 277 per fabric at the target | [Scale table](sizing-and-scale.md#single-site-controller-scale-table), DPUs in one HBN fabric, and [issue 2940](https://github.com/dsx-ai-factory/infra-controller/issues/2940) |
| Rack Management Service (RMS) | Racks per RMS instance | `TBD` | `TBD` | 1 | `TBD` | [RMS Deployed Components](../architecture/backends/rms.md#deployed-components): one replica with no shared state, reached through the RMS mock in the simulations |

Notes on the rows:

- **DPF.** A DPU is DPF-managed when the site enables DPF and its host is
  marked DPF-managed in the expected machines. Refer to
  [Mark hosts as DPF-managed in expected machines](../manuals/dpf.md#37-mark-hosts-as-dpf-managed-in-expected-machines).
  A site that does not use DPF skips this row. The NICo reference
  installation creates one DPUCluster with `maxNodes: 1000`, rendered from
  `helm-prereqs/operators/dpf/dpucluster.yaml.tmpl`, and `setup.sh` has no
  option for more. In
  [DPF v26.4.0](https://github.com/NVIDIA/doca-platform/releases/tag/v26.4.0),
  the release that `helm-prereqs/doca-platform.pin` pins, the DPUCluster API
  sets 1,000 as both the default and the largest value `spec.maxNodes`
  accepts, so a site controller with more than 1,000 DPF-managed DPUs needs
  `ceiling(DPUs / 1,000)` DPUClusters. DPF supports several DPUClusters and
  places each DPU in one of them by bin packing, and nico-api leaves the
  cluster selector unset on the objects it creates, so every DPUCluster is
  eligible. No NICo fleet has run with several DPUClusters, and the simulated
  fleets on the sizing page provisioned DPUs through the DPF simulator, so
  neither path below is measured with a real DPF. Plan one of two ways. One
  site controller per 1,000 DPF-managed DPUs stays within what the reference
  installation creates, and it caps a site at 27 GB200 NVL72 racks with 2
  DPUs per tray or 55 GB300 NVL72 racks with 1 DPU per tray, with the
  controller nodes of every added site. Several DPUClusters under one site
  controller needs no NICo change, and it needs a measurement before relying
  on it. The sizing page's field row of 1,913 DPUs does not state whether
  DPF managed them.
- **NVLink.** The figure is the partition monitor total of the simulated
  556-rack fleet, which created one partition per host through the NMX
  Controller (NMX-C) mock. Count the partitions your tenants will create,
  and 0 for hosts outside an NVLink domain. No figure exists for a real
  NMX-C, and partition monitor pass time grows with configured racks. Refer
  to [Related Limits](sizing-and-scale.md#related-limits).
- **Addresses.** Every managed BMC needs an address on the out-of-band (OOB)
  network, and hosts, DPUs, and switch trays need further addresses on the
  admin and underlay segments. The shipped 556-rack values size the three
  segments at 39,476, 30,024, and 25,020 addresses. The segment layout is a
  sizing input, not a component row. Refer to
  [Capacity and Network Setup](../provisioning/ip-and-network-configuration.md#capacity-and-network-setup)
  and to the segment comments in `helm-prereqs/values/nico-core-10k-hosts.yaml`.

## How Many Site Controllers

1. List the rack types and counts in the order, with the DPUs per compute
   tray of each, and any hosts without DPUs.
1. Convert each rack type to managed BMCs, machines, hosts, DPUs, and NVLink
   partitions per rack with the table or the formula, count each host
   without DPUs as the paragraph above says, and sum each unit across the
   order.
1. For each NICo row with a simulation figure, divide the order's sum by the
   figure and round up. The largest result is the number of site controllers
   that keeps every sum inside the measured range, and the row that produced
   it is the NICo dimension closest to its measured figure. The NICo rows
   are counts of one measured fleet, not independent limits. When their
   ratios differ, the order's mix differs from the measured fleet. Use the
   managed BMC and machine rows to compare load, use the hosts row to pick
   the sizing row in step 7, and report every ratio.
1. Divide the order's DPF-managed DPUs by 1,000 and round up. The result is
   the number of DPUClusters, and the DPF note above says how to plan site
   controllers from it.
1. Compare the sums with the field and roadmap rows and report whether the
   order exceeds them. Neither is a bound.
1. Split the racks across the site controllers so that each site's sums stay
   at or below every simulation figure and each site's DPF-managed DPUs fit
   its planned DPUClusters at 1,000 each, and report how close each sum is.
   The figures are the largest fleets measured, so treat a sum near a figure
   as a reason to split or to plan a measurement. Refer to
   [Multiple Site Controllers](sizing-and-scale.md#multiple-site-controllers).
1. Name the binding component next to the result, then size each site
   controller from
   [Site Controller Sizing by Fleet Size](sizing-and-scale.md#site-controller-sizing-by-fleet-size).
   Its simulation rows are GB200 NVL72 fleets with 2 DPUs per tray: 17,750
   managed BMCs at 4,500 hosts (250 racks), 28,400 at 7,200 hosts (400
   racks), and 39,476 at 10,008 hosts (556 racks). Its first row is the
   field dataset. Take the smallest row at or above the site in hosts and in
   managed BMCs.

Every simulated fleet on the sizing page was GB200 NVL72 only, so a mixed
order is compared by its summed units and the mix itself is outside the
measured range.

Example A: 300 GB200 NVL72 racks with 2 DPUs per tray.

```text
Managed BMCs   300 x 71 = 21,300   ceiling(21,300 / 39,476) = 1   54 percent of the figure
Machines       300 x 54 = 16,200   ceiling(16,200 / 30,024) = 1   54 percent
Hosts          300 x 18 =  5,400   ceiling(5,400 / 10,008)  = 1   54 percent
DPUs           300 x 36 = 10,800   ceiling(10,800 / 20,016) = 1   54 percent
Partitions     300 x 18 =  5,400   ceiling(5,400 / 10,008)  = 1   54 percent, one per host

NICo rows      1 site controller, sized from the 7,200-host row
DPF row        ceiling(10,800 / 1,000) = 11 DPUClusters
HBN row        10,800 exceeds the 10,000 roadmap target
```

One site controller by the NICo rows. With DPF-managed DPUs the DPU layer
binds: 11 DPUClusters under one site controller, which is not measured, or
11 site controllers. The order exceeds the HBN roadmap target, which has no
measurement.

Example B: a mixed order of 200 GB200 NVL72 racks with 2 DPUs per tray and
100 GB300 NVL72 racks with 1 DPU per tray.

```text
Managed BMCs   200 x 71 = 14,200   100 x 51 = 5,100   total 19,300   ceiling(19,300 / 39,476) = 1   49 percent
Machines       200 x 54 = 10,800   100 x 36 = 3,600   total 14,400   ceiling(14,400 / 30,024) = 1   48 percent
Hosts          200 x 18 =  3,600   100 x 18 = 1,800   total  5,400   ceiling(5,400 / 10,008)  = 1   54 percent
DPUs           200 x 36 =  7,200   100 x 18 = 1,800   total  9,000   ceiling(9,000 / 20,016)  = 1   45 percent
Partitions     200 x 18 =  3,600   100 x 18 = 1,800   total  5,400   ceiling(5,400 / 10,008)  = 1   54 percent, one per host

NICo rows      1 site controller, sized from the 7,200-host row
DPF row        ceiling(9,000 / 1,000) = 9 DPUClusters
HBN row        9,000 is below the 10,000 roadmap target
```

One site controller by the NICo rows. With DPF-managed DPUs it is 9
DPUClusters under one site controller, which is not measured, or 9 site
controllers.

## What This Page Does Not Cover

- Time to ready and the settings that set it. Refer to
  [Time to Ready Is a Concurrency Setting](../development/large-site-sizing-and-settings.md#time-to-ready-is-a-concurrency-setting)
  and [Settings Changed From the Defaults](../development/large-site-sizing-and-settings.md#settings-changed-from-the-defaults).
- Tenant-side limits such as network security groups and Virtual Extensible
  LAN (VXLAN) network identifier (VNI) pools. Refer to
  [Related Limits](sizing-and-scale.md#related-limits).
