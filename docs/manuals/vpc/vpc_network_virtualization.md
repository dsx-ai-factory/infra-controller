# VPC Network Virtualization

This page explains how VPC network virtualization connects instances
within a VPC and between instances and the internet. It ties together VNI pools, IP pools, BGP
configuration, routing profiles, and DPU behavior into a single end-to-end picture.

Refer to this page when diagnosing connectivity failures, planning a new site deployment, or
explaining the system to a network team.

**Related pages**

- [VPC Routing Profiles](vpc_routing_profiles.md) — profile configuration reference and
  troubleshooting
- [VNI Resource Pools](vni_resource_pools.md) — VNI pool configuration
- [IP Resource Pools](../networking/ip_resource_pools.md) — IP pool configuration
- [Network Prerequisites](../../getting-started/prerequisites/network.md) — underlay and EVPN prerequisites
- [DPU Configuration](../../dpu-management/dpu_configuration.md) — declarative DPU config flow
- [Flat VPCs and Zero-DPU Hosts](flat_vpcs_zero_dpu.md) — the operator-managed
  alternative for hosts without a NICo-managed DPU (no DPU overlay)

---

## The Full Picture

The following diagram shows how the principal components relate. Each DPU maintains a separate VRF
for every VPC hosted on its managed host. Routes flow between VRFs and the fabric via BGP EVPN.

```text maxLines={0}
┌─────────────────────────────────────────────────────────────────────────┐
│  Site controller (NICo API)                                             │
│                                                                         │
│  API server config                                                      │
│    asn              ──────────────────────────────► DPU BGP ASN pool    │
│    datacenter_asn   ──────────────────────────────► route-target ASN    │
│                                                                         │
│  fnn config                                                             │
│    routing_profiles[EXTERNAL/INTERNAL/…]                                │
│      route_target_imports                                               │
│      route_targets_on_exports                                           │
│      leak_* flags                                                       │
│    additional_route_target_imports                                      │
└──────────────────────┬──────────────────────────────────────────────────┘
                       │ configuration poll (every 30 seconds)
                       ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  DPU (BlueField, running HBN / Cumulus Linux)                           │
│                                                                         │
│  loopback (lo-ip pool)     ◄── VTEP for underlay BGP EVPN peering       │
│  per-VPC loopback          ◄── VTEP used as EVPN next-hop in VPC VRF    │
│    (vpc-dpu-lo pool)                                                    │
│                                                                         │
│  VPC VRF  ◄─── VPC VNI (vpc-vni or external-vpc-vni pool)               │
│    native + peered VPC RT imports: <datacenter_asn>:<imported_vni>      │
│    additional RT imports: from routing profile + fnn config             │
│    export tags: native RT + route_targets_on_exports                    │
│    site_fabric_null_routes: distance-250 blackholes in mutual isolation │
│                                                                         │
│  deny_prefixes ACL ──► blocks listed prefixes from tenant traffic       │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Component Roles

| Component | Pool / Config field | Role |
| --------- | ------------------- | ---- |
| DPU loopback IP | `lo-ip` pool | VTEP address for underlay BGP peering and EVPN sessions with route servers or TOR switches |
| Per-VPC DPU loopback IP | `vpc-dpu-lo` pool | Per-VPC VTEP; used as the EVPN next-hop for type-5 routes in that VPC's VRF |
| VPC VNI | `vpc-vni` pool (internal VPCs) or `external-vpc-vni` pool (external VPCs) | Identifies the VXLAN L3 tunnel and forms the native route-target `<datacenter_asn>:<vpc_vni>` |
| DPU BGP ASN | `fnn-asn` pool | Per-DPU ASN allocated from the pool; used in BGP session setup and route-target construction |
| Site BGP ASN | `asn` in the API server config | Ethernet-virtualization ASN; still present in the config but superseded by per-DPU ASNs |
| Datacenter ASN | `datacenter_asn` in the API server config | The ASN component used when forming route-targets from VNIs: `<datacenter_asn>:<vni>` |
| Routing profile | `fnn.routing_profiles` in the API server config | Defines `route_target_imports`, `route_targets_on_exports`, and leak behavior for a VPC's VRF |
| Additional RT imports | `fnn.additional_route_target_imports` | Extra route-targets imported into every VPC VRF site-wide |
| Deny-prefix ACL | `deny_prefixes` in the API server config | Prefixes tenant instances are not permitted to reach |
| Site fabric prefixes | `site_fabric_prefixes` in the API server config | IP prefixes assigned for tenant use within this site. |
| Site fabric null routes | `site_fabric_null_routes` in the API server config | List of IP prefixes that should receive additional L3 network isolation. Omission inherits `site_fabric_prefixes`. An empty list installs none. Do not combine an effective `/0` null route with `leak_default_route_from_underlay = true` for the same address family because the imported default overrides the equal-prefix blackhole. Open isolation does not install the routes. |

Pool definitions (ranges, prefix sizes) are configured in the API server `pools` section; see
[VNI Resource Pools](vni_resource_pools.md) and
[IP Resource Pools](../networking/ip_resource_pools.md).

### Why `datacenter_asn` and `asn` are separate

`datacenter_asn` is the ASN component embedded in route-targets (`<datacenter_asn>:<vni>`). It
identifies the datacenter to the network fabric and must match the route-target values programmed
on network devices.

`asn` is a site-wide fallback ASN inherited from the earlier
Ethernet Virtualizer model. Each DPU is now allocated a unique ASN from the `fnn-asn` pool for
VRF and BGP session use. The `asn` field is still present and used for DPU-to-host DHCP behavior.
Do not conflate the two.

Both values must agree with the network team's deployment. A mismatch causes route-targets that
NICo programs to differ from those the network devices import, resulting in a black hole.

---

## How a DPU Gets Its Configuration

The DPU agent polls NICo for its network configuration every 30 seconds. The response is
assembled from the configured pools, routing profiles, and site settings, and contains everything
the DPU needs to configure its VRFs, BGP sessions, and VTEPs.

The handler proceeds as follows:

1. **Load the managed host snapshot** from the database. If the DPU is unknown, a `NotFound`
   error is returned and the DPU places itself into isolated mode.
1. **Determine `use_admin_network`**. The DPU is placed on the admin network when no instance is
   allocated, when the instance has no interfaces configured for this DPU, or when the host is in
   specific transient lifecycle states.
1. **Resolve the routing profile**. When an instance is allocated, the VPC's routing profile is
   looked up in `fnn.routing_profiles`. If the profile name is not defined there, the DPU
   configuration call fails.
1. **Allocate or retrieve the per-VPC loopback IP** from the `vpc-dpu-lo` pool.
1. **Build tenant interface configs**, including VLAN IDs, VNI, gateway, prefixes, and the FQDN
   derived from the instance hostname or its IP address.
1. **Assemble the response**, including:
   - BGP ASN (per-DPU from `fnn-asn` pool)
   - DPU loopback IP from the `lo-ip` pool
   - DHCP server addresses and route server addresses
   - `deny_prefixes` ACL data and the effective site-isolation prefixes in `site_fabric_null_routes`
   - `datacenter_asn`
   - The resolved `routing_profile` (imports, exports, leak flags)
   - `additional_route_target_imports`
   - VPC VNI

The DPU applies the received configuration to HBN via NVUE and reports back to NICo. NICo marks
the instance as ready only after the DPU confirms the configuration has been applied. See
[DPU Configuration](../../dpu-management/dpu_configuration.md) for the full lifecycle.

---

## Default Isolation: The Admin Overlay

NICo guarantees that a managed host never carries tenant traffic unless an explicit tenant
configuration places it into a VPC. A "default VPC" in the cloud-provider sense — a system-created
VPC that every tenant inherits — does not exist. Tenants get the VPCs an operator (or the tenant
API) creates for them; absent any such VPC, an instance has no place to send tenant traffic.

The guarantee is upheld by an **admin overlay** that is separate from every tenant VPC, and by a
fail-closed default when no configuration is available at all.

### The admin VPC and admin segments

During site initialisation NICo creates an admin VPC together with a set of admin network
segments. These are not tenant-visible and exist only to give the DPU somewhere safe to attach a
host before, between, and after tenant allocations. The admin VPC follows the same VPC / VRF
mechanics as any tenant VPC — it has its own VRF on each DPU, its own VNI, and its own routing
profile — but it is reserved for NICo's own use.

### `use_admin_network`

When the API server assembles `ManagedHostNetworkConfig` for a DPU, it sets
`use_admin_network = true` whenever any of the following hold:

- The host has no instance allocated.
- The instance has no interfaces configured for this DPU.
- The host is in a transient lifecycle state (provisioning, repair, termination) in which tenant
  traffic must not flow.

With `use_admin_network = true` the DPU agent places every host interface into the admin VRF
instead of any tenant VPC's VRF. From the wire's perspective, the host is on the admin overlay
and cannot exchange traffic with any tenant instance.

### Isolated mode (fail-closed)

A DPU that cannot retrieve its configuration at all — for example, because the host is unknown to
NICo and `GetManagedHostNetworkConfig` returns `NOT_FOUND` — places itself into **isolated
mode**: every host interface is detached from any overlay until NICo issues an explicit
configuration. This is the fail-closed default. Nothing in the data path silently falls back to a
tenant network.

### Termination guard

The instance state machine blocks the termination flow until the DPU confirms that all tenant
interfaces have been moved off tenant VPCs and onto the admin overlay (or that the machine has
been tagged with a health alert preventing reuse). This is what guarantees that a tenant whose
instance has been released cannot remain on the wire as a "ghost instance" pending disk wipe.

For the per-fabric isolation story across Ethernet, InfiniBand, and NVLink, see
[Network Isolation](../network_isolation.md).

---

## Intra-VPC Connectivity

Instances in the same VPC communicate via the VPC VRF on each DPU. No operator configuration
beyond VNI pool provisioning and routing profile assignment is required to enable this.

The flow:

1. A tenant instance boots on a managed host. The DPU receives a configuration that places the
   host interface into the VPC VRF, identified by the VPC VNI.
1. The host's instance IP is advertised from the host into the DPU via a BGP session between the
   host OS and the DPU.
1. The DPU re-advertises that host route into the fabric as a BGP EVPN type-5 prefix, tagged with
   the VPC's native route-target: `<datacenter_asn>:<vpc_vni>`.
1. Every other DPU that has an instance in the same VPC imports that same route-target (it is the
   per-VPC default import) and installs the host route in its local copy of the VPC VRF.
1. Return traffic follows the per-VPC loopback IP (`vpc-dpu-lo` pool) as the EVPN next-hop.

### Site-wide additional route-target imports

`fnn.additional_route_target_imports` injects extra route-targets into every VPC VRF across the
entire site, regardless of the VPC's routing
profile. This is appropriate for routes that all VPCs must be able to reach unconditionally,
such as control-plane service VIPs. The standard import for site-controller VIPs uses route-target
`:50100` (see [Network Prerequisites](../../getting-started/prerequisites/network.md)).

---

## Internet Connectivity

A default route must be present in the VPC VRF for instances to reach destinations outside the
overlay. There are three mechanisms by which this can happen. The correct choice depends on the
site's network deployment model and must be agreed between the NICo operator and the network team.

### Mechanism 1: Explicit route-target import in the routing profile

The routing profile specifies `route_target_imports`. The network advertises a default route
tagged with one of those route-targets. The DPU imports it into the VPC VRF.

Configure this in `fnn.routing_profiles.<name>.route_target_imports`:

```toml
[fnn.routing_profiles.EXTERNAL]
internal = false
access_tier = 2
route_target_imports = [
    { asn = 11414, vni = 50500 },   # network-advertised default route for external VPCs
]
route_targets_on_exports = [
    { asn = 11414, vni = 50500 },
]
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false
```

This is the most explicit mechanism and is preferred when the network team operates a dedicated
gateway VRF that exports a default route with a known route-target.

### Mechanism 2: Default route injection via the native VPC route-target

The network advertises a default route tagged with `<datacenter_asn>:<vpc_vni>`. Because the VPC
automatically imports its own native route-target, the default route lands in the VRF without any
`route_target_imports` entry in the profile.

This mechanism relies on `datacenter_asn` and the VPC VNI matching what the network device
programs. It is operationally simpler for the NICo side but requires the network team to track
per-VPC VNIs.

### Mechanism 3: `leak_default_route_from_underlay`

`leak_default_route_from_underlay` instructs the DPU to leak the default route from the underlay
(default VRF) directly into the tenant VRF, bypassing BGP route-target mechanics entirely.

When to use this:

- The underlay already has a default route and the site lacks a BGP gateway VRF to advertise one
  into the EVPN fabric.
- The deployment is small or development-grade and full EVPN route-target coordination is not
  warranted.

When not to use this:

- Multi-tenant deployments where internet-bound traffic must be filtered or metered per VPC.
- Sites where tenant VRF isolation from the underlay is a security requirement.
- Deployments where the underlay default route changes frequently and the tenant VRFs should not
  follow it automatically.

```toml
[fnn.routing_profiles.EXTERNAL]
internal = false
access_tier = 2
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = true   # use underlay default route
leak_tenant_host_routes_to_underlay = false
```

---

## Controlled Route Leaking

The following fields in the routing profile configuration provide fine-grained control over what
crosses the boundary between the tenant VRF and the underlay.

### `leak_tenant_host_routes_to_underlay`

When `true`, the DPU leaks per-host routes from the tenant VRF into the underlay (default VRF).
This is required when the network must route return traffic directly to instances rather than
relying on an aggregate. Enable this when the network team cannot install a covering aggregate
for instance IPs.

### `tenant_leak_communities_accepted`

When `true`, the DPU honors BGP community tags set by the host OS on routes it advertises to the
DPU. Routes carrying the accepted communities are leaked from the tenant VRF into the underlay.
This allows a sophisticated tenant application to selectively announce specific prefixes to the
fabric (for example, when running a gateway workload or BYOIP scenarios).

### `accepted_leaks_from_underlay`

An explicit prefix list that controls which prefixes may be leaked from the underlay (default VRF)
into the tenant VRF. This is more granular than `leak_default_route_from_underlay`: the operator
lists specific prefixes (for example, management subnets or service VIPs) that the tenant VRF
needs, without opening the full underlay default route.

```toml
[fnn.routing_profiles.INTERNAL]
internal = true
access_tier = 1
route_target_imports = []
route_targets_on_exports = []
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false
accepted_leaks_from_underlay = [
    { prefix = "10.128.0.0/16" },   # service VIP range
]
```

---

## Export Route-Targets and Return-Path Reachability

When the DPU advertises routes from a VPC into the fabric, it tags them with:

1. The VPC's native route-target: `<datacenter_asn>:<vpc_vni>`
1. Any route-targets listed in `route_targets_on_exports` for the VPC's routing profile

The network device VRFs must import the route-targets present on VPC routes to see those routes
and return traffic to instances. If they do not, the overlay routing table appears correct from
the VPC side but external hosts cannot reach instances — the same symptom as no internet access.

This is the most common cause of one-way connectivity: the instance can send packets out, but
responses are dropped because the network cannot route back.

**Diagnostic question to ask the network team**: Is the network VRF configured to import the
route-targets listed in `route_targets_on_exports` for this VPC's routing profile?

See [VPC Routing Profiles — Troubleshooting: No Internet Access](vpc_routing_profiles.md#troubleshooting-example-no-internet-access-in-a-vpc)
for the full troubleshooting procedure.

---

## VNI Pool Selection: Internal vs. External VPCs

When a VPC is created, the VNI pool is selected automatically based on the routing profile's
`internal` flag:

- `internal = true` → allocates from the `vpc-vni` pool
- `internal = false` → allocates from the `external-vpc-vni` pool

The separation of pools allows operators to reserve distinct VNI ranges for internal and external
VPCs, which can simplify route-target policy on network devices (for example, applying a blanket
import for all external-VPC route-targets from a known VNI range).

See [VNI Resource Pools](vni_resource_pools.md) for pool sizing and configuration.

---

## Configuration Checklist for a New Site

Use this checklist when configuring VPC network virtualization for a new site. Complete each step in order and confirm
with the network team before proceeding to the next.

<Steps toc={true}>
<Step title="Agree on BGP parameters with the network team">

- [ ] Determine `datacenter_asn` — the ASN used in route-target construction.
- [ ] Determine VNI ranges for internal VPCs (`vpc-vni` pool) and external VPCs
      (`external-vpc-vni` pool).
- [ ] Determine the route-target the network will advertise for the default route.

</Step>
<Step title="Configure IP pools">

- [ ] Define the `lo-ip` pool for DPU loopback IPs. One IP per DPU.
- [ ] Define the `vpc-dpu-lo` pool for per-VPC DPU loopback IPs. One IP per DPU per VPC.
- [ ] Define the `fnn-asn` pool for per-DPU BGP ASNs. One ASN per DPU.

Refer to [IP Resource Pools](../networking/ip_resource_pools.md) for sizing guidance.

</Step>
<Step title="Configure VNI pools">

- [ ] Define the `vpc-vni` pool for internal VPCs.
- [ ] Define the `external-vpc-vni` pool for external VPCs (if the site serves external tenants).

Refer to [VNI Resource Pools](vni_resource_pools.md) for pool sizing guidance.

</Step>
<Step title="Configure API server fields">

In the API server configuration file:

```toml
asn = <site-asn>
datacenter_asn = <datacenter-asn>        # must match network device route-target config

[fnn]
additional_route_target_imports = [
    { asn = <datacenter_asn>, vni = 50100 },   # site-controller VIPs
]

[fnn.routing_profiles.EXTERNAL]
internal = false
access_tier = 2
route_target_imports = [
    { asn = <datacenter_asn>, vni = <internet-rt-vni> },
]
route_targets_on_exports = [
    { asn = <datacenter_asn>, vni = 50500 },
]
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false

[fnn.routing_profiles.INTERNAL]
internal = true
access_tier = 1
route_target_imports = []
route_targets_on_exports = [
    { asn = <datacenter_asn>, vni = 50200 },
]
leak_default_route_from_underlay = false
leak_tenant_host_routes_to_underlay = false
```

Replace placeholder values with site-specific values agreed with the network team. The route-target
numbers `:50100`, `:50200`, and `:50500` are conventional; see
[Network Prerequisites](../../getting-started/prerequisites/network.md) for the full standard route-target table.

</Step>
<Step title="Confirm network device configuration">

Have the network team confirm:

- [ ] Network device VRFs import the route-targets that external VPCs export (`route_targets_on_exports`).
- [ ] Network device VRFs export a default route tagged with either:
  - the route-target listed in `route_target_imports` for each VPC profile that needs internet
    access, or
  - `<datacenter_asn>:<vpc_vni>` for each external VPC (native route-target injection), or
  - a default route is reachable in the underlay (if using `leak_default_route_from_underlay`).
- [ ] Network device VRFs import the site-controller VIP route-target (`:50100` or equivalent).
- [ ] DPU loopback prefixes from the `lo-ip` pool are reachable from all other DPUs (full mesh or
      aggregate).

</Step>
<Step title="Verify end-to-end">

After site bring-up:

1. Create a tenant with an `EXTERNAL` routing profile.
1. Create a VPC for that tenant.
1. Allocate an instance.
1. From within the instance, confirm:
   - Intra-VPC connectivity to another instance in the same VPC.
   - Internet reachability (for example, `curl` to an external address).
1. Confirm that the DPU agent health check shows all BGP sessions established.

</Step>
</Steps>

---

## Troubleshooting

For routing-profile-specific troubleshooting, see
[VPC Routing Profiles](vpc_routing_profiles.md), which covers:

- `RoutingProfile not found` errors during VPC creation
- No internet access in a VPC
- Return-path reachability failures

**Common configuration mismatches to check first:**

| Symptom | Likely cause |
|---|---|
| VPC creation fails with `NOT_FOUND` | Routing profile name is not defined in `fnn.routing_profiles` |
| DPU reports no BGP sessions | `datacenter_asn` or `fnn-asn` pool mismatch; route servers not reachable |
| Intra-VPC connectivity works but internet access fails | `route_target_imports` missing or network not advertising the expected route-target |
| Internet access works but no return traffic | Network VRF not importing `route_targets_on_exports` |
| All connectivity fails after adding a second VPC | VNI pool exhausted; check pool capacity in [VNI Resource Pools](vni_resource_pools.md) |
| `vpc-dpu-lo` allocation error | Per-VPC DPU loopback pool exhausted; check [IP Resource Pools](../networking/ip_resource_pools.md) |
