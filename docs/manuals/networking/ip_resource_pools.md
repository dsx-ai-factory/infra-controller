# IP Resource Pools

This page explains what IP resource pools are, which named pools exist, what each pool is used for, how to size them correctly for a site, and how to inspect and grow them at runtime.

It is intended for operations engineers deploying or scaling a NICo-managed site who need to configure IP address pools in the API server configuration.

---

## What IP Resource Pools Are

The NICo API server manages several distinct IP address pools. Each pool is drawn down as the system allocates addresses for a specific purpose. When a pool is exhausted, the operations that depend on it are blocked immediately — there is no fallback or queuing behavior.

Pool capacity planning is therefore a pre-deployment concern. Pools can be extended at runtime without a restart, but running out of addresses causes provisioning failures that must be resolved before work can continue.

> **Note**: Tenant instance IP addresses — the addresses that instances see on their network interfaces — are not managed through IP resource pools. The pools described on this page are infrastructure addresses, not tenant workload addresses.

---

## Named IP Pools

The following IP pools are recognized by the API server.

| Pool name | Allocation unit | Required |
|---|---|---|
| `lo-ip` | One IPv4 per managed DPU | Yes |
| `lo-ip-v6` | One IPv6 per managed DPU | No (dual-stack underlay only) |
| `vpc-dpu-lo` | One IP per VPC per DPU that participates in that VPC | Yes |

### `lo-ip` — DPU Loopback IPs

Each managed DPU receives a loopback IP address from this pool. The loopback is advertised over BGP and serves as the VTEP address for the VXLAN underlay. It is also used for BGP peering identification.

One address is consumed per DPU at the time the DPU is first registered with the system. If a DPU is decommissioned, its address is returned to the pool.

By default, addresses are drawn automatically from the pool's auto-assign partition when a DPU is registered. Operators who need a specific DPU to always receive a specific loopback can instead reserve one; see [Deterministic DPU loopback reservations](#deterministic-dpu-loopback-reservations). Reserved addresses must be placed in a non-auto-assign range so automatic registration never hands them to a different DPU.

An optional companion pool, `lo-ip-v6` (`type = "ipv6"`), supplies IPv6 DPU loopbacks with the same per-DPU allocation and reservation behavior when the site enables dual-stack underlay addressing.

### `vpc-dpu-lo` — VPC DPU Loopback IPs

When a VPC is created and an instance is placed on a DPU for the first time, a loopback IP is allocated for that specific (VPC, DPU) pair. This IP is used as the per-VPC VTEP in the VPC overlay network.

Consumption is demand-driven: addresses are allocated as instances spread across DPUs, and deallocated when all instances for a VPC are removed from a given DPU.

---

## Pool Definition Structure

IP pools are defined in the `[pools.<name>]` section of the API server TOML configuration.

Every pool definition requires a `type` field and either a `prefix` or a `ranges` list. Providing both, or providing neither, is a configuration error that prevents startup.

### Prefix-based definition

The pool contains all usable host addresses within the given CIDR prefix. Broadcast and network addresses are excluded automatically.

```toml
[pools.lo-ip]
type = "ipv4"
prefix = "10.180.62.0/26"
```

### Range-based definition

The pool contains exactly the addresses specified. Multiple ranges may be listed; they are additive.

```toml
[pools.vpc-dpu-lo]
type = "ipv4"
ranges = [
  { start = "10.180.63.1", end = "10.180.63.254" },
]
```

Each range accepts an optional `auto_assign` boolean, which defaults to `true`. A range with `auto_assign = false` is excluded from automatic allocation: its addresses are handed out only when explicitly requested, which is how [deterministic DPU loopback reservations](#deterministic-dpu-loopback-reservations) claim a specific address. Prefix-based definitions are always auto-assign, so carve reservable addresses out as an explicit `auto_assign = false` range.

```toml
[pools.lo-ip]
type = "ipv4"
ranges = [
  # Handed out automatically to DPUs as they register.
  { start = "10.180.62.1", end = "10.180.62.199" },
  # Held back for deterministic per-DPU reservations.
  { start = "10.180.62.200", end = "10.180.62.254", auto_assign = false },
]
```

### Pool type values

The `lo-ip` and `vpc-dpu-lo` pools use `type = "ipv4"`. The optional `lo-ip-v6` pool uses `type = "ipv6"`.

### Full example

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

---

## Sizing

### `lo-ip`

One address is consumed per managed DPU. For a site with **H** hosts, each having **D** DPUs:

```text
lo-ip pool size = H × D
```

For example, 100 hosts with 2 DPUs each require 200 addresses.

`docs/getting-started/prerequisites/network.md` states the combined IPv4 prefix requirement as `(expected number of servers + expected number of DPUs) × 2 + 2`. The `lo-ip` pool supplies the per-DPU loopback portion of that allocation. The remainder of the formula covers admin and other infrastructure addresses.

### `vpc-dpu-lo`

Consumption depends on the number of VPCs and how broadly their instances are distributed across DPUs. In the worst case, every DPU participates in every VPC:

```text
vpc-dpu-lo pool size (worst case) = number of VPCs × total number of DPUs
```

In practice, consumption is typically lower because tenant workloads are not spread uniformly across the entire fleet. Plan for the worst case unless site-specific workload data supports a smaller estimate.

`docs/getting-started/prerequisites/network.md` notes that a separate IPv4 prefix is required for these addresses, with a total allocation of `expected number of DPUs × 2` at minimum. Size the `vpc-dpu-lo` pool within that allocation, leaving headroom for VPC growth.

### Headroom recommendation

Plan for fleet growth and leave headroom in all pools. Growing a pool requires only a `grow` operation (described below) with no service restart, but an exhausted pool blocks provisioning immediately. A reasonable starting margin is 20–25% above the expected peak allocation for the initial site scale.

---

## Startup Behavior

At startup, the API server registers every pool defined in `[pools.*]` into the database.

This operation is additive only. Existing pools can be extended with new ranges or a larger prefix; they cannot be shrunk. This means a misconfigured range that was once registered cannot be removed by changing the config — it persists in the database. New ranges defined in the config are added to the existing pool on the next startup.

If `listen_only = true` is set in the API server configuration, pool registration is skipped on that instance. It assumes another instance has already populated the pools.

---

## Deterministic DPU loopback reservations

By default NICo assigns each DPU an underlay loopback automatically from the auto-assign partition of `lo-ip` (and `lo-ip-v6`, when configured). A reservation lets an operator pin a specific address to a specific DPU ahead of time — for example, to align a DPU's VTEP address with an external routing or monitoring configuration.

Declare loopbacks in the host `ExpectedMachine.interfaces` list using `role: "dpu_loopback"` and `fixed_ip`. The MAC identifies the DPU's OS/admin interface; it may also appear in a `dpu_os` entry. A separate `dpu_os` entry is optional. For example:

```json
{
  "interfaces": [
    {"mac_address": "02:00:00:00:00:20", "role": "dpu_os", "ip_allocation": "fixed", "fixed_ip": "192.0.2.20"},
    {"mac_address": "02:00:00:00:00:20", "role": "dpu_loopback", "ip_allocation": "fixed", "fixed_ip": "198.51.100.20"},
    {"mac_address": "02:00:00:00:00:20", "role": "dpu_loopback", "ip_allocation": "fixed", "fixed_ip": "2001:db8::20"}
  ]
}
```

This is the interface portion of an ExpectedMachine configuration; retain the machine's identity and other required fields when creating it. Each MAC may declare one loopback per address family. Omitting `ip_allocation` infers Fixed from `fixed_ip`. Dynamic and Retained loopbacks are unsupported. Loopbacks reject interface names, segment hints, masks, gateways, and `primary`, including `primary: false`.

### Validation and allocation

A loopback address must belong to a non-auto-assign range (`auto_assign = false`) of `lo-ip` for IPv4 or `lo-ip-v6` for IPv6. It must not be reserved for another MAC or allocated to another DPU. MACs and per-family addresses are unique across the site, enforced by the reservation child table. Re-submitting unchanged intent is accepted, as is restoring an address already owned by the DPU with that MAC.

Loopbacks do not participate in DHCP, physical-interface preallocation, or primary-interface selection. In the example, `192.0.2.20` configures the OS interface and must satisfy its managed-prefix rules; the other addresses come from loopback pools.

Site Explorer matches each declared OS/admin MAC to one discovered DPU within the expected host. Missing or ambiguous matches defer creation with an error. Direct discovery uses the authenticated caller's OS/admin interface MAC. Neither path needs a pre-existing MAC-to-machine association to allocate a loopback. Both claim addresses transactionally with creation, with live pool ownership assigned to the DPU MachineId. A family without a declaration keeps automatic allocation.

### Updates and lifecycle

Use the existing `expected-machine add --interfaces` or `patch --interfaces` options, or the `interfaces` field in files for `update` and `replace-all`. `nico-admin-cli expected-machine show` displays a `DPU Loopback Reservations` column with one DPU OS/admin MAC and reserved IP per line; machines without reservations have an empty cell. `nico-admin-cli --format json expected-machine show` includes the same reservations as `dpu_loopback` entries in `interfaces`. Interface replacement replaces the complete list: resubmit physical-interface entries that should remain. Remove only the loopback entries to clear reservations while retaining physical-interface configuration. Passing `--interfaces '[]'` clears the entire list.

For file-based `update`, omitted or null `interfaces` preserves the stored list; a supplied array replaces it. `replace-all` is authoritative, including when a machine's interface list is omitted. The CLI sets the wire-level `replace_dpu_loopbacks` signal when it explicitly replaces interfaces. Older gRPC clients that omit this signal and loopback entries preserve stored reservations, including during replace-all. Supplying loopback entries explicitly replaces the loopback subset; an empty subset clears only when the signal is true. Native patches apply this behavior only when the interface field is selected in the update mask.

Changing or clearing intent does not readdress a live DPU. Permanent deletion releases the addresses actually owned by that DPU; subsequent ingestion uses the latest declarations or automatic allocation. Existing-DPU IPv6 backfill, host–DPU pairing, `fallback_dpu_serial_numbers`, and the separate `vpc-dpu-lo` pool retain their existing behavior.

See the [Core issue](https://github.com/dsx-ai-factory/infra-controller/issues/5705) and [loopback reservation epic](https://github.com/dsx-ai-factory/infra-controller/issues/5412) for scope. Live readdressing remains separate in [issue #2096](https://github.com/dsx-ai-factory/infra-controller/issues/2096).

---

## Runtime Operations

### Listing Pools

Run the following command to inspect resource pools:

```bash
nico-admin-cli resource-pool list
```

The table contains eight columns: `Name`, `Backend`, `Remote Pool`, `Min`, `Max`, `Size`, `Used`, and `Status`. For column definitions, missing counts, diagnostics, older-server behavior, and an example, refer to [Listing Pools in the VNI manual](../vpc/vni_resource_pools.md#listing-pools).

For IP pools, `Min` and `Max` show the lowest and highest addresses or prefixes in stored local inventory. Prefixes retain their CIDR length, such as `/64`; empty inventory has blank bounds.

In v2.4, `nico-api` fails to start if a pool selects a gRPC backend; [gRPC lifecycle integration](https://github.com/dsx-ai-factory/infra-controller/issues/7140) is tracked separately. Monitor known counts and the required automatic or manual range before it is exhausted. Provisioning fails when the selected pool has no eligible free address or prefix.

### Growing a pool

```bash
admin-cli resource-pool grow -f <toml-file>
```

The file must contain a TOML snippet using the same `[pools.<name>]` format as the API server configuration. Any pool definitions in the file that introduce new ranges or a larger prefix are applied immediately. The operation does not require a service restart.

**Constraints:**

- New ranges must not overlap any existing ranges in the same pool.
- New ranges must not overlap ranges defined in any other pool.
- Prefix-based pools can be extended by providing a new prefix that encompasses previously unused addresses; the existing allocated addresses remain unchanged.
- Pools cannot be shrunk. Ranges cannot be removed.

**Example grow file** (`grow-lo-ip.toml`):

```toml
[pools.lo-ip]
type = "ipv4"
ranges = [
  { start = "10.180.62.128", end = "10.180.62.191" },
]
```

If the original pool was prefix-based, the grow file may also provide additional ranges using `ranges` — the two forms are not mutually exclusive when growing.

---

## Related Pages

- `docs/manuals/vpc/vni_resource_pools.md` — companion page covering VNI and VLAN ID resource pools
- `docs/manuals/vpc/vpc_network_virtualization.md` — end-to-end VPC network virtualization overview that ties VNI pools, IP pools, and routing profiles together
- `docs/getting-started/prerequisites/network.md` — site-level networking requirements including IPv4 prefix sizing formulas
- `docs/manuals/vpc/vpc_routing_profiles.md` — VPC routing profile configuration, which governs the VPC overlay network configuration and therefore drives `vpc-dpu-lo` consumption
