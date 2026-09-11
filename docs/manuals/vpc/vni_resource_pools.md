# VNI Resource Pools

This page explains how VNI resource pools work in NICo, which named pools exist, how they are
configured, how to size them correctly, and how to inspect and extend them at runtime.

This page is intended for operations engineers who are deploying or growing a site and need to
configure the API server's resource pools correctly.

## Related documentation

- `docs/manuals/vpc/vpc_routing_profiles.md` — how the `internal` flag on a routing profile
  determines which VNI pool is used
- `docs/getting-started/prerequisites/network.md` — site-wide networking prerequisites, including
  general VNI and ASN allocation guidance
- `docs/manuals/networking/ip_resource_pools.md` — IP resource pool configuration
- `docs/manuals/vpc/vpc_network_virtualization.md` — end-to-end VPC network virtualization
  overview that ties VNI pools, IP pools, and routing profiles together

---

## What VNIs are used for

Each VPC is assigned one VNI from a resource pool at creation time. That VNI serves two purposes.

**VXLAN tunnel identifier.** The VNI identifies the L3VPN VXLAN tunnel associated with the VPC's
overlay network. All traffic within the VPC is encapsulated using this identifier.

**Native BGP EVPN route-target.** The VPC's VNI is used to construct the VPC's native
route-target in the form `<datacenter_asn>:<vpc_vni>`. This route-target governs which routes are
imported and exported for the VPC through BGP EVPN. This is the mechanism by which routing
profiles and route-target configuration take effect.

Because the VNI feeds directly into the route-target, VNI ranges must be coordinated with the
network team before a site goes live. VNI ranges for internal and external VPCs must be distinct
from each other and from any other pool on the site.

The VNI assigned at creation is permanent. A VPC cannot change its routing profile after creation,
and therefore cannot change which pool it was allocated from. VNI release happens automatically
when a VPC is deleted.

---

## Named pools

### VPC pools

These two pools cover the VPCs. The selection between them is made
automatically at VPC creation time based on whether the resolved routing profile has `internal =
true` or `internal = false`. See `docs/manuals/vpc/vpc_routing_profiles.md` for how the routing
profile is resolved.

| Pool name | Purpose |
|---|---|
| `vpc-vni` | VNIs for **internal** VPCs — routing profile has `internal = true` |
| `external-vpc-vni` | VNIs for **external** VPCs — routing profile has `internal = false` |

Pool selection is automatic: if the resolved routing profile has `internal = true`, the VNI is
allocated from `vpc-vni`; otherwise it is allocated from `external-vpc-vni`.

The `external-vpc-vni` pool is optional. If it is absent from the configuration and no external
VPCs are created, the API server will start normally. However, if any VPC creation request
resolves to an external routing profile and the pool is not defined, the request will fail with a
resource-exhausted error.

### Additional pools

The following pools are used for other allocation types. They are not covered in detail on this
page but are listed here for completeness.

| Pool name | Purpose |
|---|---|
| `vni` | Per-segment or per-interface VNIs for L2VPN (`ETHERNET_VIRTUALIZER`) network segments |
| `vlan-id` | VLAN IDs for DPU-to-host inband communication |
| `fnn-asn` | Per-DPU BGP ASNs |

---

## Pool configuration

Pools are defined in the API server TOML configuration file under `[pools.<name>]`.

### Schema

```toml
[pools.<name>]
type = "integer"          # required; must be "integer" for VNI pools
ranges = [
  { start = "<value>", end = "<value>" },
  { start = "<value>", end = "<value>", auto_assign = false },
]
```

Each pool entry accepts the following fields.

**`type`** — required. For VNI pools, this must be `"integer"`. The `ipv4` pool type is used for
IP address pools and is not relevant here.

**`ranges`** — a list of range objects. Each range has:

- `start` — the first integer value in the range (inclusive).
- `end` — the last integer value in the range (exclusive). The value at `end` is not included in
  the pool.
- `auto_assign` — optional boolean, defaults to `true`. When `true`, values in this range are
  eligible for automatic allocation. When `false`, values are reserved for explicit requests only.
  Explicit requests are not used in standard VPC creation, so ranges with `auto_assign = false`
  are not drawn from during normal operation.

Multiple ranges may be provided for a single pool. The API server treats them as a single logical
pool. Ranges within a pool must not overlap, and a pool's ranges must not overlap with the ranges
of any other pool.

### Example configuration

The following is a representative configuration for a site that uses both internal and external
VPCs.

```toml
[pools.vpc-vni]
type = "integer"
ranges = [{ start = "2024500", end = "2025500" }]

[pools.external-vpc-vni]
type = "integer"
ranges = [{ start = "3024500", end = "3025500" }]
```

This example defines 1,000 VNIs in each pool. Adjust the ranges to match your site's planned VPC
counts plus appropriate headroom.

For development and test environments, much smaller ranges are sufficient:

```toml
[pools.vpc-vni]
type = "integer"
ranges = [{ start = "2024500", end = "2024550" }]
```

---

## Sizing pools

Each VPC consumes exactly one VNI for its entire lifetime. Pool size is therefore equal to the
maximum number of simultaneously active VPCs of that type that the site must support.

Use the following approach to determine the required pool size for each pool.

1. Estimate the maximum number of simultaneously active VPCs of each type (internal or external).
2. Add headroom. A margin of 10–20% is recommended to allow for burst creation without triggering
   emergency pool-grow operations.
3. Coordinate the resulting ranges with the network team. VNI values map directly to BGP
   route-targets, so the network team must configure import and export policies that reference the
   same ranges you define in the pool.

The `docs/getting-started/prerequisites/network.md` page states the general rule: one VNI is
required per expected VPC. The pools defined here are the mechanism that enforces and tracks that
allocation.

**Pool exhaustion.** When a pool is exhausted, VPC creation requests that would draw from that
pool fail immediately with a resource-exhausted error. No partial allocations occur. The only
recovery is to grow the pool (see the section below) and retry the creation.

The API server enforces a maximum pool size of 250,000 values per pool. VNI pools are 24-bit integers, so the theoretical maximum
VNI value is 16,777,215, but the enforced maximum pool size is 250,000 values per pool regardless
of the value range.

---

## Startup behavior

At startup, the API server reads the `[pools.*]` entries from the configuration file and writes
or updates the pool definitions in the database.

The behavior when a pool already exists in the database is additive: existing entries are not
removed or changed, and new values from the configuration are inserted. This means:

- Adding a new range to an existing pool is safe. The new values are added to the database on the
  next restart.
- Removing a range from the configuration does not remove values from the database. Previously
  defined values remain allocated or available in the pool.
- Shrinking a range (moving `start` up or `end` down) has no effect on values already in the
  database.

The practical consequence is that pools can only grow, never shrink, through configuration changes
alone. To reduce a pool, a manual database operation would be required. This is intentional: it
prevents accidental deallocation of VNIs that may be in active use.

When `listen_only = true` is set in the configuration, the API server does not register pool definitions at startup. It reads pool state from the
database only, on the assumption that
another instance has already populated the pools. Pool changes in this mode must be applied using
the `admin-cli resource-pool grow` command described below.

---

## Runtime operations

### Listing pools

To inspect the current state of all resource pools:

```
admin-cli resource-pool list
```

This queries the API server for the current state of all pools. The response includes the
following fields for each pool.

| Field | Description |
|---|---|
| Name | The pool name, matching the key in the configuration file |
| Min | The lowest value currently in the pool (allocated or free) |
| Max | The highest value currently in the pool (allocated or free) |
| Size | Total number of values in the pool |
| Used | Number of allocated values, followed by the percentage of total used |

Sample output:

```
+---------------------+---------+---------+------+----------+
| Name                | Min     | Max     | Size | Used     |
+---------------------+---------+---------+------+----------+
| external-vpc-vni    | 3024500 | 3025499 | 1000 | 12 (1%)  |
| vpc-vni             | 2024500 | 2025499 | 1000 | 47 (5%)  |
+---------------------+---------+---------+------+----------+
```

Monitor the `Used` column. When it approaches 100%, VPC creation for that pool will begin
failing. Plan pool-grow operations before the pool is exhausted rather than after.

### Growing a pool

To add capacity to an existing pool at runtime without restarting the API server:

```
admin-cli resource-pool grow -f <toml-file>
```

The argument to `-f` is the path to a TOML file containing the updated pool definition.

#### Constraints

- **Ranges can only be extended, not reduced.** Attempting to remove values that are already in
  the database has no effect. The grow operation is strictly additive.
- **New ranges must not overlap existing pool ranges.** Adding a range whose values are already
  present in the pool for a different pool name will result in a conflict error.
- **The pool name must already exist in the database.** The grow operation adds values to an
  existing pool; it cannot create a new pool name that was never defined in the configuration.

#### Example

To add 500 more VNIs to the internal VPC pool, create a TOML file containing the new range:

```toml
[pools.vpc-vni]
type = "integer"
ranges = [{ start = "2025500", end = "2026000" }]
```

Then run:

```
admin-cli resource-pool grow -f grow-vpc-vni.toml
```

The server will insert values 2025500 through 2025999 into the `vpc-vni` pool. The previously
defined range (2024500–2025499) is not affected.

After the grow operation completes, run `admin-cli resource-pool list` to confirm the new size
is reflected in the output.

> **Network team coordination required.** Before growing a VNI pool, confirm with the network
> team that the new VNI range is covered by their route-target import and export policies. VNI
> values that fall outside the network's configured policy range will result in VPCs that have no
> external connectivity even though they are created successfully by the API.

---

## Relationship to routing profiles

The routing profile assigned to a VPC determines which pool its VNI is drawn from. Specifically,
the `internal` field of the resolved routing profile controls pool selection.

- If `internal = true`, the VNI is allocated from `vpc-vni`.
- If `internal = false`, the VNI is allocated from `external-vpc-vni`.

This means that the VNI pool ranges implicitly define the route-target range for each profile
class. Internal VPCs will have native route-targets in the form `<asn>:<value-from-vpc-vni>`.
External VPCs will have native route-targets in the form
`<asn>:<value-from-external-vpc-vni>`. The network team must configure their EVPN policies
accordingly.

Because VNI allocation is tied to the routing profile at creation time, the routing profile of a
VPC cannot be changed after it is created. Changing it would require releasing the VNI and
reallocating from the other pool, which is not supported. If a VPC needs a different routing
profile, it must be deleted and recreated.

For full details on how routing profiles are configured and resolved, see
`docs/manuals/vpc/vpc_routing_profiles.md`.

### Simplifying network team coordination

A VPC's native route-target (`<datacenter_asn>:<vpc_vni>`) is unique per VPC. Network devices
that need to import routes from multiple VPCs must either track each VNI individually or import a
covering aggregate. Either approach requires ongoing coordination as VPCs are created and
destroyed.

`route_targets_on_exports` in the routing profile avoids this. Route-targets listed there are
applied to every route the VPC advertises, in addition to the native VNI-based tag. Because all
VPCs sharing the same routing profile carry the same additional export tags, the network team can
configure a single import policy that covers all VPCs in that profile class — regardless of how
many VPCs exist or what their individual VNIs are. When a new VPC is created under the same
profile, its routes are immediately visible to the existing network policy without any
reconfiguration.

A conventional deployment adds a shared export tag to the `INTERNAL` profile (for example,
`{ asn = <datacenter_asn>, vni = 50200 }`) and configures network devices to import `:50200`.
All internal VPC routes then become reachable from the fabric under that single import, without
tracking individual VNIs.

See [VPC Routing Profiles](vpc_routing_profiles.md) for the `route_targets_on_exports` field
reference.

---

## Troubleshooting

### VPC creation fails with a resource-exhausted error

The pool that would have served the allocation is empty. The VNI pool selected depends on the
routing profile of the VPC being created.

1. Run `admin-cli resource-pool list` and identify which pool is at or near 100% used.
2. Coordinate a new range with the network team.
3. Run `admin-cli resource-pool grow -f <file>` with the new range.
4. Retry the VPC creation.

If the pool appears to have free capacity but creation is still failing, verify that the routing
profile is being resolved as expected. If the VPC's routing profile resolves to `internal = false`
but only the `vpc-vni` pool was grown, the growth will not help.

### The external-vpc-vni pool is missing or full at startup

The `external-vpc-vni` pool is optional. If the site has no external VPCs, the pool does not need
to be defined. However, if the API server is started without this pool and a request arrives for
an external VPC, the request will fail.

If you need to add this pool after the site is already running, add the definition to the
configuration file and restart the server, or use `admin-cli resource-pool grow -f <file>` to
populate the pool without a restart.

### Pool size appears smaller than expected after a config change

Reducing or removing a range in the configuration file has no effect at startup. Only new values
are inserted. Verify the effective pool state with `admin-cli resource-pool list`, not the
configuration file alone.
