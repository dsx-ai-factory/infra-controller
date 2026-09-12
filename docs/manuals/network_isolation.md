# Network Isolation

NICo enforces tenant network isolation across three independent fabrics. Each
fabric uses a different mechanism, is configured through a different operator
API, and is verified separately. This page summarises the model so an operator
can choose the right guide; it is not a replacement for the per-fabric
configuration guides linked below.

| Fabric | Operator-facing primitive | Isolation enforced by |
|---|---|---|
| Ethernet | VPC + VpcPrefix (+ optional Network Security Group) | DPU VRF per VPC (HBN / NVUE) over a pure type-5 EVPN overlay |
| InfiniBand | InfiniBand partition | UFM P_Key partition membership; `IbFabricMonitor` reconciler |
| NVLink | NVLink logical partition | NMX-C partition lifecycle; `NvlPartitionMonitor` reconciler |

---

## Who configures what, and how

Network operations split across two roles. The per-fabric guides tag every
operation with its role and interface using the model below; read this once,
then use the operations matrix in each guide.

**Operator** (site administrator)

- Day-0 site setup is **TOML** in the API server configuration. After Day 0,
  TOML changes are rare.
- For Day-1+ operations, prefer the **REST API**, or **`nicocli`** (its CLI
  wrapper).
- Use **`nico-admin-cli`** (which speaks the gRPC API directly) only for
  operations the REST API does not expose — for example, NMX-C endpoint
  registration or break-glass fabric cleanup.

**Tenant**

- Never edits TOML.
- Uses the **REST API** or **`nicocli`** exclusively.
- If neither exposes a required operation, that is a gap: **file a bug**
  against the REST API / `nicocli` rather than reaching for `nico-admin-cli`.

REST paths in the matrices are shown against the `/v2/org/{org}/nico/...`
placeholder; `nicocli` commands follow the `nicocli <resource> <verb>` form.
See the **REST API Reference** tab and the
[nicocli Reference](nicocli-reference.md) for exact request bodies and flags.

---

## Ethernet

**Operations**

| Task | Role | Interface |
|---|---|---|
| IP / VNI / ASN pools, `datacenter_asn`, routing profiles, `deny_prefixes`, admin network | Operator | **TOML** — Day 0 / rare. See the VPC manuals below. |
| Create / update / delete a VPC | Tenant | **REST** `…/nico/vpc` · `nicocli vpc create` |
| Create / update / delete a VpcPrefix | Tenant | **REST** `…/nico/vpc-prefix` · `nicocli vpc-prefix create` |
| Establish VPC peering | Tenant | **REST** `…/nico/vpc-peering` · `nicocli vpc-peering create` |
| Attach a Network Security Group to a VPC or instance | Tenant | **REST** (`update-vpc` / `update-instance`) · `nicocli` |

See [Who configures what, and how](#who-configures-what-and-how) for the role
and interface model.

A tenant's instance reaches a VPC by drawing addresses from one of the
**VpcPrefixes** attached to that VPC. NICo carves a /31 link-net per
interface from the prefix — one address to the instance, one to the
DPU's SVI in the VPC's VRF. An instance may participate in several VPCs
at once by having interfaces drawing from prefixes in different VPCs.
On the DPU of the managed host backing the instance, each related VPC
materialises as a Linux VRF; every host interface drawing from a prefix
in that VPC lives in that VRF. The tenant overlay is a pure type-5 EVPN
(IP-prefix) overlay — NICo does not stretch any tenant L2 segment across
the fabric.

Ethernet isolation has three independent layers:

- **Routing isolation (VPC / VRF).** VRFs are isolated by default. A route
  advertised in one VPC does not appear in another VPC's VRF. Cross-VPC
  reachability is opt-in via VPC peering or controlled route leaking on the
  VPC's routing profile.
- **Default isolation (admin overlay).** A managed host never carries tenant
  traffic unless a tenant configuration places it in a VPC. Between tenants,
  during provisioning / termination, or when its configuration is unknown,
  the DPU is held on the admin overlay (fail-closed).
- **L3 / L4 filtering (Network Security Groups).** Stateful or stateless
  rule-based filtering within or across VPCs, attached at VPC or instance
  scope.

The Ethernet configuration model is documented in the VPC manuals; this
overview does not duplicate them:

- [VPC Network Virtualization](vpc/vpc_network_virtualization.md) — the full
  VXLAN / EVPN, VRF, BGP, routing-profile, and DPU-config reference, including
  [Default Isolation: The Admin Overlay](vpc/vpc_network_virtualization.md#default-isolation-the-admin-overlay)
- [VPC Routing Profiles](vpc/vpc_routing_profiles.md) — route-target imports /
  exports and controlled route leaking
- [VPC Peering](vpc/vpc_peering_management.md) — opt-in cross-VPC reachability
- [Network Security Groups](networking/network_security_groups.md) — L3 / L4
  rule filtering and site-wide operator overrides
- [Flat VPCs and Zero-DPU Hosts](vpc/flat_vpcs_zero_dpu.md) — the
  operator-managed data plane for hosts without a NICo-managed DPU

---

## InfiniBand

Each tenant InfiniBand partition maps to a UFM P_Key. Membership is enforced
by the subnet manager at the fabric level: hosts that are not members of a
P_Key cannot exchange traffic with other members of that P_Key, regardless
of physical connectivity. NICo reconciles desired partition membership against
UFM via the `IbFabricMonitor` background task and surfaces the synchronisation
status to operators and to tenants.

See [Configuring InfiniBand Partitions](networking/infiniband_partitioning.md)
for the operator configuration guide, and the
[InfiniBand Setup Runbook](../playbooks/ib_runbook.md) for the prerequisite
UFM / OpenSM hardening.

---

## NVLink

NVLink logical partitions express how GPUs should be grouped across hosts.
Within each physical NVLink domain, NICo places the requested GPUs into an
NMX-C partition and reconciles that partition through the NMX-C gRPC API.
Multiple tenants' partitions can coexist in one physical domain, while GPUs in
different partitions remain isolated from each other.

See [NVLink Partitioning](nvlink_partitioning.md) for the operator
configuration guide.

---

## Cross-cutting behaviour

The following invariants apply to every fabric.

- **Per-fabric synchronisation status.** Each instance's `InstanceStatus`
  exposes a per-fabric `configs_synced` field that is `true` only when the
  observed fabric state matches the desired configuration. The aggregate
  `configs_synced` field is the logical AND of all per-fabric fields and gates
  the instance's `Ready` state.
- **Provisioning blocks on isolation convergence.** During initial
  provisioning, the instance state machine waits until every requested fabric
  has applied the desired configuration before the instance is marked `Ready`.
  Tenants observe this as the `Configuring` tenant state, and the machine
  remains in `WaitingForNetworkConfig` until the DPU reports back.
- **Termination blocks on isolation convergence.** During termination, the
  state machine waits until every fabric reports that the host has been
  removed from all tenant partitions before the instance is reported as
  deleted. This guarantees a terminated instance cannot continue to exchange
  traffic on any fabric.
- **Force-delete still tears down fabric state.** Force-deleting a managed
  host explicitly detaches it from every fabric through the same external
  APIs the normal lifecycle uses, so external fabric managers do not retain
  stale tenant references.
- **External fabric reachability is monitored.** Each external fabric service
  (UFM and NMX-C) is monitored from NICo with request-success and latency
  metrics so that fabric-side outages can be distinguished from NICo-side
  configuration errors.

For the architectural rationale and the patterns shared across all three
fabrics, see
[Networking Integrations](../architecture/networking_integrations.md).

For the Day 0 IP, DHCP, DNS, and admin-network configuration that every
isolation guarantee on this page rests on, see
[IP and Network Configuration](../provisioning/ip-and-network-configuration.md).
