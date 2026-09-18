# Network Security Groups

Network Security Groups (NSGs) are tenant-owned, rule-based filters that sit
on top of the VPC / VRF model. They provide stateful or stateless L3 / L4
filtering for traffic into and out of tenant instances, complementing the
routing isolation that the VPC itself provides.

This page describes the NSG object model, how rules are attached to traffic,
how the operator enables and constrains the feature, and how to verify and
troubleshoot rule enforcement.

**Related pages**

- [Network Isolation Overview](../network_isolation.md)
- [VPC Network Virtualization](../vpc/vpc_network_virtualization.md) — the
  VPC, VRF, and DPU configuration layer that NSGs sit on top of

---

## Where NSGs Sit in the Stack

A tenant's traffic on a NICo-managed host passes through three independent
isolation layers in order:

1. **VPC / VRF.** The DPU places each interface into the VRF of the VPC
   whose VpcPrefix the interface draws its /31 from. Routes do not leak
   between VRFs unless an operator opts in via routing-profile flags or
   VPC peering.
2. **`deny_prefixes` site ACL.** The site-wide `deny_prefixes` list,
   configured under the API server's networking config, blocks tenant
   traffic to a fixed set of prefixes (typically management plane and
   infrastructure). This applies to every VRF on every DPU and cannot be
   overridden by a tenant.
3. **Network Security Groups.** Per-VPC and per-instance rule sets, set by
   the tenant, with optional site-wide operator overrides inserted ahead of
   tenant rules.

NSGs are the right tool for filtering East-West traffic *inside* a VPC, for
restricting which underlay-leaked prefixes a tenant is permitted to reach,
and for enforcing site-wide baselines (operator overrides) that no tenant
can disable. They are not a substitute for VPC routing isolation: an NSG
cannot make two VPCs reachable that the routing profile keeps apart.

---

## Operations: Who Does What

NSGs are almost entirely a tenant operation. The only operator-owned pieces
are the site-level limits and overrides (TOML) and a break-glass path for
incident response. See
[Network Isolation → Who configures what, and how](../network_isolation.md#who-configures-what-and-how)
for the role and interface model.

| Task | Role | Interface |
|---|---|---|
| Site limits and overrides (`max_network_security_group_size`, `stateful_acls_enabled`, `policy_overrides`) | Operator | **TOML** (`[network_security_group]`) — Day 0 / rare; API restart |
| Create / update / delete an NSG | Tenant | **REST** `…/nico/network-security-group` · `nicocli network-security-group create` |
| Attach an NSG to a VPC | Tenant | **REST** (`update-vpc`) · `nicocli vpc update` |
| Attach an NSG to an instance | Tenant | **REST** (`update-instance`) · `nicocli instance update` |
| Inspect an NSG's rules, version, and status | Tenant | **REST** `GET …/nico/network-security-group/{id}` · `nicocli network-security-group get` |
| Check where an NSG is attached and its rollout state | Tenant | **REST** `GET …/nico/vpc/{id}` or `…/nico/instance/{id}` (`networkSecurityGroupPropagationDetails`) |
| Break-glass: modify a tenant's NSG during an incident | Operator | **`nico-admin-cli`** (gRPC) — emergency only |

---

## The Rule Model

An NSG is a tenant-owned object with the following shape:

| Field | Purpose |
|---|---|
| `id` | NSG identifier, optionally supplied by the caller as a UUID at creation. If omitted, NICo generates one. Either way, it is returned on the created NSG. A supplied `id` that collides with an existing NSG is rejected with `409 Conflict`. |
| `tenant_organization_id` | The owning tenant |
| `stateful_egress` | When `true`, return traffic for egress flows is permitted automatically. It can be enabled only when the site-level `stateful_acls_enabled` flag is `true` (see below) |
| `rules` | Ordered list of `NetworkSecurityGroupRule` entries, evaluated by priority |
| `version` | Optimistic-concurrency token; required when updating the NSG |

Each rule has:

| Field | Allowed values |
|---|---|
| `direction` | `Ingress` or `Egress` |
| `ipv6` | `true` for IPv6 rules, `false` for IPv4 (split into two policies on the DPU) |
| `protocol` | `Any`, `Icmp`, `Icmp6`, `Udp`, `Tcp` |
| `src_net` / `dst_net` | A CIDR prefix (the wire model is `NetworkSecurityGroupRuleNet::Prefix(IpNetwork)`) |
| `src_port_start` / `src_port_end` | Optional inclusive source port range |
| `dst_port_start` / `dst_port_end` | Optional inclusive destination port range |
| `action` | `Permit` or `Deny` |
| `priority` | Integer; lower numbers evaluated first inside a policy |

Rules are evaluated in priority order. The first matching `Permit` or
`Deny` decides the packet; there is no implicit fall-through behaviour
between rules of the same NSG.

---

## Attaching an NSG

NSGs attach at exactly two scopes:

- **VPC scope.** Set `network_security_group_id` on the VPC by updating it
  (REST `PATCH …/nico/vpc`, or `nicocli vpc update`). Every instance that has
  interfaces in this VPC inherits the NSG's rules.
- **Instance scope.** Set `network_security_group_id` on the instance by
  updating it (REST `PATCH …/nico/instance`, or `nicocli instance update`).
  The instance NSG **replaces** the VPC NSG for that instance. Instance-scope
  NSGs are not merged with VPC-scope NSGs; the instance-scope NSG wins
  outright.

An NSG can be referenced by multiple VPCs and multiple instances
concurrently. The NSG object itself does not list its references — the
attachment lives on the referencing side, as `networkSecurityGroupId` on
each VPC or instance. To find what an NSG is attached to, inspect the VPCs
and instances (their `networkSecurityGroupPropagationDetails` also report
rollout state — see [How NSG Changes Propagate](#how-nsg-changes-propagate)).

### Deletion

`nicocli network-security-group delete` (REST `DELETE …/nico/network-security-group/{id}`)
succeeds only when the NSG is not referenced by any VPC or instance. The
expected workflow is:

1. Detach the NSG from any VPCs (`nicocli vpc update` with the field cleared).
2. Detach the NSG from any instances (`nicocli instance update`).
3. Wait for the NSG's propagation status to clear for those objects (see
   [How NSG Changes Propagate](#how-nsg-changes-propagate)).
4. Delete the NSG.

---

## How NSG Changes Propagate

NSG propagation is tracked **differently** from the other network fabrics.
For VRF / segment / routing changes, convergence is reflected in the
instance's `configs_synced.ethernet` field, which gates the instance's
`Ready` state. **NSGs do not work that way.** Read this section before
relying on any readiness signal to confirm an NSG change has taken effect.

### Attaching or detaching an NSG

There are two attachment scopes, and they interact with the instance config
version differently:

- **Instance scope.** `network_security_group_id` is part of the instance's
  versioned configuration, so attaching or detaching an NSG directly on an
  instance **bumps that instance's `config_version`**.
- **VPC scope.** The instance inherits the VPC's NSG (the instance reports
  `networkSecurityGroupInherited = true`) without the NSG id appearing in
  the instance's own config. A VPC-scope attach or detach therefore does
  **not** change the inheriting instances' `config_version`.

In neither case does NSG attachment feed `configs_synced.ethernet` or the
instance's `Ready` state. Even at instance scope — where the config version
does bump — the actual application of the rules on the DPU is reported
through the NSG's own propagation status, not through the config-sync
machinery.

### Updating an NSG's rules

Editing the rules of an NSG that is already attached bumps only the **NSG's
own `version`**. It does **not** bump any instance's `config_version` and
does **not** change any instance's `configs_synced`. The only place the
rollout is visible is the **NSG propagation status**: each attached DPU
observes that the NSG version applied on its interfaces is older than the
NSG version now on the VPC / instance, and that mismatch is what the
propagation status reflects until the new rules are applied.

### Where propagation status lives

Propagation is exposed per attached object on the VPC and Instance
resources as `networkSecurityGroupPropagationDetails`, with:

- `status` — `Synchronizing`, `Synchronized`, or `Error`.
- `detailedStatus` — `None`, `Partial`, `Full`, `Unknown`, or `Error`
  (how many of the expected interfaces have applied the current NSG version).
- `unpropagatedInstanceIds` — instances that have not yet applied the
  current version; the set to watch during a rollout.

NICo computes this on read by comparing the NSG id and version on the
VPC / instance against what each DPU reports observing on its interfaces.
There is no separate "NSG synced" flag on the instance; the propagation
detail is the authoritative signal.

---

## DPU Enforcement

NSG rules are resolved on the API server and pushed to the DPU as part of
the per-interface configuration response, alongside the VRF, segment, and
routing-profile data described in
[VPC Network Virtualization](../vpc/vpc_network_virtualization.md). The DPU
agent materialises them into NVUE ACLs.

The DPU receives, per interface:

- The resolved rule set (already expanded across port and prefix ranges).
- A `source` tag (`NSG_SOURCE_NONE`, `NSG_SOURCE_VPC`, or
  `NSG_SOURCE_INSTANCE`) indicating which scope produced the rules.
- The `stateful_egress` flag.
- The NSG `id` and `version`, used by the propagation-status reporting.

The agent renders IPv4 and IPv6 rules into separate NVUE policies and
combines ingress and egress rules into the appropriate direction. Site-wide
operator overrides (see below) are rendered into a **separate policy** that
the DPU evaluates **before** any tenant policy.

---

## Site-Level Operator Configuration

The operator controls three site-wide knobs that affect NSG behaviour.
These live in the API server configuration file under
`[network_security_group]`:

```toml
[network_security_group]
# Hard cap on the number of expanded rules per NSG.
# Expansion = src port range × dst port range × src prefix list × dst prefix list.
max_network_security_group_size = 200

# Master switch for stateful (reflexive) ACL support.
# Leave disabled until every DPU in the site is running HBN 2.3 or later.
stateful_acls_enabled = false

# Site-wide override rules. Inserted into a separate policy on the DPU
# that is evaluated AFTER deny_prefixes but BEFORE any tenant NSG rules.
# A tenant cannot disable or contradict these.
policy_overrides = []
```

### `max_network_security_group_size`

NICo expands rule entries before pushing them to the DPU (the cartesian
product of source ports × destination ports × source prefixes × destination
prefixes). This cap is the operator's protection against a tenant
accidentally requesting a vast rule set. The DPU agent also enforces its
own ceiling, on the order of 10,000 expanded rules, as a final safeguard.

A tenant whose NSG would expand beyond this limit receives an error when
creating or updating the NSG. Tune this
value if tenants legitimately need larger rule sets; do not raise it
unilaterally without coordinating with whatever ceiling is configured on
the DPU side.

### `stateful_acls_enabled`

Toggling this flag controls whether NICo will configure the DPU's default
stateful-ACL options in the NVUE config it pushes. Stateful NSG behaviour
also requires the tenant to set `stateful_egress = true` on the NSG;
when the site-level switch is disabled, NICo rejects stateful NSG
creation and updates that enable stateful egress. An existing stateful
NSG can still be otherwise updated if an operator later disables the
site flag, but the DPU treats its rules as stateless until site support
is enabled again.

Leave `stateful_acls_enabled = false` until every DPU in the site is
running HBN 2.3 or later. Earlier HBN versions implement reflexive ACLs in
a way that lets a single rule permit traffic in both directions, which is
operationally unsafe.

### `policy_overrides`

`policy_overrides` is a list of `NetworkSecurityGroupRule` entries (same
shape as tenant rules) that the operator wishes to enforce site-wide.

These rules are inserted into a **separate policy** on the DPU, evaluated:

1. **After** `deny_prefixes` (the absolute site denylist).
2. **Before** any tenant-defined NSG rules.

This ordering gives the operator a reliable place to:

- Force-permit infrastructure flows (for example, package mirrors,
  telemetry collectors, time servers) that every tenant must be able to
  reach, without depending on each tenant putting them in their own NSG.
- Force-deny baselines that every tenant must obey, regardless of what
  their own NSGs say. A tenant cannot write a `Permit` rule that
  contradicts an operator override, because the override is evaluated
  first and decides the packet.

Each entry follows the same JSON / TOML structure as a tenant rule. A
worked example:

```toml
[network_security_group]
stateful_acls_enabled = false
max_network_security_group_size = 200

  [[network_security_group.policy_overrides]]
  direction = "Egress"
  ipv6 = false
  protocol = "Udp"
  dst_net = "10.0.5.0/24"          # site-controller VIPs
  dst_port_start = 123
  dst_port_end = 123
  action = "Permit"
  priority = 10

  [[network_security_group.policy_overrides]]
  direction = "Egress"
  ipv6 = false
  protocol = "Tcp"
  dst_net = "0.0.0.0/0"
  dst_port_start = 22
  dst_port_end = 22
  action = "Deny"
  priority = 20
```

Changing `policy_overrides` requires restarting the API server (it is a
static configuration field, not a runtime-mutable RPC). After restart, the
new override set propagates to every DPU as part of the next
configuration-poll cycle.

---

## Quarantine and Forced Override

When a managed host is placed into a quarantine lifecycle state, NICo
substitutes a quarantine-specific override policy in place of
`policy_overrides`. The intent is to give an operator a way to constrain
traffic from a host that is under investigation without having to detach
it from its tenant VPCs first. This is internal behaviour and is not
operator-configurable; quarantine is driven by the machine lifecycle and
health-alert subsystem.

---

## Current Limitations

The NSG feature is in production use, but the following limitations are
worth knowing before designing rule sets:

- **Rule `src_net` / `dst_net` accept CIDR prefixes only.** The model has
  a structural extension point for VPC references (so that a tenant could
  say "permit from any instance in VPC X"), but VPC-reference resolution
  is not yet implemented. Use explicit CIDRs.
- **IPv4 and IPv6 are separate rules.** A rule has an `ipv6` boolean and
  applies only to one address family; if a tenant needs both, two rules
  are required.
- **`stateful_egress` requires the site flag.** Creating an NSG with
  `stateful_egress = true`, or updating it from `false` to `true`, is
  rejected unless the site-level `stateful_acls_enabled` flag is `true`.
- **Updates require version-token agreement.** An NSG update takes the
  NSG's `version` and fails if it has been concurrently modified. This is
  the standard NICo optimistic-concurrency pattern.

---

## Configuration Workflow

The site operator's NSG configuration is normally done once and rarely
changed; the tenant flow is what runs day-to-day.

### Operator (once per site)

1. Confirm the HBN version on every DPU in the site. If any DPU is on a
   version earlier than HBN 2.3, leave `stateful_acls_enabled = false`.
2. Decide the rule-size budget per NSG and set
   `max_network_security_group_size` accordingly.
3. Decide what baseline traffic must be permitted (telemetry, NTP, package
   mirrors) and what baseline must be denied (operator-defined denylist
   that goes beyond `deny_prefixes`). Encode these as
   `policy_overrides`.
4. Restart the API server.

### Tenant (per NSG)

All tenant steps use the REST API or `nicocli`; none require TOML or
`nico-admin-cli`.

1. `nicocli network-security-group create` (REST `POST …/nico/network-security-group`)
   with the desired rule set. An `id` (UUID) may optionally be supplied to assign
   a specific identifier; if omitted, NICo generates one. The response includes
   the NSG `id` and `version`. Supplying an `id` that already exists returns
   `409 Conflict`.
2. Attach the NSG at VPC scope (`nicocli vpc update`) or instance scope
   (`nicocli instance update`).
3. Wait for the NSG **propagation status** on the VPC / instance to reach
   `Synchronized` (`detailedStatus: Full`). Do **not** wait on
   `configs_synced.ethernet` — NSG rollout is not reported there. See
   [How NSG Changes Propagate](#how-nsg-changes-propagate).

Updates use `nicocli network-security-group update` with the current `version`
token. Deletion uses `nicocli network-security-group delete` after detaching
every reference.

---

## Verification

For a given tenant configuration, confirm:

1. **NSG exists and is the expected version.**
   `nicocli network-security-group get <id>` (REST
   `GET …/nico/network-security-group/{id}`) shows the rules,
   `stateful_egress`, and current `version`.
2. **The NSG is attached where intended.** Check the
   `networkSecurityGroupPropagationDetails` on each VPC and instance
   (`nicocli vpc get <id>` / `nicocli instance get <id>`); each lists the
   attached `networkSecurityGroupId` and, for instances, whether it is
   inherited from the VPC (`networkSecurityGroupInherited`).
3. **Propagation has converged.** On those same objects,
   `networkSecurityGroupPropagationDetails.status` reads `Synchronized` with
   `detailedStatus: Full`, and `unpropagatedInstanceIds` is empty. While the
   rollout is in flight the status is `Synchronizing`. Note this is a
   **separate** signal from `configs_synced.ethernet`, which NSG changes do
   not affect.
4. **Operator overrides are present site-wide.** On any active DPU,
   inspect the running NVUE ACL configuration; the override policy is
   distinct from the tenant policy and contains the rules listed in the
   API server's `policy_overrides`. A discrepancy means the API server
   was not restarted after the configuration change.

---

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `network-security-group create` fails with size-limit error | Expanded rule count exceeds `max_network_security_group_size` |
| NSG creation or update fails when enabling stateful egress | `stateful_acls_enabled = false` site-wide |
| `network-security-group create` fails with a conflict (`409`) on `id` | A caller-supplied NSG `id` collides with an existing NSG; omit the `id` to auto-generate one, or choose a different UUID |
| Stateful return traffic is being dropped | `stateful_acls_enabled = false` site-wide, or `stateful_egress = false` on the NSG, or DPU on HBN earlier than 2.3 |
| Tenant rule that "should permit" a flow has no effect | An operator `policy_overrides` rule denies the same flow at higher (earlier-evaluated) priority |
| Two instances in the same VPC cannot reach each other on a permitted port | NSG attached only to one side, or the NSG attached at instance scope overrides the VPC-scope NSG |
| `network-security-group delete` fails | At least one VPC or instance still sets this NSG as its `networkSecurityGroupId`; detach on those objects first |
| Site-wide override change does not take effect | API server was not restarted after editing `policy_overrides` |
| NSG rule change made, but `configs_synced.ethernet` / instance `Ready` never reacted | Expected: NSG changes do not feed `configs_synced`. Track rollout via `networkSecurityGroupPropagationDetails` instead |
| NSG propagation status stuck `Synchronizing` (`unpropagatedInstanceIds` non-empty) | DPUs on the listed instances have not yet applied the current NSG version; check DPU health and the per-interface config push |
| NSG propagation status `Error` | A DPU rejected the rule set (for example, expanded size over the DPU ceiling); inspect the propagation `details` string |
