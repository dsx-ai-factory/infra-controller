# Control Plane Networking

This page states what the datacenter network must provide **before** a NICo site can be
brought up, and how those provisions map onto the two files that consume them:

- the **control-plane DPU site file** (`scripts/control-plane-dpu/site-sample.yaml`),
  from which `build-dpu-install-iso.sh` renders each site controller DPU's HBN
  configuration into the install ISO;
- the **site config TOML**: the API server configuration, which you supply as the
  `siteConfig.nicoApiSiteConfig` block in `helm-prereqs/values/nico-core.yaml` and
  which is rendered into the nico-api ConfigMap as `nico-api-site-config.toml`. It
  drives nico-api's pools, networks and routing profiles. Other pages call it "the API
  server configuration"; this page says "the site config TOML".

Both files ask for the *same* numbers, seen from two sides of one fabric. Agree the
numbers once with your network team (the team that operates the datacenter fabric),
using the worksheet in section 2, and fill both files from it.

The page describes *what* the fabric must do, not *how* to configure any vendor's
equipment. It complements [Network Prerequisites](../../docs/getting-started/prerequisites/network.md) (underlay, EVPN peering
options, the standard route-target table), [IP and Network
Configuration](../../docs/provisioning/ip-and-network-configuration.md) (the Day 0 pools,
segments, DHCP and DNS), the VPC manuals linked from section 1.7, and the operator
runbook in
[`scripts/control-plane-dpu/README.md`](README.md).

## Scope

The page describes the reference design in which every site controller has a DPU that is
its only path into the fabric ([Network Prerequisites → Option 2: Dual-homed
Uplink](../../docs/getting-started/prerequisites/network.md#option-2-dual-homed-uplink-reference-design)).
Site controllers without DPUs are supported but outside the scope of this page; section
1.3 notes how the ToR takes the DPU's role for them.

## The datacenter design this page assumes

A datacenter carries its out-of-band management prefixes, the addresses of its servers'
BMCs, DPU management ports, switches and other managed devices, in one of two ways: in
its global routing table, where anything that routes there can reach them, or inside a
Secure Management Network. This page recommends and describes the second, and the
shipped samples assume it. Sites whose out-of-band prefixes stay in the global routing
table are supported, but they are outside the scope of this page.

### The Secure Management Network

A Secure Management Network (SMN) is a way of building the out-of-band management
network so that only the VRFs meant to reach the managed devices can reach them. Instead
of routing the management prefixes in the fabric's global routing table, the datacenter
places them in EVPN VRFs of its own, one or more for each system that uses it, and
exports each VRF's prefixes with a route target; a device behind that VRF is reachable
only from a VRF that imports the target. The management network itself, its devices,
addresses and switches, does not change. What changes is how the fabric carries it: in
VRFs with route targets instead of the global routing table.

For NICo the SMN primarily carries the *Management Network for Managed Hosts* of
[Network
Prerequisites](../../docs/getting-started/prerequisites/network.md#ip-address-pools).
That network holds the host BMC, DPU BMC and DPU out-of-band addresses of every managed
host, one plus two per DPU. NICo allocates these addresses. The datacenter holds them in
one SMN VRF that exports `:900`. The SMN also carries the other management segments the
site controllers reach (1.4.2): the jump hosts, and the rack devices that Network
Prerequisites counts under the managed-hosts heading, NVLink switches and power shelves,
which live in device-class VRFs of their own.

The admin network of managed hosts is **not** part of the SMN. It is a datacenter
allocation that NICo carries in its own overlay, the admin VPC on the managed hosts'
DPUs, exported with `:50400`.

The site controllers' own out-of-band addresses, the *Control Plane Management Network*
of [Network
Prerequisites](../../docs/getting-started/prerequisites/network.md#ip-address-pools),
are allocated by the datacenter. They are outside NICo's reach by design, and NICo never
imports them (1.5.2). The datacenter may carry them in a companion `<X>-SC` VRF exported
with `:901`, or on a plain management VLAN.

## Terms

Site controller, route target, FNN, VNI, VRF, VTEP, EVPN, BGP, OOB, BMC, HBN
and DPU are used as defined in the [Glossary](../../docs/glossary.md). The site controller
DPU configuration described here is the FNN-mode render (`startupSMN.template`), in
which the control-plane host link sits in a VRF with an L3VNI and route-target-driven
import and export, like any VPC. The Secure Management Network (SMN) is defined
above, and external versus internal VPCs in section 1.7.

## 1. What the fabric must provide

Each item below is a requirement the network team can check and answer yes or no.
Section 2, which maps the agreed values onto the two configuration files, refers to these
items by number.

### 1.1. Allocations

#### 1.1.1. The datacenter ASN

The datacenter must provide NICo the ASN it uses in every route target it originates
(`<datacenterAsn>:<n>`). NICo needs the value; it does not choose it.

#### 1.1.2. The VNI block

The datacenter must allocate NICo one contiguous VNI block from the datacenter-wide VNI
space and must guarantee that nothing else, including the SMN VRFs' own L3VNIs, uses
numbers inside it. **Starting point: every number in the shipped configuration falls inside the
block `50000–65000`, laid out as in the table below.** In a new datacenter, where the VNI
space is still free, ask for exactly that block and the files work as shipped. If the
datacenter already uses some of those numbers, either it frees them or you change NICo's
numbers in both files and in the route-target table of 1.4; every one of them is
configurable, and changing them before the site exists is far cheaper than moving a live
datacenter VRF.

| Range | Assumed use |
|---|---|
| `50000–50999` | reserved for the common route-target numbers (`:50100`…`:50500`); never used as a VNI (see below) |
| `51000–55999` | external tenant VPCs (`[pools.external-vpc-vni]`); individual VNIs need no registration with the datacenter (1.7.1) |
| `56000–56999` | L2VNIs of the admin segments (`[pools.vni]`), ten per site (1.1.2.1) |
| `57000–59999` | unused; free for the operator's other needs |
| `60000–60099` | control-plane VNIs, one per NICo site (`fnn.controlPlaneVni`) |
| `60100–65000` | internal tenant VPCs (`[pools.vpc-vni]`); the samples put the admin VPC (`[fnn.admin_vpc].vpc_vni`) at the first number of the site's range |
| outside the block | `helm-prereqs/values/nico-core.yaml` leaves `[pools.vni]` empty and gives `1024500–1024800` as the example; the minimal Helm example and the Helm prerequisites hardcode that range. If it is used, the datacenter must guarantee that `1024500–1024800` is used nowhere else. Otherwise the L2VNIs move inside the block, into `56000–56999` |

NICo derives every VPC's native route target from its VNI as `<datacenterAsn>:<vni>`
([VNI Resource
Pools](../../docs/manuals/vpc/vni_resource_pools.md#what-vnis-are-used-for)). Two rules
follow.

- **Site operator.** The site operator must never use a number from the first range as a
  VNI. A VPC with VNI 50100 would carry the same tag as the common site-controller
  target, and every VRF importing `:50100` would import that tenant's routes.
- **Datacenter.** The datacenter must not use `<datacenterAsn>:<n>` as a route target on
  any VRF of its own for any `n` inside NICo's block, except the common tags of 1.4 that
  it imports and exports as agreed. Every other value in the block is a native target
  NICo may assign.

The site operator divides the block into the ranges that the site config TOML and the
DPU site file ask for, keeping them disjoint. The block must cover two kinds of VNI:

- **L3VNIs of the VPC VRFs.** One per VPC: the admin VPC, the **control-plane VNI** (the
  L3VNI of the VRF the site controller DPU places its host link in, one per NICo site,
  shared by all of the site's controller DPUs), and one for every tenant VPC that may
  exist at the same time, drawn from the internal and external pools.
- **L2VNIs of the admin segments** (`[pools.vni]`). The admin network is an L2 segment
  stretched across every managed host's DPU, so each admin segment carries an L2VNI on
  the fabric, and that L2VNI must be datacenter-unique like any other ([Network
  Prerequisites → VNI
  Allocations](../../docs/getting-started/prerequisites/network.md#vni-allocations) asks
  for one per site). NICo draws the value from `[pools.vni]` when it creates the segment
  at startup. Because the value cannot be pinned, the whole range must be
  datacenter-unique. The matching `[pools.vlan-id]` value is DPU-internal: the DPU
  presents the admin segment untagged to the host, so it needs no datacenter
  coordination and no per-site separation (section 2).

The pool sizes given here and in 1.1.2.1 assume the FNN design of this page. Two other
virtualization types, both outside the scope of this page, size `[pools.vni]` and
`[pools.vlan-id]` differently. Sites that run Ethernet Virtualizer VPCs create one
network segment per tenant segment through the segment API, and each draws an L2VNI that
is programmed on the fabric and a VLAN ID for the host link, so both pools must hold one
value per concurrent tenant segment and the whole VNI range must be datacenter-unique.
Sites with zero-DPU hosts declare HostInband segments, which draw a value from each pool
without programming it anywhere. They need one value per declared segment, like the
admin segments.

##### 1.1.2.1. How to size the block

Count the L3VNIs as the number of tenant VPCs that may exist at the same time plus two
(the admin VPC and the control-plane VNI); all of them must be datacenter-unique. Count
the L2VNIs as the number of admin segments the site may ever declare. Each site should
take at least ten, which covers a fleet that grows in bursts and needs more admin
segments. Add headroom to both counts. A site planning for 200 concurrent VPCs needs
roughly 200 L3VNIs and ten L2VNIs.

##### 1.1.2.2. One block, several sites

The datacenter allocates this block once and shares it among every NICo site
in the datacenter, because all sites ride the same EVPN fabric and a VNI is a
datacenter-wide identifier. Each site must therefore take its own disjoint slice of
every range: its control-plane VNI, its admin VPC VNI, its tenant pools, its registered
external VNIs and its L2VNI range. The sample layout leaves `60000–60099` for up to a
hundred sites' control-plane VNIs.

The route-target numbers behave the opposite way. `:50100`…`:50500` are formed with the
datacenter's ASN, so every site in the datacenter shares them. The operator must give
each site its own `siteControllerRoutesAsn`; that is what keeps two sites' control-plane
routes apart, since each exports `:50100` under a different ASN. The admin and tenant
tags carry the datacenter ASN and are therefore common to all sites: a VRF that imports
`:50400` sees every site's admin network (1.8.2). Whether sites can reach each other is
decided by the tags, never by sharing a VNI. One operator running several sites obtains
one management reach by importing the shared tags into its own VRFs. Sites that belong
to different operators, or that must be kept apart, must use per-site tag numbers
(1.8.2).

Control-plane prefixes, the loopbacks, host links and service VIPs, are addresses in one
underlay. They must be disjoint per site in every case.

#### 1.1.3. The ASN range

The datacenter must allocate NICo an ASN range, sized as described in [Network Prerequisites
→ Autonomous System Numbers](../../docs/getting-started/prerequisites/network.md#autonomous-system-numbers-asns): one 32-bit ASN
per managed-host DPU, plus headroom. The site controllers take a **separate,
non-overlapping range** from the same allocation: one ASN per site controller node for
its DPU (`bgpAsnStart + nodeId`) and one shared ASN for the host side of the peering
(`bgpAsnStart`, which is MetalLB's `myASN` on every node). A site with three site
controllers, for example, uses four consecutive ASNs. The managed-host pool
`[pools.fnn-asn]` must not overlap them.

#### 1.1.4. IP prefixes

The datacenter must allocate the following IP prefixes, typically carved from one
aggregate per site.

##### 1.1.4.1. DPU loopbacks

The loopbacks of the site controllers' and managed hosts' DPUs are the VXLAN tunnel
endpoints. They live in the underlay and must be routable between every DPU and ToR.

##### 1.1.4.2. Site controller host links

Each site controller needs one `/31` between its host and its DPU.

##### 1.1.4.3. Control-plane service VIPs

A prefix, usually a `/27`, holds the addresses MetalLB announces.

##### 1.1.4.4. Admin network

The admin network holds the addresses managed hosts use while no tenant is assigned to
them. NICo needs one address per managed host regardless of how many DPUs it has
([Network Prerequisites → Admin
Network](../../docs/getting-started/prerequisites/network.md#admin-network)).

##### 1.1.4.5. Tenant address space

The tenant address space is the set of aggregates from which tenants carve their VPC
prefixes (subnets), listed in the site config TOML's `site_fabric_prefixes`. Every
subnet lies entirely inside one aggregate, and the datacenter routes each aggregate as
one block, so a class of tenant space that the datacenter must route differently needs
an aggregate of its own. A real site needs the following:

- **External tenant space**, for the site operator's customers, routed through the
  tenant gateways. Every site has this one.
- **Internal tenant space**, for the site operator's own users, routed through the
  operator's own egress. Needed when the operator runs internal VPCs.
- **Break-fix space**, from which the repair tenant's VPCs draw, reachable only from the
  break-fix tooling VRF. Needed when the site runs a `MAINTENANCE` profile.

Each instance interface takes a `/31` from a subnet: one address for the host, one for
its DPU. This is the "two addresses per host per tenant network" that [Network
Prerequisites](../../docs/getting-started/prerequisites/network.md#tenant-networks)
counts. A host with several interfaces therefore consumes several `/31`s. Size each
aggregate for the number of instance interfaces it will carry, not for the number of
hosts.

##### 1.1.4.6. Managed hosts' out-of-band prefixes

These prefixes hold the addresses of each managed host's BMC and of every DPU's BMC and
out-of-band port. The count per host depends on the number of DPUs: one for the host BMC
plus two per DPU, so three for a host with one DPU, five with two, nine with four
([Network Prerequisites → Management Network for Managed
Hosts](../../docs/getting-started/prerequisites/network.md#management-network-for-managed-hosts)).

The datacenter must allocate one prefix per rack, sized for that count times the hosts
in the rack, plus the gateway, plus the DHCP relay agent's address where it differs from
the gateway, plus growth. The relay address must lie inside the prefix, because nico-api
selects the segment by the prefix that contains it; it does not have to be the gateway
address ([IP and Network
Configuration](../../docs/provisioning/ip-and-network-configuration.md#22-dhcp-configuration-for-physical-machine-interfaces)).

Under-sizing this space is a common mistake, and its symptom is late and confusing: the
first hosts in the rack come up normally, then the BMCs and DPUs that ask for an address
after the prefix is exhausted never get one and never appear in NICo, and ingestion of
the rack stalls with nothing on the NICo side to explain why (the refused requests show
only in the `nico-dhcp` logs). Size for the full rack and for the DPU count of the hosts
actually installed, not for one DPU per host.

The datacenter carries these prefixes in the managed hosts' SMN VRF (1.5.1). NICo assigns
the addresses from them by DHCP, so every such prefix must also be declared as a `type =
"underlay"` network segment in the site config TOML with a DHCP relay pointing at the
NICo DHCP VIP ([BMC and Out-of-Band
Setup](../../docs/getting-started/prerequisites/bmc-oob-setup.md)).

### 1.2. Underlay

#### 1.2.1. eBGP unnumbered on the site controller ToR ports

[Network Prerequisites](../../docs/getting-started/prerequisites/network.md#underlay-and-bgp-configuration) requires eBGP
unnumbered (RFC 5549) on every ToR port that faces a DPU, accepting the DPU's ASN as
external. For the site controller DPUs, each of the two ToR ports must additionally meet
the following:
- The port is a **routed layer-3 port**, not a switched or bonded one. `p0` and `p1`
  are two independent uplinks, and the site controller configuration runs no LACP.
- **IPv6 link-local addressing with router advertisements** is enabled on the port;
  unnumbered peering rides on it.
- The session is configured `remote-as external`, accepting whatever ASN the DPU
  presents (`bgpAsnStart + nodeId`).
- **Both address families run on the one session**, IPv4 unicast and L2VPN EVPN (1.3.1).
- The **MTU** is large enough for the host MTU (`siteControllerMtuSize`) plus the
  VXLAN overhead of about fifty bytes, end to end across the fabric ([Network
  Prerequisites → General Guidance](../../docs/getting-started/prerequisites/network.md#general-guidance)).
- The filters of 1.2.3 apply.
- **No BGP session password is configured.** The site controller DPU is configured from the
  install ISO, before any NICo service exists, and the configuration built into the ISO
  has no password field; a ToR that demands one leaves those sessions down and the
  provisioning procedure in `scripts/control-plane-dpu/README.md` fails.

Both uplinks must be cabled and configured. The site controller DPU peers on both. It
should be noted that, as of now, the site controller DPU runs no agent and has no health
check, so nothing reports a down uplink; the failure becomes visible only when the
remaining uplink fails as well.

#### 1.2.2. Loopback reachability

Every DPU `/32` loopback must be reachable from every other DPU and ToR, whether through
full routes, an aggregate, or a default route. This is what the VXLAN tunnels ride on.

#### 1.2.3. Filters

The ToR must accept from a site controller DPU only what it should announce: its `/32`
loopback, its host `/31`, and `/32` host routes from the service VIP prefix. Each
DPU-facing port must carry a max-prefix limit, so that a misconfigured DPU cannot flood
the fabric. The fabric may summarize these routes at the leaf or pod boundary; it is not
required to carry them as individual `/31`s and `/32`s beyond it.

### 1.3. Overlay

#### 1.3.1. EVPN to the site controller DPUs

The ToRs must peer EVPN with the site controller DPUs, dual-stacked with IPv4 on the
same session. The site controller DPU configuration has no route-server peer group, so
its ToR sessions must carry **both** IPv4 unicast and L2VPN EVPN. The fabric must
propagate EVPN between the ToRs and the rest of the fabric (spines or route reflectors)
so that routes tagged with the targets in 1.4 reach every VRF that imports them.

#### 1.3.2. Site controllers without DPUs

Site controllers that connect to the ToRs through plain NICs ([Network Prerequisites →
Option
1](../../docs/getting-started/prerequisites/network.md#option-1-single-uplink-logical-separation))
are outside the scope of this page. In that design the ToR takes over the role the DPU
plays here: MetalLB peers with the ToR, and the ToR exports the service VIP `/32`s and
the site controllers' host prefixes into EVPN tagged `:50100` and performs the imports
of 1.4 on their behalf. Such site controllers have no DPU site file; the site config TOML side
of section 2 applies unchanged.

### 1.4. Route-target ownership (who exports, who imports)

NICo owns and originates a small fixed set of **common route targets**, listed in 1.4.1,
and the datacenter must import each of them in every VRF that needs to reach the routes
it marks. A common target stands for a whole class of routes: importing `:50200` once,
for example, brings in every internal tenant's prefixes, present and future, without the
network team tracking each VPC's own native target (`<datacenterAsn>:<vni>`) as VPCs
come and go. That is why NICo tags every route twice, with its native target and with
the common target of its class: the two teams never have to coordinate per prefix. The
mechanism is described in [VNI Resource Pools → Simplifying network team
coordination](../../docs/manuals/vpc/vni_resource_pools.md#simplifying-network-team-coordination)
and [VPC Network Virtualization → Export Route-Targets and Return-Path
Reachability](../../docs/manuals/vpc/vpc_network_virtualization.md#export-route-targets-and-return-path-reachability).

#### 1.4.1. Route targets that NICo exports

Every route target in this table has the form `<asn>:<number>`. For the admin and tenant
tags the ASN half is the datacenter ASN allocated in 1.1.1. The control-plane tag
`:50100` is the exception: the site controller DPUs export it under
`siteControllerRoutesAsn` (section 2, README Step 1). The shipped sample sets that field equal to the
datacenter ASN, and it differs only when several sites share one datacenter (1.1.2.2).
The second half is a number from NICo's conventions, listed below. The network team must
know both halves, because the datacenter imports these targets by their full value. A
tag is advertised only while a VPC of the corresponding routing profile exists. Routing
profiles are defined in the `[fnn.routing_profiles]` section of the site config TOML;
see [VPC Routing Profiles](../../docs/manuals/vpc/vpc_routing_profiles.md).

| RT number | Meaning | Exported by | What the datacenter must do |
|---|---|---|---|
| `:50100` | site controller control plane: host `/31`s and service VIP `/32`s | the site controller DPUs, as `<siteControllerRoutesAsn>:50100` | The SMN VRFs and every other VRF that must reach NICo services must import it (1.6.1). |
| `:50500` | external tenant routes (the normal tenant for a provider) | the managed-host DPUs, for VPCs in the `EXTERNAL` profile, in addition to each VPC's native `<datacenterAsn>:<vni>` | The internet gateway VRF must import it to learn every external VPC's prefixes (Mechanism 1 in [VPC Network Virtualization → Internet Connectivity](../../docs/manuals/vpc/vpc_network_virtualization.md#internet-connectivity)). A gateway VRF that serves one VPC with its own default route imports that VPC's native target instead (Mechanism 2, 1.7.1). |
| `:50400` | admin network routes | the managed-host DPUs, for the admin VPC | Every VRF that must reach hosts' admin addresses must import it (1.6.2). |
| `:50200` | internal tenant routes (operator's own users) | the managed-host DPUs, for VPCs in the `INTERNAL` profile | The shared internal egress VRF imports it ([VPC Network Virtualization](../../docs/manuals/vpc/vpc_network_virtualization.md#internet-connectivity)). |
| `:50300` | maintenance (break-fix) routes | the managed-host DPUs, for VPCs in the `MAINTENANCE` profile | The break-fix tooling VRF imports it. Optional; used only when the site runs a `MAINTENANCE` profile. |

#### 1.4.2. Route targets that the datacenter must export

| RT | Meaning | What NICo does |
|---|---|---|
| `:900` (conventional) | The datacenter exports this target on the SMN VRF that holds the managed hosts' out-of-band prefixes: the host BMCs, the DPU BMCs and the DPU out-of-band ports (1.5.1). NICo's DHCP, Redfish and console services reach the managed hosts through these prefixes, and the DHCP relays on the out-of-band switches source their requests from them. | The site controller DPUs must import it. |
| `:901` (conventional) | The datacenter exports this target on the VRF that holds the site controllers' own out-of-band segment: the controllers' host BMCs, DPU BMCs and DPU out-of-band ports (1.5.2). It exists only when the datacenter carries that segment in an SMN VRF rather than a plain VLAN. It is kept apart from `:900` so that the out-of-band path to the site controllers never depends on the site controllers' own DPUs. | The site controller DPUs must never import it. |
| targets of other SMN segments | The datacenter exports one target per SMN segment the site controllers must reach. Typical segments are the jump hosts or utility cluster from which the site operator's staff administer NICo, the power-shelf controllers and NVLink switch controllers of the racks NICo manages, and the management endpoints of the storage NICo provisions. Each site needs only the targets of the segments its services actually talk to. | The site controller DPUs import them through the per-site list `fnn.routeTargetsToImport` (1.4.3). |
| a summary target for all SMN segments | Some datacenters attach one additional target to every SMN prefix, so that a single import brings in the whole SMN. Such a summary also carries the segments the site controllers must not see, including the `:901` segment above. | An optional shortcut. The site controller DPUs must import the individual targets instead, unless the datacenter scopes the summary to the segments NICo needs (1.5.2). |
| default-route targets | The datacenter's gateway VRFs export a default route into the overlay under an agreed target, one per egress path: the internet-facing gateway for external tenants and the shared internal egress for internal tenants. The alternatives, injecting the default route under each VPC's native target or leaking it from the underlay, are described in [VPC Network Virtualization → Internet Connectivity](../../docs/manuals/vpc/vpc_network_virtualization.md#internet-connectivity). | The tenant routing profiles import them. The site controller DPUs do not. Whether the admin VPC needs one is an open question. |

#### 1.4.3. The import list of the site controller DPUs

The two tables above are ordered by who exports a target. This subsection collects, from
both of them, what the site controller DPUs import. They always import three targets:
`:900` for the managed hosts' out-of-band prefixes (1.4.2), `:50100` for the other site
controller nodes of the site and `:50400` for the admin VPC (1.4.1). All three are
imported under the datacenter ASN. When `siteControllerRoutesAsn` differs from the
datacenter ASN (1.1.2.2), the site's own `:50100` routes therefore fall outside the fixed
import, and the operator must add `<siteControllerRoutesAsn>:50100` to
`fnn.routeTargetsToImport` (section 2).

In addition, the site controllers must import **the common tag of every tenant routing
profile the site uses**. The reason is the return path. Hosts in a tenant VPC send DHCP
(relayed by their DPU), PXE and image pulls, DNS, NTP and metadata requests to the
service VIPs, and the site controllers must route the replies back to the tenant prefix.
That prefix is advertised with only two tags, the VPC's native target and its profile's
common tag, so the site controllers must import the common tag. The mechanism is the
`fnn.routeTargetsToImport` list of the DPU site file (section 2): `startupSMN.template`
renders every entry in it into the same EVPN import list as the three targets above.

In practice, a site with only external tenants imports `:50500`; one with internal
tenants as well adds `:50200`; `:50300` is needed only if a `MAINTENANCE` profile is in
use. [Network Prerequisites](../../docs/getting-started/prerequisites/network.md#route-targets) states this as "import `:50200`
through `:50500`", meaning the tags in use, not all four.

### 1.5. SMN requirements

The SMN is defined in the introduction of this page. This section states what the datacenter must provide for it.

#### 1.5.1. One SMN VRF for the site's managed hosts

The datacenter must provide one SMN VRF for the managed hosts of this NICo site. That
VRF must contain the out-of-band prefixes of every rack whose hosts NICo manages: the
host BMC, DPU BMC and DPU out-of-band addresses. Racks that NICo does not manage stay
outside it. The VRF must export those prefixes into EVPN as type-5 routes tagged `:900`.
It must import `:50100`. It must import **nothing tenant-related** (1.8.1). The `:50100`
import is what lets the hosts answer NICo. It also carries the DHCP relays' traffic.
[BMC and Out-of-Band Setup](../../docs/getting-started/prerequisites/bmc-oob-setup.md)
requires a relay on every BMC-facing segment. Those relays sit on this VRF's gateway
interfaces, so their requests to the NICo DHCP VIP and the replies to their `giaddr`
both travel through this VRF.

#### 1.5.2. The site controllers' own out-of-band segment

The datacenter owns the site controllers' out-of-band segment and NICo never imports it.
The datacenter must allocate the addresses of the site controllers' host BMCs, DPU BMCs
and DPU out-of-band ports (a handful of nodes with static configuration, and no DHCP
relay to NICo needed) and must decide how to carry the segment. A plain management VLAN
is sufficient. On the other hand, datacenters with an SMN usually give each NICo site a
companion VRF (`<name>` and `<name>-SC`) and export the `-SC` one with `:901` for their
own operators' access. Whichever it is, **no NICo component reaches the site
controllers' own BMCs**, and the site controller DPU must **not** import that route
target. Importing it would expose the controllers' BMCs to the site controller VRF and,
through the VRF leak, to the control plane hosts and everything that reaches them.

#### 1.5.3. Import policy of the SMN VRFs that NICo must have access to

The managed hosts' SMN VRF (1.5.1) must import `:50100` and nothing else that is
NICo-related. The same policy applies to every other SMN VRF that holds devices NICo
manages (1.6.1): import `:50100`, and never a tenant tag (1.8.1).

#### 1.5.4. The word "underlay" in the site config TOML

The site config TOML declares the out-of-band prefixes as network segments of
`type = "underlay"`. In that file the word means a physical management network that is
not a NICo overlay. It says nothing about how the datacenter carries the prefixes. A
segment of this type may sit in the fabric's global routing table or in an SMN VRF. This
page assumes the latter.

### 1.6. Reachability policy

#### 1.6.1. Every datacenter VRF that must reach NICo services imports `:50100`

Typically these are:

- every SMN VRF holding devices NICo manages or provisions: the managed hosts' VRF (1.5.1),
  power-shelf management, compute-fabric switch management;
- the egress VRF that supplies NICo's default route, so that traffic from the backbone
  side can return to the VIPs;
- the VRF of the operator jump hosts that must reach the site controllers or the NICo
  service VIPs (a jump host that only needs the SMN does not need this import);
- the user-storage VRF.

Rule of thumb: if a device gets an address from NICo, is inventoried by site explorer, or
is reached by an operator through NICo's services, its VRF must import `:50100`.

#### 1.6.2. Every datacenter VRF that must reach managed hosts' admin addresses imports `:50400`

Typically two VRFs import it:

- the egress VRF that supplies the default route to infrastructure tenants;
- the VRF of the operator jump hosts that must reach managed hosts on the admin network.

No SMN VRF should import `:50400`, and the admin VPC's routing profile should not import
`:900`. A host on the admin network therefore cannot reach any BMC over the network, its
own included. This is deliberate: a host is on the admin network before it has been
validated and after a tenant has released it, and those are the moments it is least
trusted (1.8.1).

### 1.7. Tenant connectivity

An **external** VPC serves users outside the site operator's organization, that is, the
site operator's customers. An **internal** VPC serves the site operator's own users.
Mechanically the difference is the routing profile's `internal` flag, which selects the
VNI pool and the export tags ([VPC Routing
Profiles](../../docs/manuals/vpc/vpc_routing_profiles.md), [VNI Resource
Pools](../../docs/manuals/vpc/vni_resource_pools.md)). Tenant VPCs reach destinations
outside the overlay through a default route that a datacenter VRF injects into the VPC
VRF: the internet gateway VRF for external VPCs, the internal egress VRF for internal
VPCs. How that default route is delivered, by an explicit route-target import, under the
VPC's native target, or by a leak from the underlay, is tenant connectivity, not
control-plane connectivity. [VPC Network Virtualization → Internet
Connectivity](../../docs/manuals/vpc/vpc_network_virtualization.md#internet-connectivity)
describes the three mechanisms.

#### 1.7.1. External VNIs and the default route

The datacenter's internet gateway VRF delivers the default route to external VPCs in one
of two ways ([VPC Network Virtualization → Internet
Connectivity](../../docs/manuals/vpc/vpc_network_virtualization.md#internet-connectivity)).
In the common case, Mechanism 1, the datacenter must provide an internet gateway VRF
that imports `:50500`, so that it learns every external VPC's prefixes, and must export
from it one default route under a target agreed with the site operator, which the
EXTERNAL routing profile imports (1.4.1, 1.4.2). Both settings are class-wide, so the
datacenter needs no knowledge of individual VNIs. NICo assigns them from
`[pools.external-vpc-vni]`, inside the block of 1.1.2, without further coordination.

Mechanism 2 serves a VPC that needs a different default route, a dedicated upstream for
instance. For that VPC the datacenter exports the default route tagged with the VPC's
native target `<datacenterAsn>:<vni>` from the gateway VRF that serves it, and imports
the same target there. This is done after the VPC exists, once NICo reports its VNI, so
no VNI has to be known in advance in either case. The comments in the shipped
`helm-prereqs/values/nico-core.yaml`, which ask for the external VNI values to be agreed
with the network team, assume Mechanism 2 for every VPC.

#### 1.7.2. Control-plane reachability of tenant VPCs

As discussed in previous sections, tenant routing profiles import `:50100` to reach the
service VIPs (1.6.1), and the site controllers import the common tag of every routing
profile in use so that replies find the tenant prefix (1.4.3). Neither import requires a
datacenter action.

### 1.8. Security boundary

#### 1.8.1. The SMN and the tenants never meet

No SMN VRF may import a tenant tag (`:50200`, `:50500`, or any VPC's native target), and
no tenant routing profile may import an SMN route target (`:900`, `:901`, or any other
SMN segment). Either import would let a tenant instance reach BMCs and DPU management
ports, its own included. The only profile that imports `:900` is the operator-reserved
`PRIVILEGED_INTERNAL` profile for NICo's own services, and it must never be assigned to
a customer tenant. With the out-of-band prefixes in an SMN VRF exporting `:900`, this
rule holds by construction, because no tenant VRF imports that target; nothing depends
on a filter that somebody has to keep correct.

#### 1.8.2. Several NICo sites in one datacenter

A datacenter may export one shared route target from several SMN VRFs, one per site or
data hall, each with its own L3VNI. A site controller importing that target then sees
every site's out-of-band prefixes. Sites that must be isolated from each other must be
given per-site SMN route targets. The NICo-side counterpart, shared admin and tenant
tags versus per-site tag numbers, is in 1.1.2.2.

---

## 2. Customize your site

Two input files carry the agreed values into a site: the control-plane DPU site file,
which `build-dpu-install-iso.sh --control-plane-config <site file>` renders into one
`startup.yaml` per site controller from `startupSMN.template`, and the site config TOML.
Both take their values from the worksheet below. Each row names the item of section 1
the value comes from, an example, and the field it lands in for each file. What the
build script does with each DPU site file field is described in the [README, Step
1](README.md#step-1--prepare-the-site-config). The meaning of the site config TOML
fields is described in [VPC Routing
Profiles](../../docs/manuals/vpc/vpc_routing_profiles.md), [VNI Resource
Pools](../../docs/manuals/vpc/vni_resource_pools.md) and [IP and Network
Configuration](../../docs/provisioning/ip-and-network-configuration.md).

Fictional example values. The datacenter provides the left column; the site operator
copies each row into both files.

| Fabric item | What to agree | Example | DPU site file | Site config TOML |
|---|---|---|---|---|
| 1.1.1 | datacenter ASN | `4200000100` | `datacenterAsn` | `datacenter_asn` |
| 1.1.3 | ASN range | `4200100000–4200100999` | `bgpAsnStart: 4200100000` (site controllers use `…000`–`…003`), `siteControllerRoutesAsn: 4200100000` | `[pools.fnn-asn]` `4200100100–4200100999` |
| 1.2.1 | BGP session password on DPU-facing ToR ports | **none on site controller ports** (required); optional site-wide secret on managed-host ports | — (no field; the ISO-built configuration cannot carry one today) | `bgp_leaf_session_password = "site_wide"` + `nico-admin-cli credential bgp set-sitewide` (managed hosts only) |
| 1.1.2 | NICo VNI block (example layout); no other VNI in it, and no datacenter route target `<datacenterAsn>:<n>` with `n` in it other than the common tags | `50000–65000` | `fnn.controlPlaneVni: 60000` | `[fnn.admin_vpc].vpc_vni = 60100`; `[pools.vpc-vni]` `60101–60199`; `[pools.vni]` `56000–56009` (L2VNIs, admin segments); `[pools.vlan-id]` any ten values, DPU-internal |
| 1.1.4 | DPU loopbacks | `10.10.0.0/26` | `forgeDpuLoopbackPrefix: 10.10.0.0/28` | `[pools.lo-ip]` `10.10.0.16–10.10.0.62`; `[pools.vpc-dpu-lo]` disjoint from the optional `fnn.vpcVrfLoopbackPrefix` |
| 1.1.4 | site controller host `/31`s (DPU design) | `10.10.1.0/29` | `forgeControlPlanePrefix` | MetalLB peers `10.10.1.0`, `.2`, `.4` (without DPUs: the ToR addresses, 1.3) |
| 1.1.4 | service VIPs | `10.10.2.0/27` | `forgeServiceVipPrefix` | MetalLB pools inside `10.10.2.0/28` (internal) and `10.10.2.16/28` (external) |
| 1.1.4 | admin network | `10.10.64.0/22` | — | `[networks.admin]` |
| 1.1.4.5 | tenant address space (external, internal, break-fix aggregates) | `10.30.0.0/16`, `10.31.0.0/16` | — | `site_fabric_prefixes` = the aggregates; `deny_prefixes` = admin network, out-of-band prefixes, and the control-plane prefixes the site decides to withhold |
| 1.1.4 | per-rack out-of-band prefixes (in the SMN) | `10.20.<rack>.0/24` | — | DHCP scopes are selected by the relay addresses ([IP and Network Configuration](../../docs/provisioning/ip-and-network-configuration.md#22-dhcp-configuration-for-physical-machine-interfaces)) |
| 1.5.1 | SMN RT, managed hosts | `:900` | `fnn.commonManagedNodeBmcRouteTarget: 900` | `PRIVILEGED_INTERNAL` import |
| 1.5.2 | site controllers' own OOB segment (datacenter's; SMN companion RT if it has one) | `:901`, or a plain VLAN | not imported | — |
| 1.6.1 | NICo control-plane RT | `:50100` | `fnn.commonSiteControllerRouteTarget: 50100` | `[fnn].additional_route_target_imports` (site-wide), or every profile's `route_target_imports` |
| 1.6.2 | NICo admin RT | `:50400` | `fnn.commonAdminNetworkTarget: 50400` | admin VPC export |
| 1.7.1 | external tenant VNI range | `51000–51255` for up to 256 concurrent tenant VPCs, inside the block; no per-VNI registration under Mechanism 1 | — | `[pools.external-vpc-vni]` = that range; tenants default to `EXTERNAL` |
| 1.7.1 | default-route target for external VPCs (Mechanism 1) | agreed value, `<datacenterAsn>:<n>`; exported by the internet gateway VRF, which also imports `:50500` | — | `[fnn.routing_profiles.EXTERNAL].route_target_imports` = that target (the shipped sample uses `:50500`, under clarification) |
| 1.4 | other SMN segments the site controllers must reach | jump hosts `:101`, power shelves `:1003` | `fnn.routeTargetsToImport` | `PRIVILEGED_INTERNAL` imports |
| 1.4.3 | routing profiles in use (common tags the site controllers must import) | `EXTERNAL` only → `:50500` | `fnn.routeTargetsToImport` | `[fnn.routing_profiles.<name>]` definitions ([VPC Routing Profiles](../../docs/manuals/vpc/vpc_routing_profiles.md)) |

The `vni = N` field of every route-target entry in the site config TOML is a
route-target number, not a VNI, and no VXLAN tunnel with that number exists (1.1.2).

**Coupling the tooling does not check.** The ASN, loopback and `/31` values baked into
the ISO are re-entered by hand as MetalLB BGP peers during prerequisite deployment and
must match (with DPU-less site controllers the peers are the ToRs instead, 1.3.2).
MetalLB's address pools must fall inside the two halves of the service VIP prefix. The
loopback prefixes must not overlap. The VNI ranges of 1.1.2 must be disjoint. A lint for
these is planned.

What each site controller DPU then does, so the network team knows what to expect:
default VRF with the `/32` loopback as VTEP source and eBGP
unnumbered on `p0`/`p1` with IPv4 unicast + EVPN (1.2.1, 1.3.1); the host link `/31` in
`vpc_<controlPlaneVni>` peering with the host; type-5 export of the host link and VIP
`/32`s tagged `<siteControllerRoutesAsn>:50100`; imports of `:900`, `:50400`, `:50100`,
the per-site list, and the VRF's own auto target; bidirectional leak with the default
VRF. The build renders this FNN-mode template (`startupSMN.template`) when the
site file has an `fnn:` block, which is the configuration this page describes. A site
file without that block renders the non-FNN `startup.template` instead; that mode is
supported but beyond the scope of this page.

---

## 3. The questions from issue #5358, by reference

1. *What is SMN?* Secure Management Network: the datacenter's out-of-band management
   overlay, segmented into VRFs per system that uses it (defined in the introduction;
   requirements in section 1.5).
2. *SMN vs FNN?* FNN is NICo's overlay model; the SMN is the datacenter's. The FNN-mode
   site controller configuration is what consumes the SMN, hence "FNN/SMN mode".
3. *Which prefix does the SMN advertise, who originates it, what must the DPU import?*
   The managed hosts' out-of-band prefixes, originated by the datacenter's SMN VRF,
   tagged `:900`; the DPU imports `:900` (1.5.1).
4. *What is SMN return traffic?* Managed hosts answering NICo through the SMN VRF's
   `:50100` import (1.5.1, 1.6.1).
5. *Who exports which route targets?* Section 1.4, both tables.
6. *Which are mandatory?* `:900` (datacenter), `:50100` and `:50400` (NICo). Everything
   else is per-site import or profile-specific; `:901` is never imported (1.5.2).
7. *ToR requirements?* 1.2.1–1.3.1; dual-stacked sessions are mandatory for site controllers.
8. *BMC reachability design?* An SMN VRF exporting `:900`, the design this page assumes
   and the shipped samples expect (introduction, 1.5.1). A site without an SMN is
   supported but outside the scope of this page.
9. *When is the non-FNN template valid?* When the site file has no `fnn:` block. It is
   supported; this page covers the FNN-mode configuration only (section 2).
