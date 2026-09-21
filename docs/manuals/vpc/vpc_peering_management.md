# VPC Peering

VPC peering allows you to connect two VPCs together, enabling bi-directional network communication between instances in different VPCs. This page explains how to manage VPC peering connections using `nico-admin-cli`.

## Virtualized-to-Flat Routing Prerequisite

An active Virtualized-to-Flat peering imports the Flat VPC's native route target into the FNN VRF. It does not create or advertise a route for the Flat VPC's prefixes.

Prepare routing before any of these operations:

- Upgrading an agent that serves an active Virtualized-to-Flat peering to a release that uses null-route isolation.
- Creating a new Virtualized-to-Flat peering on an agent that uses null-route isolation.
- Activating a stored Virtualized-to-Flat peering by enabling `vpc_peering_policy_on_existing`.

The prerequisite applies when both of these conditions hold:

- `vpc_isolation_behavior` is `mutual_isolation`, and an affected Flat prefix is contained by an effective `site_fabric_null_routes` prefix.
- The tenant VRF reaches that Flat prefix only through a less-specific underlay route, such as a leaked default route.

Before creating, activating, or upgrading the affected peering, prepare a route for every affected Flat prefix by using one of these methods:

- Have the fabric advertise the Flat route through EVPN with the Flat VPC's native route target, `<datacenter_asn>:<flat_vni>`. The FNN peering import admits that route into the FNN VRF after the peering exists and its activation policy permits the import.
- Ensure the Flat route is present in the underlay/default VRF and explicitly admit it through the tenant VPC's effective [`accepted_leaks_from_underlay`](vpc_network_virtualization.md#accepted_leaks_from_underlay) list.

The imported route must be at least as specific as the containing FNN blackhole. A leaked default route is insufficient when the blackhole is more specific than `/0`, because longest-prefix selection chooses the blackhole before administrative distance is compared. Do not combine an effective `0.0.0.0/0` or `::/0` null route with `leak_default_route_from_underlay = true` for the same address family. Core accepts this unsupported combination, but the leaked default has a better administrative distance than the equal-prefix distance-250 blackhole and overrides it.

## VPC Peering Commands

The `nico-admin-cli vpc-peering` command provides three main operations:

```bash
nico-admin-cli vpc-peering <COMMAND>

Commands:
  create  Create VPC peering connection
  show    Show list of VPC peering connections
  delete  Delete VPC peering connection
```

### Creating VPC Peering Connections

To create a new VPC peering connection between two VPCs:

```bash
nico-admin-cli vpc-peering create <VPC1_ID> <VPC2_ID>
```

**Example:**
```bash
nico-admin-cli vpc-peering create e65a9d69-39d2-4872-a53e-e5cb87c84e75 366de82e-1113-40dd-830a-a15711d54ef1
```

**Notes:**
- The operator should confirm with both VPC owners (VPC tenant org) that they approve the peering before creating the connection
- The VPC IDs can be provided in any order
- The system will automatically enforce canonical ordering (smaller ID becomes `vpc1_id`)
- If a peering connection already exists between the two VPCs, the command will return an error indicating a peering connection already exists
- Both VPCs must exist before creating the peering connection
- For a Virtualized-to-Flat peering, follow the [routing preparation and verification steps](#virtualized-to-flat-routing-prerequisite)

### Listing VPC Peering Connections

To view VPC peering connections, you can either show all connections or filter by a specific VPC:

**Show all peering connections:**
```bash
nico-admin-cli vpc-peering show
```

**Show peering connections for a specific VPC:**
```bash
nico-admin-cli vpc-peering show --vpc-id <VPC_ID>
```

**Example:**
```bash
# Show all peering connections
nico-admin-cli vpc-peering show

# Show peering connections for a specific VPC
nico-admin-cli vpc-peering show --vpc-id 550e8400-e29b-41d4-a716-446655440000
```

The output will display:
- Peering connection ID
- VPC1 ID (smaller UUID)
- VPC2 ID (larger UUID)
- Connection status
- Creation timestamp

### Deleting VPC Peering Connections

To delete an existing VPC peering connection:

```bash
nico-admin-cli vpc-peering delete --id <PEERING_CONNECTION_ID>
```

**Example:**
```bash
nico-admin-cli vpc-peering delete --id 123e4567-e89b-12d3-a456-426614174000
```

**Notes:**
- You need the peering connection ID (not the VPC IDs) to delete a connection
- Use the `show` command to find the peering connection ID
