# IP Resource Pools

This page explains what IP resource pools are, which named pools exist, what each pool is used for, how to size them correctly for a site, and how to inspect and grow them at runtime.

It is intended for operations engineers deploying or scaling a NICo-managed site who need to configure IP address pools in the API server configuration.

---

## What IP Resource Pools Are

The NICo API server manages several distinct IP address pools. Each pool is drawn down as the system allocates addresses for a specific purpose. When a pool is exhausted, the operations that depend on it are blocked immediately — there is no fallback or queuing behavior.

Pool capacity planning is therefore a pre-deployment concern. Pools can be extended at runtime without a restart, but running out of addresses causes provisioning failures that must be resolved before work can continue.

> **Note**: Tenant instance IP addresses — the addresses that instances see on their network interfaces — are not managed through IP resource pools.  The pools described on this page are infrastructure addresses, not tenant workload addresses.

---

## Named IP Pools

The following IP pools are recognized by the API server.

| Pool name | Allocation unit | Required |
|---|---|---|
| `lo-ip` | One IP per managed DPU | Yes |
| `vpc-dpu-lo` | One IP per VPC per DPU that participates in that VPC | Yes |

### `lo-ip` — DPU Loopback IPs

Each managed DPU receives a loopback IP address from this pool. The loopback is advertised over BGP and serves as the VTEP address for the VXLAN underlay. It is also used for BGP peering identification.

One address is consumed per DPU at the time the DPU is first registered with the system. If a DPU is decommissioned, its address is returned to the pool.

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

### Pool type values

Both IP pools described on this page use `type = "ipv4"`.

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

```
lo-ip pool size = H × D
```

For example, 100 hosts with 2 DPUs each require 200 addresses.

`docs/getting-started/prerequisites/network.md` states the combined IPv4 prefix requirement as `(expected number of servers + expected number of DPUs) × 2 + 2`. The `lo-ip` pool supplies the per-DPU loopback portion of that allocation. The remainder of the formula covers admin and other infrastructure addresses.

### `vpc-dpu-lo`

Consumption depends on the number of VPCs and how broadly their instances are distributed across DPUs. In the worst case, every DPU participates in every VPC:

```
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

## Runtime Operations

### Listing pools

```
admin-cli resource-pool list
```

This queries the API server for current pool state. The output is a table with the following columns:

| Column | Meaning |
|---|---|
| Name | Pool name |
| Min | Lowest address in the pool |
| Max | Highest address in the pool |
| Size | Total number of addresses in the pool |
| Used | Number currently allocated, and percentage of total |

Monitor pools that are approaching their limit. When `Used` is near `Size`, the next provisioning operation that needs that pool will fail. Grow the pool before that point.

### Growing a pool

```
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

