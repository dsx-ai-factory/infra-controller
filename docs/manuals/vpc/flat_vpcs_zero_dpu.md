# Flat VPCs and Zero-DPU Hosts <Badge intent="info">v2.0</Badge>

`Flat` is a VPC virtualization type for tenant instances that run on hosts
**without a NICo-managed DPU** — either hosts with no DPU hardware at all, or
hosts whose BlueField DPU is operated as a plain NIC. On these hosts NICo has no
DPU agent through which to build a VXLAN/EVPN overlay, so a Flat VPC's tenant
instances attach **directly to operator-defined underlay segments**
(`HostInband` network segments) instead of to a NICo-managed overlay.

A Flat VPC is still a real tenant VPC: it has an owner, a VNI, and can carry a
Network Security Group. What it does **not** have is a NICo-driven data plane.
NICo allocates the addresses and records the VPC's bookkeeping (VNI, NSG,
peering relationships), but routing and L3/L4 enforcement between a Flat VPC and
any other network is the **network operator's responsibility**, configured on
the physical/SDN fabric — not by NICo. This is the central difference from the
`EthernetVirtualizer` (ETV) and FNN virtualization types, where NICo programs a
per-VPC VRF on each DPU.

This page describes how an operator prepares a site for zero-DPU hosts and Flat
VPCs, and how a tenant then creates Flat VPCs and instances on them.

**Related pages**

- [Network Isolation](../network_isolation.md) — the per-fabric isolation model
  and the operator/tenant role split this page follows
- [VPC Network Virtualization](vpc_network_virtualization.md) — the DPU-overlay
  (ETV / FNN) model that Flat VPCs deliberately do **not** use
- [Network Security Groups](../networking/network_security_groups.md) — the L3/L4
  rule filter that a Flat VPC can still carry
- [VPC Peering](vpc_peering_management.md) — Flat VPCs may peer with ETV, FNN,
  and other Flat VPCs

---

## Where Flat VPCs Sit in the Stack

For ETV and FNN VPCs, the host's primary data path is its DPU, and NICo drives
the overlay: it places each host interface into a per-VPC VRF, programs BGP EVPN,
and confirms convergence through the DPU agent before an instance is `Ready`.
See [VPC Network Virtualization](vpc_network_virtualization.md).

A Flat VPC inverts that model:

| | ETV / FNN VPC | Flat VPC |
|---|---|---|
| Host fabric attachment | DPU-managed | Plain NIC, directly on the operator's segment |
| Data plane owner | NICo (per-VPC VRF on the DPU) | **The network operator** (physical / SDN fabric) |
| Tenant interface binding | `/31` link-net carved from a **VpcPrefix** | Directly onto a **`HostInband`** segment |
| Tenant chooses subnets / prefixes | Yes (VpcPrefix selection) | **No** — interfaces are auto-resolved |
| Routing profiles | FNN only | Not supported |
| VNI | Yes | Yes (exposed to peers for operator SDN use) |
| Network Security Group | Yes | Yes (object stored; enforcement is the operator's) |
| Convergence reported via | DPU config-sync feedback | NICo's own resolution (no DPU in the path) |

Because there is no NICo-managed data plane, a Flat VPC's reachability —
including isolation *between* a Flat VPC and other VPCs — must be arranged on the
operator's network. NICo will not, and cannot, enforce it through a DPU VRF.

---

## Operations: Who Does What

Setting up zero-DPU hosts and the `HostInband` segments they attach to is an
**operator** responsibility. Creating Flat VPCs and instances on them is a
**tenant** responsibility. The roles and interfaces follow the standard model in
[Network Isolation → Who configures what, and how](../network_isolation.md#who-configures-what-and-how).

REST paths below are shown against the `/v2/org/{org}/nico/...` placeholder and
abbreviated as `…/nico/...` thereafter.

| Task | Role | Interface |
|---|---|---|
| Set hosts to `nic` or `ignore` through the site-wide `dpu_policy` | Operator | **TOML** — Day 0 / rare; API restart |
| Set one host's DPU policy with `--dpu-policy` | Operator | **`nico-admin-cli`** (`em add` / `em patch`) — updating the declaration needs no API restart; applying a changed policy to an already-ingested host requires [force-delete and re-ingestion](../../provisioning/boot-interfaces-and-dpu-modes.md#35-flipping-a-dpu-to-nic-mode) |
| Declare `HostInband` underlay segments | Operator | **TOML** (`[networks.<name>]`, `type = "hostinband"`) — Day 0 |
| Create an additional `HostInband` segment after initial setup | Operator | **Restart-applied TOML** (`[networks]`), or runtime **`nico-admin-cli`** (`network-segment create`) — see [Configuring HostInband Segments](#2-configuring-hostinband-network-segments) |
| Inspect / delete a `HostInband` segment | Operator | **`nico-admin-cli`** (`network-segment show` / `delete`) |
| Create an instance type and associate zero-DPU machines | Operator | **REST** `…/nico/instance-type` · `nicocli` |
| Bind a `HostInband` segment to a Flat VPC | Tenant *(VPC owner)* | Set the VPC on the segment (see below) |
| Create / delete a Flat VPC | Tenant | **REST** `…/nico/vpc` (`networkVirtualizationType: "FLAT"`) |
| Create an instance on a Flat VPC | Tenant | **REST** `…/nico/instance` (`autoNetwork: true`) |
| Check instance status | Tenant | **REST** `GET …/nico/instance/{id}` · `nicocli instance get` |

> **`nicocli` coverage gap (file a bug).** As of this writing the `nicocli`
> REST wrapper does not expose two Flat-specific operations that the REST API
> *does* support: selecting `FLAT` when creating/updating a VPC's virtualization
> type, and setting `autoNetwork` when creating an instance. Until the wrapper
> catches up, tenants perform those two steps against the **REST API directly**.
> The remaining tenant operations (status, delete, NSG attach) work through
> `nicocli` normally. Per the tenant operating model, the missing wrapper
> commands should be filed as a bug against `nicocli` rather than worked around
> with `nico-admin-cli`.

---

## Site Operations (Operator)

A site is ready to host Flat VPCs once three things are true: the relevant hosts
are running without a NICo-managed DPU, the underlay segments those hosts sit on
are declared as `HostInband` segments, and (optionally) instance types exist so
tenants can request the right machines.

### 1. Set the host DPU policy

Whether NICo treats a host as a "zero-DPU" host is decided by its **DPU policy**,
which has three values:

| `dpu_policy` value | Meaning |
|---|---|
| `manage` | *(site/effective default)* The DPU is managed by NICo: BFB/firmware upgrades, HBN deployment, DPU agent, and the tenant overlay all apply. A per-host `manage` declaration inherits the site policy rather than overriding it. |
| `nic` | A DPU is physically present but should be operated as a plain NIC. NICo skips DPU provisioning and overlay management for the host. |
| `ignore` | NICo does not configure or attach DPU hardware. Use this for a host without DPUs or when installed DPUs should be intentionally ignored. |

Both `nic` and `ignore` make the host a **zero-DPU host** for the purposes
of Flat VPCs: NICo does not manage an overlay for it, and its only valid tenant
attachments are `HostInband` segments.

Set the policy in either of two places. Per-host `nic` and `ignore`
override the site-wide setting; per-host `manage` or an omitted per-host value
instead defers to the site-wide setting for backward compatibility:

- **Site-wide**, in the API server configuration:

  ```toml
  [site_explorer]
  dpu_policy = "nic"   # or "ignore"; omit entirely for the default "manage"
  ```

- **Per host**, on the host's `ExpectedMachine` entry, via the admin CLI's
  `--dpu-policy` option or the `dpu_policy` key in admin JSON. Per-host
  `nic` and `ignore` override the site-wide setting; for backward
  compatibility, per-host `manage` or an omitted policy defers to the site-wide
  value, which defaults to `manage`.

Existing configuration and admin JSON input remain readable: the previous
`dpu_policy` value `use_as_nic` remains accepted, as do the legacy `dpu_mode`
key and values `dpu_mode`, `nic_mode`, and `no_dpu`, which map to `manage`,
`nic`, and `ignore`, respectively. The admin CLI likewise accepts the previous
`use-as-nic` value and `--dpu-mode dpu-mode|nic-mode|no-dpu`; use
`--dpu-policy manage|nic|ignore` for new automation.

The Forge RPC intentionally retains `ExpectedMachine.dpu_mode` (field 16,
`DpuMode`) as its stable compatibility surface. Direct gRPC clients continue to
send `DPU_MODE`, `NIC_MODE`, or `NO_DPU`; NICo translates those values at the
RPC boundary to the internal `manage`, `nic`, and `ignore` policies.
`HostDpuPolicy` and `dpu_policy` are model, configuration, admin-CLI, and admin
JSON vocabulary rather than Forge protobuf symbols. Responses translate
non-default policies back through `dpu_mode`; the default `manage` policy might
leave that field unset.

Two related Day-0 settings matter for zero-DPU sites:

- **`[site_explorer] admin_segment_type_non_dpu`** (default `false`). When
  `true`, non-DPU hosts use the `HostInband` admin segment type instead of the
  regular `Admin` segment type for their admin-network attachment.
- **`rack_management_enabled`** (top-level, default `false`). This is the
  standalone / air-gapped rack-manager mode for GB200/GB300/VR144 deployments.
  It is not a DPU-policy override: rack-manager deployments that run DPUs as
  NICs must also set `[site_explorer] dpu_policy = "nic"` (or set that
  policy per host). The resulting `nic` policy produces zero-DPU hosts;
  the rack-management flag alone does not. Enable the flag only when running
  NICo with Rack Manager for those platforms.

Because a zero-DPU host has no DPU to DHCP and identify host NICs for it, the
host's data-NIC **MAC addresses must be registered** on its `ExpectedMachine`
entry, and the site DHCP service serves addresses only to known MACs. At most
one NIC per host is marked the **primary** (boot) interface. NICo records each
registered host NIC as an interface bound to a `HostInband` segment, which is
what the tenant's instance later attaches to.

Site-wide TOML changes are Day-0 (or rare, restart-applied) changes. Per-host
Expected Machine declarations can be updated through the admin CLI without an
API restart; applying a changed policy to an already-ingested host requires
force-delete and re-ingestion.

### 2. Configuring HostInband network segments

A `HostInband` segment is the underlay network a zero-DPU host's NIC physically
lives on. Unlike tenant overlay segments, a `HostInband` segment:

- is **not** an overlay network — it describes a real underlay subnet;
- **exists before any VPC** and may stay unassociated with a VPC indefinitely
  (it carries no VPC until a tenant binds one — see [the tenant side](#2-find-the-hostinband-segment-backing-the-vpc));
- is allocated a VLAN ID and VNI from the shared Ethernet resource pools, the
  same as Admin and Tenant segments;
- is **exempt** from site-fabric-prefix validation (it lives on the underlay,
  not the site fabric).

**Day 0 — declare segments in TOML.** Add one `[networks.<name>]` block per
`HostInband` segment to the API server configuration. Names are free-form but
must be unique:

```toml
[networks.rack09-inband]
type = "hostinband"          # selects the HostInband segment type
prefix = "10.40.9.0/24"      # CIDR of the underlay subnet
gateway = "10.40.9.1"        # usually the first usable address
mtu = 1500
reserve_first = 2            # addresses to skip before allocating
# allocation_strategy = "dynamic"   # or "reserved" for reservation-only DHCP
```

<Note>
A zero-DPU host's BMC and host OS NIC may share this physical subnet/VLAN. Give
the two interfaces distinct addresses from **one** `HostInband` segment; do not
declare an overlapping `Underlay` segment for the same prefix.
`bmc_ip_address` is optional: the default allocation retains the BMC's first
DHCP address for the machine-interface row's lifetime. This is also supported
when `dpu_policy = "nic"`, but it does not place the DPU BMC or DPU OOB
interface on HostInband. See
[Shared HostInband for a Host BMC and Host OS](../../provisioning/ip-and-network-configuration.md#15-shared-hostinband-for-a-host-bmc-and-host-os)
for relay selection, DNS-subdomain, allocation, and isolation requirements.
</Note>

The same `[networks.<name>]` mechanism is used for `admin` and `underlay`
segments; `hostinband` is the third config-declarable type. (Tenant segments are
**not** config-declarable; they are created only through the API.) Declared
segments are created when the API server starts.

**Day 1+ — add more segments.** Additional `HostInband` segments can be added
after Day 0 in either of two ways:

- **Config TOML (same mechanism as Day 0).** Add another `[networks.<name>]`
  block — in exactly the form above — and restart the API server. The startup
  network reconciliation is additive and idempotent: it creates any configured
  segment whose name does not yet exist and leaves existing ones untouched, so a
  block added later is created on the next restart. This is the simplest path
  when a restart is acceptable, and it keeps the config the single source of
  truth for declared segments.
- **Segment-creation API (no restart).** Create the segment at runtime through
  the `CreateNetworkSegment` API.

Note the current CLI surface for the runtime path:

- `nico-admin-cli --cloud-unsafe-op=<username> network-segment create` creates a
  segment at runtime.

  The global `--cloud-unsafe-op` acknowledgment is required and must not be used
  against a production site.

  For a `HostInband` segment, pass `--segment-type host-inband` with `--name`,
  `--prefix`, and `--subdomain-id`; add `--gateway` when the IPv4 prefix has a
  gateway.

  Run `nico-admin-cli network-segment create --help` for the full flag set.
- `nico-admin-cli network-segment show` lists segments. To remove one, use
  `nico-admin-cli --cloud-unsafe-op=<username> network-segment delete`; the same
  global acknowledgment is required and must not be used against production.

<Tip>
The REST API and `nicocli` do not expose operator network-segment management. The REST `/subnet` endpoints are the tenant subnet surface, not operator `HostInband` segments.
</Tip>

Deleting a `HostInband` segment follows the standard segment lifecycle: the
segment is drained (it is not removed while any host interface or instance
address still references it, plus a grace period) before its VLAN ID and VNI are
released and the row is deleted.

### 3. Configure instance types for zero-DPU machines

An **instance type** is an operator-owned object that describes a set of desired
machine capabilities; allocation uses it to filter for an available machine that
matches. Instance types are not Day-0 TOML — they are created after bootstrap.

Per the operator operating model, manage instance types through the **REST API**
or `nicocli` (its wrapper), which expose the full surface:

| Task | REST |
|---|---|
| Create an instance type | `POST …/nico/instance-type` |
| List / get instance types | `GET …/nico/instance-type` · `GET …/nico/instance-type/{id}` |
| Update / delete an instance type | `PATCH` / `DELETE …/nico/instance-type/{id}` |
| Associate a machine with an instance type | `POST …/nico/instance/type/{instanceTypeId}/machine` |
| Disassociate a machine | `DELETE …/nico/instance/type/{instanceTypeId}/machine/{id}` |

For a zero-DPU site, create instance type(s) describing the zero-DPU machines'
capabilities and associate those machines, so tenants can request instances of
that type. The instance type itself does **not** carry a "no-DPU" flag — what
makes a host zero-DPU is its `dpu_policy` (above). The instance type simply selects
which machines are allocatable; whether the selected host is zero-DPU then
governs the network model at allocation time.

---

## Tenant Operations

A tenant on a Flat VPC follows the same high-level flow as any tenant — create a
VPC, create an instance, watch its status — but with two Flat-specific
differences: the VPC's virtualization type is `FLAT`, and the instance is created
**without selecting any subnet or prefix**.

All tenant operations use the REST API or `nicocli`; none use TOML or
`nico-admin-cli`.

### 1. Create a Flat VPC

Create the VPC through the REST API with `networkVirtualizationType` set to
`FLAT`:

```http
POST /v2/org/{org}/nico/vpc
{
  "name": "flat-vpc-1",
  "siteId": "<site-id>",
  "networkVirtualizationType": "FLAT"
}
```

The accepted values are `ETHERNET_VIRTUALIZER`, `FNN`, and `FLAT`. Notes for
Flat VPCs:

- **`routingProfile` is rejected.** Routing profiles are FNN-only; supplying one
  on a Flat (or ETV) VPC is an error. The Flat data plane is operator-managed, so
  there is no NICo-side routing layer for a profile to configure.
- **A VNI is assigned.** Like other VPCs, a Flat VPC receives a VNI (or you may
  request a specific one via the optional `vni` field, subject to the site's
  allowed range). The VNI is surfaced to peers because operator-side SDN
  integrations may consume it (for example, for switch VTEPs or ACLs).
- **An NSG may be attached** via `networkSecurityGroupId`, exactly as for other
  VPC types. Bear in mind that for a Flat VPC the *enforcement* of those rules is
  the operator's network responsibility; NICo stores the association but does not
  program a DPU ACL for a zero-DPU host.

> The generated `nicocli vpc create` command accepts
> `--network-virtualization-type FLAT`. The specialized `nicocli tui` VPC
> creation flow does not prompt for a type and uses the Site default instead.
> Its `vpc virtualization update` request only supports changing to `FNN` and
> is rejected while the VPC contains any Subnets or Instances. Create a Flat
> VPC with the generated command or the REST API.

### 2. Find the HostInband segment backing the VPC

A Flat VPC accepts **only `HostInband` segments** — no Tenant, Admin, or Underlay
segments. The `HostInband` segments themselves are created by the **operator**
(see [Site Operations](#2-configuring-hostinband-network-segments)); a tenant
does not create them.

For a tenant's instance allocation to succeed, each `HostInband` segment its host
sits on must be **bound to a Flat VPC** (a VPC whose fabric interface type is
`nic`). Operators create `HostInband` segments unbound so they can exist for DHCP
during host ingestion; the VPC binding is required only when a tenant intent
actually arrives to allocate an instance. Coordinate with the operator to learn
which `HostInband` segment(s) back the hosts you intend to use, and ensure the
segment is associated with your Flat VPC before allocating.

### 3. Create an instance on a Flat VPC

This is the step that differs most from a normal VPC. On an ETV/FNN VPC a tenant
lists explicit network interfaces and the subnets/prefixes they draw from. On a
Flat VPC, a zero-DPU host has exactly one set of valid attachments — its
`HostInband` segment(s) — and NICo already knows them. So the tenant does **not**
select subnets or prefixes; instead the request sets `autoNetwork: true` and
leaves the interface list empty:

```http
POST /v2/org/{org}/nico/instance
{
  "name": "flat-instance-1",
  "vpcId": "<flat-vpc-id>",
  "machineId": "<zero-dpu-machine-id>",
  "autoNetwork": true
}
```

Rules enforced at allocation:

- **Zero-DPU hosts require `autoNetwork: true`.** A zero-DPU host cannot be
  allocated with an explicit interface list or with `autoNetwork: false`.
- **`autoNetwork: true` requires an empty interface list.** The two are mutually
  exclusive; NICo resolves one interface per `HostInband` segment the host is on.
- **`autoNetwork` is only valid on zero-DPU hosts.** A DPU-managed host rejects
  it and must list interfaces explicitly.
- **`autoNetwork` is immutable.** Once an instance is created with it, it stays
  auto for the life of the instance; sending `autoNetwork: true` on an update
  simply re-resolves interfaces from the host's *current* `HostInband` segments
  (a no-op if nothing changed).
- **No DPU extension services.** Extension services run on DPU agents; a zero-DPU
  host has none, so an instance config that requests them is rejected.

> The generated `nicocli instance create` command accepts
> `--auto-network=true`. The specialized `nicocli tui` flow sets
> `autoNetwork: true` automatically when the selected VPC is `FLAT` and skips
> the interface prompts.

### 4. Check instance status

Read instance status through the REST API or `nicocli`:

```text
GET /v2/org/{org}/nico/instance/{id}
nicocli instance get <instance-id>
```

What to look at for a Flat-VPC instance:

- **Lifecycle / `Ready` state.** The instance progresses through the normal
  tenant lifecycle. On a zero-DPU host the DPU-dependent waits are skipped
  (there is no DPU to push config to and no extension services to schedule), so
  readiness does not block on a DPU agent.
- **Resolved interfaces.** Because the request carried an empty interface list,
  the *config* side stays empty, and the **status** network interface list stands
  alone — it reports the interfaces NICo resolved from the host's `HostInband`
  segments, including each interface's allocated IP address and MAC. This is
  where a tenant reads the address their instance actually received.
- **Network `configs_synced`.** For a zero-DPU instance this reports **synced**
  based on NICo's own resolution from the `HostInband` segments — there is no DPU
  feedback loop in the path, so unlike an ETV/FNN instance, ethernet
  config-sync is not gated on a DPU confirming an overlay push.

A consequence worth stating plainly: a `Ready` Flat instance with a synced
network status means NICo has allocated and recorded the instance's underlay
addresses. It does **not** attest that traffic actually flows between this
instance and anything else — that depends on the operator's underlay/SDN
configuration, which NICo neither programs nor observes.

---

## What NICo Does and Doesn't Do for Flat VPCs

Because this boundary is the most common source of misunderstanding, it is worth
stating explicitly.

**NICo does:**

- Store the Flat VPC and its VNI, NSG association, and peering relationships.
- Allocate underlay addresses from the `HostInband` segment(s) and assign them to
  the instance's interfaces.
- Enforce that zero-DPU hosts allocate only into Flat VPCs via `HostInband`
  segments, and that DPU-managed hosts never do.
- Report the resolved interfaces and a synced network status once allocation is
  recorded.

**NICo does not:**

- Program any per-VPC VRF, BGP/EVPN, or route leaking for a Flat VPC (there is no
  DPU to program).
- Enforce routing isolation or NSG rules in the data path for a zero-DPU host.
- Observe or attest actual reachability between a Flat VPC and other networks.

Everything in the second list is the **network operator's** responsibility on the
physical / SDN fabric. The VNI NICo assigns is exposed precisely so that an
operator SDN integration can tie its switch-side configuration to the VPC.

---

## Verification

**Operator — site is Flat-ready:**

1. Confirm each intended host's effective policy resolves to `nic` or
   `ignore` after applying per-host and site-wide inheritance. Per-host
   `nic` and `ignore` win; per-host `manage` or an omitted value inherits
   `[site_explorer] dpu_policy`.
2. `HostInband` segments exist. `nico-admin-cli network-segment show` lists the
   declared segments with the expected prefix, gateway, and a VLAN/VNI assigned.
3. Each zero-DPU host's data-NIC MACs are registered, and the host shows
   interfaces bound to the expected `HostInband` segment.
4. Instance type(s) exist and the zero-DPU machines are associated, if tenants
   allocate by instance type.

**Tenant — Flat VPC and instance:**

1. The VPC reports `networkVirtualizationType: FLAT` and has a VNI; no routing
   profile is set.
2. The intended `HostInband` segment is bound to the VPC.
3. The instance was created with `autoNetwork: true`; `nicocli instance get`
   shows resolved status interfaces with allocated IP/MAC.
4. The instance reaches `Ready` with a synced network status. (Reachability
   itself is validated on the operator's fabric, not through NICo.)

---

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| VPC create rejected with a "ETHERNET_VIRTUALIZER, FNN, and FLAT are currently supported" error | `networkVirtualizationType` value is misspelled or unsupported |
| VPC create rejected for `routingProfile` | Routing profiles are FNN-only; remove the field for a Flat VPC |
| Instance allocation fails: "requires `auto` / `autoNetwork = true`" | A zero-DPU host was allocated with explicit interfaces or `autoNetwork: false`; set `autoNetwork: true` and omit interfaces |
| Instance allocation fails: "`autoNetwork` is only valid on zero-DPU hosts" | The target host has a NICo-managed DPU; either pick a zero-DPU host or list interfaces explicitly for a DPU-overlay VPC |
| Instance allocation fails: segment "is not bound to a Flat VPC" | The host's `HostInband` segment has no VPC; the tenant/operator must bind it to a Flat VPC first |
| Instance allocation fails: segment bound to a VPC whose `fabric_interface_type` is not `nic` | The `HostInband` segment is attached to a non-Flat VPC; only Flat VPCs may own `HostInband` segments |
| Instance allocation fails: extension services on a zero-DPU host | Remove `dpu_extension_services` from the instance config; a zero-DPU host cannot run them |
| The specialized TUI does not offer `FLAT` during VPC creation | Use generated `nicocli vpc create --network-virtualization-type FLAT` or the REST API; the TUI uses the Site default |
| Instance is `Ready` but cannot reach another host | Expected from NICo's side — Flat data-plane reachability is the operator's fabric responsibility, not something NICo programs or verifies |
