# IP and Network Configuration

This guide is the single Day 0 reference for IP address management and in-band/out-of-band network configuration for a NICo deployment. It walks an operator through every IP pool that must exist, how DHCP and DNS are served, and how to verify each piece end-to-end. Follow this page once during initial site bring-up; subsequent host ingestion and tenant operations rely on the configuration described here.

The values here are entered into the `siteConfig` TOML block of `helm-prereqs/values/nico-core.yaml` (see [Quick Start Guide, Step 3c](../getting-started/quick-start.md#3c-configure-nico-core-site-deployment)) and into your site DNS and DHCP relay infrastructure. This page does not replace per-topic references — it consolidates them in the order an operator needs them. Sizing formulas, switch configuration, and BMC ingestion details are linked rather than duplicated.

---

## Prerequisites

Before configuring the items on this page, complete:

- [Hardware](../getting-started/prerequisites/hardware.md) — server, DPU, and BMC inventory.
- [Network Prerequisites](../getting-started/prerequisites/network.md) — VNI/ASN allocation, BGP/EVPN, route targets, and switch configuration.
- [BMC and Out-of-Band Setup](../getting-started/prerequisites/bmc-oob-setup.md) — physical BMC connectivity, DHCP relay, and credentials.

This page assumes the underlay and overlay routing decisions described in those pages have already been made.

---

## 1. IP Pool Allocation

NICo consumes IP addresses from three distinct sources:

| Source | Owner | Used for |
|---|---|---|
| The parent datacenter | External infrastructure (not NICo) | Control-plane management network IPs (site controller node BMCs, K8s node IPs) |
| Operator-supplied subnets configured at install time | NICo `siteConfig` | Admin network, DPU loopbacks, VPC loopbacks, tenant networks |
| The OS install image | Site operator (static assignment) | Site controller host OS ↔ DPU PF representor `/31` |

Plan capacity for all pools **before** running `setup.sh`. Pools can be grown at runtime without a restart, but an exhausted pool blocks the next provisioning operation immediately. A 20–25% headroom margin over expected peak allocation is a reasonable default.

For per-pool sizing formulas (servers, DPUs, VPCs, instance types), see [Network Prerequisites — IP Address Pools](../getting-started/prerequisites/network.md#ip-address-pools).

### 1.1 Loopback Address Pools

NICo defines two named loopback pools in the `[pools.<name>]` section of `siteConfig`:

| Pool name | Allocation unit | Required | Purpose |
|---|---|---|---|
| `lo-ip` | One IP per managed DPU | Yes | DPU loopback advertised over BGP; VTEP address for the VXLAN underlay; BGP peer identifier |
| `vpc-dpu-lo` | One IP per (VPC, DPU) pair | Yes | Per-VPC VTEP used in the VPC overlay; allocated on demand when an instance is first placed on a DPU for a given VPC |

Define each pool with either a `prefix` or one or more `ranges`. Providing both, or neither, prevents the API server from starting.

```toml
[pools.lo-ip]
type = "ipv4"
prefix = "10.180.62.0/26"

[pools.vpc-dpu-lo]
type = "ipv4"
ranges = [
  { start = "10.180.63.1", end = "10.180.63.254" },
]
```

For pool semantics, runtime inspection (`admin-cli resource-pool list`), and the `grow` operation, see [IP Resource Pools](../manuals/networking/ip_resource_pools.md).

> **Note:** Tenant workload IPs (the addresses an instance sees on its NICs) are not managed through these pools.

### 1.2 Host OS IP Assignment

The host OS on each site controller node receives its primary IP from the parent datacenter (typically via DHCP). NICo does not assign or manage the site controller's host OS IP.

For each site controller node that participates in DPU mode, a single `/31` point-to-point subnet is allocated between the host OS and the DPU PF representor. These IPs are **statically assigned at OS install time** — not by NICo and not by the parent datacenter DHCP server. One `/31` per site controller node (with DPUs) is required; nodes without DPUs need only one host IP.

For managed hosts (ingested machines), the host OS IP comes from the **admin network** until the host is assigned to a tenant:

- The admin network is a NICo-managed pool. Allocations are made by `nico-api` and pushed to the host via DHCP.
- Size the admin network for at least one usable IP per managed server, plus network and broadcast addresses. Multiple admin segments may be declared in `[networks.<name>]`; each managed host sources its admin IP from whichever segment matches.
- When a tenant is assigned, the host's interfaces leave the admin network and join the relevant tenant networks (see [Network Prerequisites — Tenant Networks](../getting-started/prerequisites/network.md#tenant-networks)).

The admin network is defined in the `[networks.admin]` block of `siteConfig`:

```toml
[networks.admin]
prefix = "10.180.64.0/24"
gateway = "10.180.64.1"
mtu = 1500
```

> **Warning:** `[networks.admin]` `prefix` and `gateway` must be non-empty. `nico-api` panics at startup if either field is the empty string.

### 1.3 Expected Interface IP Allocation

Expected Machine declarations can allocate addresses for host OS, DPU OS, DPU
BMC, and host BMC interfaces. Every role supports three allocation policies:

| Policy | Behavior |
|---|---|
| **Dynamic** | At DHCP discovery, NICo allocates an address from the segment selected by the DHCP relay or DHCPv6 link address. |
| **Fixed** | NICo reserves the configured `fixed_ip` before DHCP. The address normally selects the managed segment whose prefix contains it; [legacy inferred reservations](expected-machine-interfaces.md#network-segment-selection) can fall back to `static-assignments`. |
| **Retained** | NICo allocates an address through DHCP, then keeps it static for the lifetime of the machine-interface record. |

All four interface roles (`host`, `dpu_os`, `dpu_bmc`, and `host_bmc`) support
all three policies. The default is Dynamic for every role except `host_bmc`,
which defaults to Retained. A declaration with `fixed_ip` and no explicit
policy remains Fixed for backward compatibility.

Mixing policies within the same site and Expected Machine is supported.
See [Configure Expected Machine Interfaces](expected-machine-interfaces.md)
for the complete field reference and examples.

#### Physical Management Networks

Every host BMC, DPU BMC, and DPU OOB interface needs an address on a physical
management network. Dynamic and Retained allocations, and new explicit Fixed
declarations, require a NICo-managed segment. Legacy inferred Fixed `host`
declarations, and inferred Fixed `host_bmc` declarations without a segment
guard, can instead use `static-assignments` for an address outside managed
prefixes. The usual deployment places these interfaces on an OOB network
represented by an `Underlay` segment. A host with no managed DPUs can instead
place its BMC and host OS on one shared `HostInband` subnet/VLAN; see
[Shared HostInband for a Host BMC and Host OS](#15-shared-hostinband-for-a-host-bmc-and-host-os).
That exception applies only to the host BMC. DPU BMC and DPU OOB interfaces
remain on `Underlay` segments.

#### Host BMC Compatibility Fields

The top-level Expected Machine fields `bmc_ip_address` and
`bmc_ip_allocation` control host-BMC allocation.

The `nico-admin-cli em add` and `em patch` flags accept policy values
`unspecified`, `auto`, `dynamic`, `fixed`, and `retained`. The
`bmc_ip_allocation` enum in the whole-table JSON consumed by `em replace-all`
uses `Unspecified`, `Auto`, `Dynamic`, `Fixed`, and `Retained`. Direct gRPC
clients use the corresponding `BMC_IP_ALLOCATION_TYPE_*` enum values.

The table describes a newly created entry. On patch/update, omitting the policy
preserves the stored effective policy rather than resetting it to Auto.
Explicit `unspecified`/`Unspecified` resets the policy to Auto.

| Expected Machine configuration | Effective policy | Behavior |
|---|---|---|
| Omit both fields, or select Auto without an address | **Retained** (default) | DHCP selects an address; Site Explorer makes it static for that machine-interface row's lifetime. |
| Set `bmc_ip_address`; omit `bmc_ip_allocation` or select Auto | **Fixed** | NICo reserves and serves the configured address. |
| Select Dynamic without an address | **Dynamic** | DHCP allocates a normal lease that can expire and change. |
| Select Retained without an address | **Retained** | Same retained behavior as the default. |
| Select Fixed with `bmc_ip_address` | **Fixed** | Same fixed behavior as inferred Auto. |

Fixed requires `bmc_ip_address`. Dynamic and Retained reject a configured
address. A retained address is not written back to the Expected Machine, so
deleting the machine-interface row and re-ingesting the host can select a new
address. Use a fixed address when it must survive interface recreation. Mixing
policies within one site is supported.

#### Capacity and Network Setup

**Per node**, expect to allocate:

- 1 IP for the host BMC.
- For hosts with DPUs: 1 IP for the DPU ARM OS + 1 IP for the DPU BMC, per DPU.

In the conventional topology, a host with one DPU therefore consumes three OOB
addresses. When a host with no managed DPUs shares HostInband, its one BMC
address is capacity on that HostInband segment rather than on a dedicated OOB
segment.

The conventional OOB management network is declared as one or more
NICo-managed `Underlay` network segments in `siteConfig`
`[networks.<name>]` blocks. Each segment carries its own prefix, gateway, and
MTU. The switches **must run a DHCP relay** pointed at the `nico-dhcp`
LoadBalancer VIP; they must not assign addresses themselves. See
[BMC and Out-of-Band Setup](../getting-started/prerequisites/bmc-oob-setup.md)
for switch-side relay configuration.

### 1.4 Configure Host BMC IP Allocation

For sites that require a preassigned host BMC IP, add a `host_bmc` entry whose
MAC matches the top-level `bmc_mac_address`. Admin CLI manifests use protobuf
enum number `2` for the Underlay segment type:

```json
{
  "expected_machines": [
    {
      "bmc_mac_address": "C4:5A:B1:C8:38:0D",
      "bmc_username": "root",
      "bmc_password": "<bmc-password>",
      "chassis_serial_number": "SERIAL-1",
      "interfaces": [
        {
          "mac_address": "C4:5A:B1:C8:38:0D",
          "role": "host_bmc",
          "ip_allocation": "fixed",
          "fixed_ip": "10.180.70.11",
          "network_segment_type": 2
        }
      ]
    }
  ]
}
```

For this fixed declaration:

- NICo records the fixed intent with the Expected Machine. The API update path
  and Site Explorer reconciliation materialize the machine-interface
  reservation; the DHCP path also restores it if the interface was deleted.
- The first DHCP DISCOVER from that BMC's MAC is answered with the reserved
  address; `nico-dhcp` does not draw another address for that host.
- For a BMC on a managed segment, the address must fall within that segment's
  prefix. It can be inside the segment's otherwise-dynamic pool: once the
  reservation exists, NICo's address-uniqueness constraint prevents the
  dynamic allocator from assigning it to another interface.
- The optional `underlay` segment guard rejects the configuration if the
  containing segment has another type.

Reconciliation can replace an existing DHCP or SLAAC address with the Fixed
reservation. It does not automatically replace one Fixed Static address with
another or return a Static address to Dynamic allocation. For those changes,
follow the targeted procedure in
[Retained Address Lifetime](expected-machine-interfaces.md#retained-address-lifetime).

The legacy top-level `bmc_ip_address` and `bmc_ip_allocation` fields remain
supported. They act as explicit overrides when a matching `host_bmc` entry is
also present. Existing manifests that only set `bmc_ip_address` continue to
work without conversion.

For the full `expected_machines.json` schema and upload command, see
[Ingesting Hosts](ingesting-hosts.md).

### 1.5 Shared HostInband for a Host BMC and Host OS

NICo supports connecting a host's BMC and host OS NIC to the same physical
subnet/VLAN when the host has no managed DPUs. This applies when the effective
DPU policy is `ignore` (no managed DPU) or `nic` (an installed DPU operates as
a plain NIC). Each interface receives its own address from one `HostInband`
segment.

Model the prefix **once**, as `HostInband`. Do not also declare an `Underlay`
segment for the same prefix. NICo enforces globally non-overlapping network
prefixes, so duplicate, nested, or partially overlapping segment definitions
are rejected. Machine-interface addresses are also globally unique.

The shared topology has these requirements:

- Configure the `HostInband` segment with `allocation_strategy = "dynamic"`
  (the default) whenever either interface needs an unreserved DHCP allocation.
  A `reserved` segment answers only clients whose fixed reservation already
  exists.
- Associate the segment with a valid DNS subdomain. Config-seeded network
  creation requires one selected initial forward domain and associates the
  segment with it automatically; startup skips segment creation if no forward
  domain can be selected unambiguously. A segment created at runtime with
  `nico-admin-cli network-segment create` requires `--subdomain-id`.
- Size the prefix for distinct host-BMC and host-OS addresses and set
  `reserve_first` high enough to protect gateway or infrastructure addresses.
- Declare every data-NIC MAC for a host with no managed DPUs in that host's
  Expected Machine `interfaces` array. Mark one data NIC `primary: true`; that
  NIC is the host's boot interface, and declarations with more than one primary
  are rejected. Setting
  `network_segment_type: "host_inband"` is recommended so DHCP fails on a
  segment-type mismatch instead of using a relay candidate of another type.
  See the [zero-DPU site setup](../manuals/vpc/flat_vpcs_zero_dpu.md#site-operations-operator).
- Register the BMC MAC and credentials at the top level of the Expected
  Machine, and use a matching `host_bmc` interface entry for allocation. Omit
  both `ip_allocation` and `fixed_ip` to use the default Retained behavior, or
  select Fixed and set `fixed_ip` to an address in the HostInband prefix when
  the address must survive interface deletion and re-ingestion. The compatible
  top-level `bmc_ip_address` and `bmc_ip_allocation` fields remain supported.
- Configure the BMC-facing and host-NIC-facing switch ports so DHCP reaches
  `nico-dhcp` with relay/link metadata that identifies the HostInband prefix.

For IPv4, NICo selects candidate segments whose prefix contains the relay's
`giaddr`; `giaddr` does not need to equal the configured gateway. For DHCPv6,
an exact configured link-address match is authoritative, with prefix
containment used as a fallback. VLAN ID, circuit ID, remote ID, and physical
cabling do not themselves select a NICo segment, although the switch
configuration determines which relay/link address NICo receives.

This is a host-BMC exception only. Do not place a DPU BMC or DPU OOB interface
on HostInband; those endpoints retain the conventional `Underlay` model.

<Warning>
Sharing the host-facing L2 network removes the network-level OOB isolation that
a dedicated management VLAN provides. The BMC still operates independently of
the host OS, but it can be reachable from the same physical network unless the
fabric supplies ACL, VRF, private-VLAN, or equivalent isolation. Treat that as
an explicit site security decision.
</Warning>

---

## 2. DHCP Configuration

### 2.1 How `nico-dhcp` Works

`nico-dhcp` is **not** a standalone DHCP daemon. It is a [Kea DHCP](https://www.isc.org/kea/) hooks library (`cdylib`) loaded into the upstream Kea v4 server inside the `nico-dhcp` container. Every DHCPDISCOVER/REQUEST is intercepted by the hooks library and forwarded to `nico-api` over mTLS gRPC (the `discover_dhcp` RPC). `nico-api` decides what address to lease based on:

- Whether the source MAC matches a Fixed Expected Machine interface
  reservation, including one declared through the compatible top-level
  `bmc_ip_address` field.
- Otherwise, whether the source MAC has a Dynamic or Retained Expected Machine
  policy or is a known host, host BMC, DPU BMC, or DPU OS interface.
  `nico-api` uses relay metadata to select the applicable network segment.
  Dynamic interfaces receive the next free address. Retained interfaces reuse
  their existing static address, or receive a new address when none exists.
- Vendor class (option 60) determines whether the client is a PXE/iPXE/BlueField boot client, which influences the boot options returned.

The hook callouts (`lease4_select` and `lease4_renew`) overwrite the lease that Kea would have selected — `yiaddr`, valid lifetime, and DHCP options are replaced with the values `nico-api` produced, and the hook can return `SKIP` to cancel Kea's own lease assignment and database write. The result is written to Kea's memfile (`kea-leases4.csv`), but the authoritative record lives in `nico-api`. From an operator perspective this means:

- The state-of-truth for every lease lives in `nico-api`'s database, not in Kea's lease file.
- There is no standalone DHCP configuration file to populate with
  reservations. Reservations come from `expected_machines.json`, and network
  segments come from `siteConfig`.
- If `nico-api` is unreachable, the hooks library serves cached negative responses (negative cache TTL: 5 minutes); this is a degraded-mode safety net, not a fallback pool.

### 2.2 DHCP Configuration for Physical Machine Interfaces

All physical interface types are served by the same `nico-dhcp` instance. For
IPv4, `nico-api` selects candidate segments by finding the configured prefix
that contains the relay's `giaddr`. The configured `gateway` is not the segment
selector. DHCPv6 uses an exact configured link-address match when present and
otherwise falls back to prefix containment.

| Interface | DHCP request originates on | Served from |
|---|---|---|
| Host BMC | Usually the OOB management network; optionally a shared host network when the host has no managed DPUs | The relay-selected `Underlay`, or the supported shared `HostInband` segment |
| DPU BMC | OOB management network | The relay-selected `Underlay` segment |
| DPU OOB (ARM OS) | OOB management network | The relay-selected `Underlay` segment; it can share the DPU BMC segment or use another Underlay prefix |
| Host OS NIC without managed DPUs | Host-facing physical network | The relay-selected `HostInband` segment |

Each `[networks.<name>]` block declares:

| Field | Purpose |
|---|---|
| `type` | Config-seeded segment classification: `admin`, `underlay`, or `hostinband`. Tenant segments are created through the API and are not config-declarable. Expected Interfaces can use the corresponding guard described in [Network Segment Selection](expected-machine-interfaces.md#network-segment-selection). |
| `prefix` | The IPv4 CIDR for the segment. |
| `gateway` | The IPv4 gateway associated with the prefix and returned in network configuration. It does not have to equal `giaddr`. |
| `mtu` | MTU advertised to clients on this segment. |
| `reserve_first` | Number of leading addresses in the prefix to hold back from the dynamic pool. A typical value is 5. |
| `allocation_strategy` | `dynamic` (default) permits pool allocation and Fixed reservations; `reserved` serves only reservations created before DHCP. Dynamic and Retained interfaces cannot acquire their first address on a reserved segment. |

A conventional DPU site declares an `admin` segment and one or more `underlay`
segments covering its OOB relay scopes. A site with hosts that have no managed
DPUs also declares the required `hostinband` segments. The host BMC can use one
of those HostInband segments as described in
[section 1.5](#15-shared-hostinband-for-a-host-bmc-and-host-os).

To configure these flows:

1. **Declare the physical network segments in `siteConfig`.** Use the schema
   above. Each relay/link address must fall within the intended segment prefix,
   except a separately configured DHCPv6 link-address can select its segment by
   exact match.
2. **Configure the DHCP relay on every applicable switch** to forward DHCP
   traffic to the `nico-dhcp` LoadBalancer VIP (the IP assigned to the
   `nico-dhcp` service by MetalLB in
   [Quick Start Step 3h](../getting-started/quick-start.md#3h-assign-service-vips)).
   The relay-facing interface must cover the L2 broadcast domain whose clients
   it serves. In the shared HostInband topology, both the host BMC and host OS
   paths must produce relay metadata for that HostInband prefix.
3. **For Fixed policies**, upload `expected_machines.json` with the applicable
   interface reservations before the device first powers on. NICo can replace
   an existing DHCP or SLAAC address with a Fixed reservation. If the interface
   already has a Static address that blocks the change, follow the targeted
   address update procedure in
   [Retained Address Lifetime](expected-machine-interfaces.md#retained-address-lifetime).
4. **For reservation-only segments**, set
   `allocation_strategy = "reserved"` and configure every Fixed reservation
   before DHCP. Dynamic and Retained declarations cannot acquire their first
   address when no reservation exists.
5. **Set `dhcp_servers`** in `siteConfig` to the list of DHCP server IPs
   reachable from bare-metal hosts. This list is informational and is passed
   through to agents; it does not change how `nico-dhcp` itself serves leases.
   May be left as `[]`.
6. **Set `ntp_servers`** in `siteConfig` to your NTP server IPs. NICo uses this
   list to configure BMC NTP through Redfish during pre-ingestion, includes it
   in `DiscoverDhcp` responses, and passes it to DPU agents so their DHCP server
   advertises the same NTP servers to managed hosts.

The values that `nico-dhcp` returns in DHCP options (nameservers, NTP servers, next-server, boot file, etc.) are sourced from:

- The `ntp_servers` list in `siteConfig` — preferred for DHCP option 42 when non-empty.
- The Kea hook parameters in the `nico-dhcp` Helm chart (`nico-nameserver`, `nico-ntpserver`, etc.) — set these to the `unbound.nico` (or `unbound.nico`, see [section 3](#3-dns-configuration)) recursive resolver VIP. `nico-ntpserver` is used only as a fallback when `siteConfig.ntp_servers` is empty.
- The per-segment definitions in `siteConfig` `[networks.<name>]` blocks — gateway, MTU, additional routes.

<Note>
NICo does not run a standalone NTP service. Point `siteConfig.ntp_servers` to your enterprise NTP servers. NICo also attempts to set those servers on host BMCs during pre-ingestion; if the Redfish operation fails repeatedly, pre-ingestion continues and the failure is logged.
</Note>

### 2.3 How to Verify DHCP Is Working

After deployment, validate the DHCP path end-to-end:

**Confirm the `nico-dhcp` service is reachable on its LoadBalancer VIP:**

```bash
kubectl get svc nico-dhcp -n nico-system
```

Both EXTERNAL-IP and TYPE=`LoadBalancer` must be populated. A `<pending>` IP indicates a MetalLB issue — see the [Reference Installation](../getting-started/installation-options/reference-install.md) guides for MetalLB troubleshooting.

**Tail `nico-dhcp` logs while a BMC powers on:**

```bash
kubectl logs -n nico-system -l app.kubernetes.io/name=nico-dhcp --tail=20 -f
```

Each DISCOVER should produce a log line showing the source MAC, the resolved segment, and either a leased address or a `discover_dhcp` gRPC error from `nico-api`. A `DeadlineExceeded` or `Unavailable` error means the hook cannot reach `nico-api`; check the `nico-api` LoadBalancer and TLS material.

**Inspect Kea's lease file** to confirm a lease was committed:

```bash
kubectl exec -n nico-system deploy/nico-dhcp -- \
    cat /var/lib/kea/kea-leases4.csv | head
```

The lease IP and MAC should match what `nico-api` allocated. The lease file is authoritative for Kea only — `nico-api` is the system of record.

**On the switch relaying the tested BMC or HostInband client**, verify packets
are being forwarded by checking its relay statistics (`show ip dhcp relay
statistics` on Cumulus / SONiC). DISCOVER packets sent should match OFFER
packets received.

For DHCP-related stuck states during ingestion, see the [WaitingForNetworkConfig playbook](../playbooks/stuck_objects/waiting_for_network_config.md).

---

## 3. DNS Configuration

NICo's DNS layer has two distinct pieces:

| Piece | Backed by | Serves |
|---|---|---|
| `nico-dns` | A standalone DNS server (the `carbide-dns` binary) that answers every query from `nico-api` record data (see [section 3.1](#31-nico-dns-zones-and-what-they-serve)) | The site's authoritative zones — generated from machine, instance, and tenant records in the `nico-api` database |
| `unbound` (recursive resolver) | Unbound | The resolver that managed machines (host BMCs, host OS, DPU OS, DPU BMCs) use for *all* DNS lookups |

These two roles are independent. Managed machines never query `nico-dns` directly — they query the recursive resolver, which forwards or recurses as needed.

### 3.1 `nico-dns` Zones and What They Serve

`nico-dns` serves the site's authoritative zones from `nico-api`'s database. It is a standalone DNS server: the binary listens on UDP and TCP 53 (`--listen`, default `[::]:53`) and answers each query by calling `nico-api` over gRPC for record data. It serves A, AAAA, and PTR records only, and it does not recurse; clients reach it through the recursive resolver ([section 3.2](#32-unbound-recursive-resolver-for-managed-machines)), not directly.

The zones served are seeded by the `initial_domain_name` field in `siteConfig` (for example, `mysite.example.com`). On first start, `nico-api` creates the corresponding domain record; `nico-dns` then exposes whatever records exist in that zone in `nico-api`'s database.

UFM endpoints under `default.ufm.<initial_domain_name>` are one example of records served this way when InfiniBand is configured (see [InfiniBand Setup](../playbooks/ib_runbook.md)).

Operators do not edit `nico-dns` zone files directly. Zone content is a function of `nico-api`'s database state.

For the record catalog these zones serve - machine, BMC, and instance names, plus the automatically derived reverse zones and their lifecycle - refer to [DNS](../configuration/dns.md).

To configure `nico-dns`:

1. Set `initial_domain_name` in `siteConfig` to your site's DNS domain.
2. Deploy the `nico-dns` StatefulSet; the binary listens on UDP/TCP 53 by default (`--listen=[::]:53`).
3. Assign a stable LoadBalancer VIP to the `nico-dns` service that listens on UDP/TCP 53 (one per replica via `perPodAnnotations`; see [Quick Start Step 3h](../getting-started/quick-start.md#3h-assign-service-vips)).
4. Delegate the `initial_domain_name` zone from your upstream DNS to those VIPs, or configure your recursive resolver to forward queries for the zone to them.

### 3.2 `unbound` Recursive Resolver for Managed Machines

Managed machines (host OS, DPU OS, host BMCs, DPU BMCs) need a recursive resolver that can resolve **both** the site-internal NICo service zone and external names. NICo deploys an `unbound` instance for this purpose.

The resolver address is distributed to managed machines via **DHCP option 6**, set in the `nico-dhcp` Kea hook parameter `carbide-nameservers` (the `config.kea.hookParameters.nameservers` Helm value emits it). Managed machines have no compiled-in resolver address — changing the resolver is a DHCP configuration change, not a rebuild.

The resolver is responsible for:

- Recursive resolution of external (public-internet) names — needed for package fetches, NTP, etc.
- Authoritative resolution of the NICo service zone (`.forge`, `.nico`, or whichever convention your deployment uses; see below).
- Forwarding to `nico-dns` for the site domain configured in `initial_domain_name`.

To configure `unbound`:

1. Populate the `local_data.conf` ConfigMap consumed by the `unbound` Helm chart with one A record per service VIP (see [section 3.3](#33-nico-dns-service-endpoints)).
2. Add a forward zone entry for `initial_domain_name` pointing at the `nico-dns` VIPs.
3. Allow public-internet recursion (the default for the upstream `unbound` image) unless your site is fully air-gapped.

The `unbound` pod auto-reloads when the ConfigMap changes.

### 3.3 `.nico` DNS Service Endpoints

A fixed set of NICo service hostnames are resolved by DPU agents, host PXE loaders, and other in-band management components at runtime. Several of these names are **compiled into binaries or embedded shell scripts** and cannot be overridden via config — DNS is the only way to redirect them.

Two TLD conventions exist:

- **`.forge`** is the compiled default in `crates/agent/src/util.rs` and the host PXE loader scripts. The agent resolves `carbide-pxe.forge`, `carbide-ntp.forge`, etc. at startup. This is the TLD used by deployments built from the current binaries.
- **`.nico`** is the rebranded TLD documented in [`deploy/DNS.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/deploy/DNS.md). New deployments may use this convention, but only if the agent and PXE images have been rebuilt with the new TLD.

Choose the convention that matches your binaries — do not mix. Verify by checking what the agent actually resolves at startup (`kubectl exec -n nico-system <agent-pod> -- getent hosts carbide-pxe.forge` or the `.nico` equivalent).

The required A records (shown for `.nico`; substitute `.nico` if your binaries use it) are:

| Hostname | Port | Resolves to | Purpose | Configurable at runtime? |
|---|---|---|---|---|
| `nico-api.nico` | 443 | `nico-api` external LoadBalancer VIP | NICo gRPC API | Yes — `NICO_API_URL` env var on most clients |
| `nico-pxe.nico` | 80 | `nico-pxe` LoadBalancer VIP | iPXE scripts, cloud-init, internal APT, and the legacy bootstrap-CA endpoint | The DNS record remains fixed for general consumers. DPF can separately configure bootstrap CA acquisition through a complete URL override or mounted Secret or ConfigMap. Non-DPF boot instructions include `pxe_uri`. Other consumers retain the compatibility hostname. |
| `nico-static-pxe.nico` | 80 | Static PXE asset server VIP | `scout.squashfs`, `scout.efi`, BFB images, and other static boot artifacts | **No** — hardcoded in the host boot scripts that ship inside boot images |
| `nico-ntp.nico` | 123 | Operator-supplied NTP server IP(s) — the record points at your existing NTP infrastructure, not a NICo-deployed service | Legacy NTP fallback for DPU agents when `siteConfig.ntp_servers` is empty | **Fallback only** — prefer `siteConfig.ntp_servers`, but keep this DNS record if any deployed agent still relies on it |
| `unbound.nico` | 53 | `unbound` LoadBalancer VIP | Recursive DNS resolver | Yes — the resolver address itself is distributed via DHCP option 6 |
| `otel-receiver.nico` | 443 | OTel receiver VIP on the site controller | OTLP ingestion endpoint for DPU otel-collector sidecars | Yes — set in the otel-collector configuration YAML and re-deployed |

One additional `.nico` hostname, `socks.nico`, is hardcoded into the DPU agent as the SOCKS5 outbound proxy for DPU extension-service pods. Add a corresponding A record only if your environment runs a SOCKS5 proxy for that purpose; it is not part of every NICo deployment. For per-endpoint detail (consumers, in-cluster addresses, hardcode locations, and the `unbound`-vs-other-resolver guidance), see [`deploy/DNS.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/deploy/DNS.md). That file is the canonical endpoint reference; the table above is the operator-facing summary.

> **Note:** The `.nico` service zone is private and is not a publicly
> registered TLD. It can be served on the conventional isolated OOB network or
> on a supported shared HostInband network. Configure the recursive resolver to
> treat it as locally authoritative and **not** forward it to upstream public
> resolvers.

#### Bootstrap CA Selection and Network Trust

If you do not configure a bootstrap certificate authority (CA), DPUs retain the
historical behavior and download
`http://<nico-pxe>/api/v0/tls/root_ca`. You can make the bytes served by that
legacy endpoint independent of PXE's own outbound API trust. Reference an
existing ConfigMap or Secret in the `nico-pxe` Helm release namespace:

```yaml
nico-pxe:
  bootstrapRootCa:
    configMapName: forge-root-ca # Set secretName instead for a Secret.
    key: ca.crt
```

Set either `configMapName` or `secretName`, but not both. If you set neither,
the chart preserves the old PXE deployment and payload path. This option can
serve a stable root instead of a rotating site intermediate. It does not
authenticate a CA fetched over DHCP-directed, DNS-resolved HTTP. Existing DPUs
retain the CA they already installed. A payload change affects only later
downloads unless you reprovision or refresh the DPU through another trusted
mechanism.

Outside Helm, set `FORGE_BOOTSTRAP_ROOT_CAFILE_PATH` to the PEM bundle path in
the `nico-pxe` container. If you do not set it, `nico-pxe` serves the file named
by `FORGE_ROOT_CAFILE_PATH`, preserving the historical bundle.

Non-DPF deployments can set `[dpu_config].bootstrap_ca_source` to `embedded`
or `mounted`. That setting applies only to DPU provisioning, not host Scout
boots. Embedded mode requires a site-specific BFB build with an explicit
`BOOTSTRAP_CA_PATH`. There is no default fallback for the dedicated embedded
payload. Existing legacy artifact inputs remain unchanged. Mounted mode
consumes the operator-populated final `/opt/forge/forge_root.pem` path instead
of the distinct embedded `/opt/forge/embedded_forge_root.pem` source. DPF
deployments use
`[dpf.dpu_agent_bootstrap_ca]` and support the legacy download or a mounted
Secret or ConfigMap. In legacy mode, `url` can replace the complete default
endpoint with an HTTP or HTTPS URL. The
[DPU Agent Bootstrap CA](../manuals/dpf.md#dpu-agent-bootstrap-ca) section
provides full URL and mounted-object examples. The shared published DPU agent
image does not embed a site CA. Non-network modes require a valid local bundle
and fail closed without falling back to the download. Upgrade code, images, and
boot artifacts before enabling them. Reprovision non-DPF DPUs. For DPF changes,
restart `carbide-api` and roll the DPU agent pods.

Before pinning a root, inspect the NICo API certificate chain with the same DNS
name the DPU uses:

```bash
export NICO_API_HOST='<nico-api-hostname-used-by-dpu>'
openssl s_client -connect "${NICO_API_HOST}:443" \
  -servername "${NICO_API_HOST}" -verify_hostname "${NICO_API_HOST}" \
  -showcerts -verify_return_error \
  -CAfile /path/to/site-bootstrap-roots.pem </dev/null
```

Confirm the server sends the issuing intermediate certificate as well as the
leaf. The bundle validates the server certificate independently of whether
client-certificate authentication is enabled. If each replacement intermediate
chains to the pinned root and the server presents the complete chain, clients
can validate leaf certificates across those rotations without replacing the
bundle. If an intermediate chains to a different root, stage and verify an
updated root bundle before rotating the server chain. TLS server authentication
does not authenticate the broader DHCP, DNS, iPXE, and user-data boot chain.
Embedded deployments must separately protect artifact integrity and enforce
Secure Boot or an equivalent trusted boot chain.

### 3.4 How to Verify DNS Is Working

**From the site controller**, confirm `nico-dns` is responding:

```bash
kubectl get svc nico-dns -n nico-system
dig +short @<nico-dns-vip> <initial_domain_name>
```

**From a managed physical-network vantage point** (a host BMC console, a
managed host's BMC web UI shell, or another client on its network), confirm the
service zone resolves:

```bash
for name in nico-api.nico nico-pxe.nico nico-static-pxe.nico \
            nico-ntp.nico unbound.nico otel-receiver.nico; do
    printf "%-30s -> %s\n" "$name" "$(dig +short "$name" @<UNBOUND_VIP> || echo 'FAILED')"
done
```

Substitute `.nico` if that is the TLD baked into your binaries. Every name must return a non-empty A record set; a `FAILED` or empty result means the `local_data.conf` ConfigMap is missing that record. If your environment also runs a SOCKS5 proxy, extend the loop with `socks.nico`.

**Confirm reachability on the expected ports:**

```bash
# nico-api gRPC (TLS handshake)
openssl s_client -connect nico-api.nico:443 </dev/null 2>/dev/null | grep -E '^(subject|Verify)'

# nico-pxe
curl -sf --max-time 5 http://nico-pxe.nico/ -o /dev/null && echo OK || echo FAILED

# unbound recursing externally
dig +short +timeout=3 example.com @unbound.nico
```

A successful external recursion via `unbound.nico` confirms both DHCP option 6 (clients learn the resolver) and `unbound`'s recursion policy are correct.

---

## 4. End-to-end Day 0 Checklist

Use this checklist across the Day 0 rollout. Complete the configuration and
infrastructure prerequisites before powering on the first host BMC. Complete
the runtime checks as the first managed endpoints come online and before
expanding the rollout to the rest of the fleet:

- [ ] `siteConfig` `[pools.lo-ip]` and `[pools.vpc-dpu-lo]` populated with non-empty ranges.
- [ ] `siteConfig` `[networks.admin]` has non-empty `prefix` and `gateway`.
- [ ] Each required physical segment declared in `[networks.<name>]`:
      `underlay` capacity for every DPU BMC/OOB pair and isolated host BMC, and
      `hostinband` capacity for every shared host BMC and host OS NIC on a host
      without managed DPUs.
- [ ] `initial_domain_name` set in `siteConfig`.
- [ ] `dhcp_servers` set in `siteConfig` (or left as `[]`).
- [ ] `ntp_servers` set in `siteConfig`, or the legacy `nico-ntp` DNS / `nico-dhcp` `nico-ntpserver` fallback is intentionally configured.
- [ ] `expected_machines.json` uploaded for every host; a Fixed `host_bmc`
      interface or compatible top-level `bmc_ip_address` configured for any
      host that needs a predefined BMC IP.
- [ ] Every BMC-facing network, and every host-facing network for a host without
      managed DPUs, configured with a DHCP relay pointing to the `nico-dhcp`
      LoadBalancer VIP.
- [ ] LoadBalancer VIPs assigned for `nico-api`, `nico-dhcp`, `nico-pxe`, `nico-dns` (one per replica), `nico-ssh-console-rs`, and `unbound`.
- [ ] `unbound`'s `local_data.conf` ConfigMap contains A records for `nico-api`, `nico-pxe`, `nico-static-pxe`, `unbound`, and `otel-receiver` in the `.nico` zone; include `nico-ntp` pointing at your operator-supplied NTP server if you use the legacy fallback.
- [ ] `nico-dns` zone for `initial_domain_name` is delegated from upstream DNS, or `unbound` forwards the zone to the `nico-dns` VIPs.
- [ ] `unbound.nico` resolves every NICo service hostname (verified with the `dig` loop in [section 3.4](#34-how-to-verify-dns-is-working)).
- [ ] Select the bootstrap-CA mode intentionally.
- [ ] For non-DPF `embedded`, deploy site-specific artifacts built with `BOOTSTRAP_CA_PATH`, verify their integrity, and enforce Secure Boot or an equivalent trusted boot chain.
- [ ] For non-DPF `mounted`, verify that provisioning installs a valid bundle at `/opt/forge/forge_root.pem` on every DPU and that each DPU authenticates the NICo API without TLS errors.
- [ ] For DPF `mounted`, verify that the selected Secret or ConfigMap and key exist in the `dpu-agent` workload namespace of every target DPU cluster. After the pods start, verify that every DPU agent pod installs `/opt/forge/forge_root.pem` and authenticates the NICo API without TLS errors.
- [ ] When pinning a root, the NICo API TLS handshake includes the issuing intermediate certificate.
- [ ] `nico-dhcp` logs show DISCOVER → OFFER for a test BMC power-on.

When every item is checked, proceed to [Ingesting Hosts](ingesting-hosts.md).

---

## Related Pages

- [Network Prerequisites](../getting-started/prerequisites/network.md) — VNI/ASN/IPv4 sizing, BGP/EVPN, route targets, switch configuration.
- [BMC and Out-of-Band Setup](../getting-started/prerequisites/bmc-oob-setup.md) — physical management networks, DHCP relay setup, BMC credentials.
- [IP Resource Pools](../manuals/networking/ip_resource_pools.md) — `lo-ip` / `vpc-dpu-lo` semantics, sizing, `admin-cli resource-pool grow`.
- [Quick Start Guide](../getting-started/quick-start.md) — the install flow that consumes the configuration described here.
- [Reference Installation](../getting-started/installation-options/reference-install.md) — pointers to the manual, manifest-level install and troubleshooting references.
- [Ingesting Hosts](ingesting-hosts.md) — `expected_machines.json` schema and upload commands.
- [`deploy/DNS.md`](https://github.com/dsx-ai-factory/infra-controller/blob/main/deploy/DNS.md) — canonical reference for NICo service hostnames, ports, and hardcoded-vs-configurable status.
