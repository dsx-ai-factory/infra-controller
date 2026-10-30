# BGP Peering

This page describes the BGP peering relationship that NICo supports between a tenant's host operating system and the DPU that fronts the host on the overlay network. The following topics are covered:

- Intended use cases
- How ownership is divided between the tenant and the platform
- What operators must configure on the site to permit a tenant to use the feature
- The discovery mechanism by which a tenant learns the peer ASN
- An end-to-end example of BGP peering

This page is intended for operations engineers running a NICo-managed site, as well as platform engineers writing tenant runbooks. The peering session itself is owned by the tenant; this page describes the surface the platform exposes to that tenant and the configuration controls available to the site operator.

## Feature Overview

A tenant instance can establish a BGP session from its host OS to the DPU and use that session to advertise routes for prefixes that the tenant chooses to announce on the overlay. The session is point-to-point between the host OS and the DPU directly attached to that host; it is not a session to a remote route reflector or to the fabric.

The BGP session unlocks two primary use cases:

- **Anycast service Virtual IPs (VIPs)**: A tenant can run a BGP-aware load balancer (for example, MetalLB in BGP mode for a Kubernetes cluster) and announce a service VIP from every host that should receive traffic for that VIP. The fabric equal-cost multi-path routings (ECMPs) traffic across all announcing hosts. The VIP is reachable from any host on the overlay without per-host application configuration.
- **Bring-your-own-IP (BYOIP)**: A tenant can advertise prefixes that the platform did not assign to the instance (for example, a prefix the tenant owns externally) and have them treated as reachable through the announcing host.

Both use cases require the platform to permit the announced prefix at the site and routing-profile level. Refer to the [Operator Prerequisites](#operator-prerequisites) section below for more details.

## Architecture and Ownership

```text
   Tenant host OS                       DPU (BlueField)                  Fabric / Overlay
   ┌────────────────────┐              ┌─────────────────────┐
   │ BGP speaker        │   eBGP       │ HBN / NVUE          │   eBGP/iBGP   ┌─────────┐
   │ (FRR, BIRD,        │ ◄─────────►  │ (BGP responder on   │ ◄──────────►  │ Leaf /  │
   │  ExaBGP, MetalLB)  │              │  tenant interface)  │               │ spine   │
   └────────────────────┘              └─────────────────────┘               └─────────┘
        Tenant-owned                       Platform-owned                    Site fabric
```

| Boundary | Owner | Responsibility |
|---|---|---|
| BGP speaker process on the host OS | Tenant | Choose, install, and configure the speaker. Hold the host-side autonomous system number (ASN). Choose which prefixes to announce. |
| Peer endpoint on the DPU | Platform (NICo) | Materialize the BGP listener on the tenant-facing DPU interface as part of the per-host network configuration. Apply prefix filters derived from the routing profile of the VPC. Inject accepted routes into the VPC overlay. |
| Site fabric / route servers | Platform (NICo + network team) | Carry routes between DPUs and out to the overlay or external routing domain according to the VPC routing profile's route-target imports and exports. |

NICo does not configure or operate the host-side BGP speaker. The tenant chooses the software, installs it inside the instance OS, and supplies its configuration. The platform does configure the DPU to accept the session and to filter and redistribute the prefixes the tenant announces.

The session uses the external Border Gateway Protocol (eBGP): The tenant ASN and the DPU ASN are different administrative domains. The tenant peers with the DPU using the IP address the DPU has on the tenant interface, which is the same address the tenant's host already receives as its default gateway via DHCP. No additional address discovery is required for the peer endpoint.

## Operator Prerequisites

Before a tenant can peer successfully, the operator must ensure the following are in place on the site.

### 1. Tenant Host Autonomous System Number (ASN) Policy

The site configuration controls whether tenants may choose any host-side ASN or must use a single common value.

| Config field | Effect |
|---|---|
| `common_tenant_host_asn` unset | Any ASN is accepted from the tenant. Each tenant may choose its own. |
| `common_tenant_host_asn = <ASN>` | Only sessions with a tenant-side ASN that equals this value are accepted. This is useful for sites that require predictable peering behavior or that re-use the same operator-provided tenant ASN across all hosts. |

If a site enforces a `common_tenant_host_asn`, the value of that ASN must be communicated to tenants out of band; it is not currently advertised through the metadata service.

### 2. Allowed Anycast Prefixes (per Routing Profile)

The set of prefixes a tenant may announce is governed by the *routing profile* assigned to the VPC. Each FNN routing profile has an `allowed_anycast_prefixes` list. A tenant's BGP advertisement is accepted only if the announced prefix falls inside one of the allowed entries in the routing profile.

This list lives under `fnn.routing_profiles.<name>.allowed_anycast_prefixes` in the API server configuration. The list is per-routing-profile, so two VPCs at the same site can have different prefix-announcement policies. Refer to the [VPC Routing Profiles](../vpc/vpc_routing_profiles.md) page for the broader routing-profile configuration model.

If `allowed_anycast_prefixes` is empty for the VPC's routing profile, no tenant advertisements will be accepted on that VPC. A tenant's BGP session may still come up, but no routes will be installed.

- **Availability**: The per-routing-profile `allowed_anycast_prefixes` field landed on `main` in PR #1780 (commit `baae1d7dd`). The most recent tagged release that does **not** include it is `v0.9.0-rc06`; the first release tagged after that point is the cutover. Sites running an older build must rely on the predecessor configuration described below.

- **Predecessor: `anycast_site_prefixes` (deprecated)**: Before PR #1780, the allow-list was a single site-wide list at the top level of the API server configuration: `anycast_site_prefixes`. That field is still recognized for backwards compatibility but is **deprecated** and should not be used on new sites. It applied uniformly to every VPC and could not express per-routing-profile policy. Sites that previously relied on `anycast_site_prefixes` should migrate to `fnn.routing_profiles.<name>.allowed_anycast_prefixes`: copy the existing prefix list into the `allowed_anycast_prefixes` entry of each routing profile that should be permitted to announce those prefixes, then remove the top-level field.

### 3. Allowed Tenant Leak Communities (Optional)

By default, a prefix advertised by the tenant and accepted on the DPU is reachable only through the VPC overlay. A site can additionally permit the tenant to control two extra DPU behaviors per prefix by attaching well-known BGP communities to its advertisements. The communities are honored only when the VPC routing profile is set to `tenant_leak_communities_accepted = true`.

| Community | Tenant intent | DPU behavior when honored |
|---|---|---|
| `65100:01` — **allow leak to underlay** | "Make this prefix reachable from the site underlay in addition to the overlay." | The accepted prefix is leaked from the tenant VRF into the underlay/default VRF and exported to the fabric outside the overlay path. |
| `65100:02` — **allow drop from overlay** | "Make this prefix reachable from the underlay instead of the overlay." | Same leak-to-underlay action as `65100:01`, and additionally the matching EVPN/overlay route is suppressed so that traffic returns via the underlay path. |

If `tenant_leak_communities_accepted` is `false` (the default), communities attached by the tenant are ignored. The prefix may still be accepted on the overlay if it lies inside `allowed_anycast_prefixes`, but neither leak-to-underlay nor drop-from-overlay will take effect.

These communities are a per-prefix control: Tenants attach the community in their BGP speaker to exactly the prefixes that should leak or be dropped from the overlay, and omit it from prefixes that should remain overlay-only. The DPU strips any tenant-attached communities after using them, so they are not re-exported to the fabric.

### 4. Route Targets for Reachability

Permitting a tenant to announce a prefix only causes the route to enter the VPC local routing table on that DPU. For the announced prefix to be reachable from other hosts in the VPC, or to be reachable from outside the VPC, the VPC routing profile must export the prefix on appropriate route targets and importing VRFs must be configured to import them. This is part of normal routing-profile design and is not specific to host-DPU BGP. Refer to the [VPC Routing Profiles](../vpc/vpc_routing_profiles.md) page for guidance.

## How the Tenant Discovers the Peer ASN

Every DPU at the site is allocated its own unique ASN from a site-managed pool. There is *no single shared "DPU ASN"* that a tenant can hard-code: The peer ASN that a host can see depends on which DPU fronts that host, and two instances on different hosts will generally peer with different ASNs. Tenants must therefore look up the ASN dynamically, per host.

The DPU BGP ASN is exposed to the tenant through the `cloud-init-compatible` Instance Metadata Service on the DPU. Its default IPv4 endpoint is the link-local address `169.254.169.254`.

A tenant retrieves the peer ASN with a plain HTTP GET:

```text
GET http://169.254.169.254/latest/meta-data/asn
```

The response body is the ASN as a decimal integer. The metadata endpoint has
no HTTP authentication and uses the local HBN metadata attachment. The examples
on this page use the standalone IPv4 listener on port 80. For embedded port 7777,
the optional IPv6 endpoint, and the request and reply path, refer to
[NICo Metadata Service](../../dpu-management/dpu-lifecycle-management.md#nico-metadata-service).

The value returned reflects the ASN assigned to the DPU that fronts the host. It is the peer ASN the tenant must configure into the BGP speaker *for that host*; tenants that operate fleets of hosts must query the endpoint on each host rather than assuming a single value.

The tenant's own ASN is whatever the site policy permits (refer to the [Tenant Host ASN Policy](#1-tenant-host-autonomous-system-number-asn-policy) section above for more details).

## Tenant Configuration Steps

The following steps are performed by the tenant inside their host OS. NICo does not perform these steps; they are listed here so that operators and runbook authors have a complete picture of the end-to-end flow.

1. **Install a BGP speaker.** Any standard BGP speaker is supported. Common choices include the following:

   - FRRouting (`frr`)
   - BIRD
   - ExaBGP
   - MetalLB (for Kubernetes service VIPs; runs in BGP mode)
2. **Retrieve the peer ASN** from the metadata service:

   ```bash
   curl --noproxy '*' http://169.254.169.254/latest/meta-data/asn
   ```

3. **Determine the peer IP address.** This is the host's existing default gateway (assigned via DHCP from the DPU tenant-facing interface). It can be discovered with the following command:

   ```bash
   ip route show default
   ```

4. **Determine the tenant-side ASN.** This is either the tenant's own choice, or--if the site has set `common_tenant_host_asn`--the operator-provided value.
5. **Configure the BGP speaker** with the peer IP, the peer ASN, and the local ASN. Configure the speaker to announce only the prefixes the tenant intends to advertise, and to either accept no inbound routes or to filter inbound routes according to the tenant's own policy. The DPU side will filter outbound advertisements against the `allowed_anycast_prefixes` list set for the VPC; any prefix outside that list will be silently dropped by the platform.
6. **(Optional) Attach leak communities** to specific advertisements that should leak to the underlay (`65100:01`) or be reachable via the underlay instead of the overlay (`65100:02`). These communities take effect only when the VPC routing profile sets `tenant_leak_communities_accepted = true`; refer to the [Allowed Tenant Leak Communities](#3-allowed-tenant-leak-communities-optional) section for more details.
7. **Bring up the session.** The session should establish within a few seconds. If it does not, refer to the [Troubleshooting](#troubleshooting) section below.

The tenant must ensure that the prefixes the speaker announces are actually present on the host (for example, bound to a loopback interface) so that traffic forwarded to the announcing host has a place to land.

## Example: MetalLB Announcing a Kubernetes Service VIP

The following example shows a tenant using MetalLB in BGP mode to announce a Kubernetes `LoadBalancer` service VIP from every node in a Kubernetes cluster running on a NICo-provided VPC.

**Assumptions**

- The site permits the prefix `203.0.113.0/24` in the VPC routing profile (`allowed_anycast_prefixes`).
- The tenant has chosen `65010` as its host-side ASN; the site does not pin `common_tenant_host_asn`.
- The VPC DPU ASN, retrieved from the metadata service, is `65000`.
- The default gateway of the host is `10.0.0.1`.

**Step 1 — Confirm the peer ASN on one node.**

```text
$ curl --noproxy '*' -s http://169.254.169.254/latest/meta-data/asn
65000
```

**Step 2 — Configure MetalLB.**

```yaml
apiVersion: metallb.io/v1beta2
kind: BGPPeer
metadata:
  name: dpu
  namespace: metallb-system
spec:
  myASN: 65010
  peerASN: 65000
  peerAddress: 10.0.0.1
---
apiVersion: metallb.io/v1beta1
kind: IPAddressPool
metadata:
  name: service-vips
  namespace: metallb-system
spec:
  addresses:
    - 203.0.113.10/32
---
apiVersion: metallb.io/v1beta1
kind: BGPAdvertisement
metadata:
  name: service-vips
  namespace: metallb-system
spec:
  ipAddressPools:
    - service-vips
```

**Step 3 — Create a `LoadBalancer` service that consumes the pool.**

MetalLB allocates `203.0.113.10` to the service and advertises `203.0.113.10/32` to the DPU from every node that runs an endpoint for the service. The DPU accepts the advertisement because `203.0.113.0/24` is present in the routing profile's `allowed_anycast_prefixes`. The fabric ECMPs traffic destined to `203.0.113.10` across all announcing nodes.

**Step 4 — Verify on the host.**

Verify that the session is established and the prefix is being advertised. With FRRouting (FRR), the following commands are used:

```bash
vtysh -c "show bgp summary"
vtysh -c "show bgp ipv4 unicast neighbors 10.0.0.1 advertised-routes"
```

Other speakers expose equivalent commands.

## Troubleshooting

### The BGP session does not establish

1. Confirm TCP reachability to the DPU on port 179:

   ```bash
   nc -vz <default-gateway> 179
   ```

   If TCP/179 is not reachable, the DPU is not currently configured as a BGP responder for this host. Verify that the instance is fully provisioned (`READY` state) and that the VPC has been built with a routing profile that permits the feature.
2. Confirm the peer ASN configured in the host speaker matches the value returned by `meta-data/asn`. An ASN mismatch will cause OPEN messages to be rejected.
3. If the site sets `common_tenant_host_asn`, confirm that the tenant's local ASN matches that value exactly. If it does not, the OPEN will be rejected.

### The session is up but no routes are visible from other hosts

1. Confirm that the announced prefix is fully contained in one of the `allowed_anycast_prefixes` entries in the routing profile. Prefixes outside the allow-list are silently dropped on the DPU.
2. Confirm that the VPC routing profile exports the relevant route-targets and that downstream VRFs import them. Without correct route-target plumbing, the prefix lives only in the local table of the announcing DPU and is not visible elsewhere. Refer to the [VPC Routing Profiles](../vpc/vpc_routing_profiles.md) page for more details.
3. Confirm that traffic forwarded to the announcing host actually lands somewhere. The host must have the prefix bound to an interface (typically a loopback) so that the kernel does not drop the inbound packets.

### A specific prefix the tenant wants to announce is being rejected

The prefix is not present in the `allowed_anycast_prefixes` list of the VPC routing profile. Either an operator must add the prefix to that profile, or the tenant must announce a different prefix that is permitted. The platform does not negotiate this dynamically; it is a configuration decision made at the routing-profile level.

### A prefix is tagged with `65100:01` or `65100:02` but is not leaking to the underlay

Confirm that the VPC routing profile has `tenant_leak_communities_accepted = true`. When the flag is `false`, the DPU strips the community without acting on it; the prefix may still be accepted on the overlay, but no leak or drop will occur. The setting is per routing profile, so different VPCs at the same site can have different policies.

## Summary

The following is a summary of essential BGP settings, the ownership of each settings, and where each setting lives.

| Concern | Owner | Location |
|---|---|---|
| BGP speaker on the host OS | Tenant | Inside the instance OS |
| Peer IP address | Platform | DPU tenant interface; delivered to the host as the DHCP default gateway |
| Peer ASN (unique per DPU) | Platform | `http://169.254.169.254/latest/meta-data/asn`; must be re-queried per host |
| Tenant-side ASN policy | Operator | `common_tenant_host_asn` in API server configuration |
| Allowed announced prefixes | Operator | `fnn.routing_profiles.<name>.allowed_anycast_prefixes` |
| Tenant leak communities (`65100:01` allow leak to underlay, `65100:02` allow drop from overlay) | Operator gates; tenant opts in per prefix | `fnn.routing_profiles.<name>.tenant_leak_communities_accepted` |
| Prefix reachability beyond the VPC | Operator + network team | Route-target imports/exports on the VPC routing profile and on fabric VRFs |
