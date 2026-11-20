# Expected Inventory

Expected inventory declares the hardware NICo should discover and manage. It is
not a report of discovered hardware or confirmation that a device is ready.
Use it to reconcile DCIM records with NICo before discovery, and to maintain
device identity and placement as equipment changes.

## Resources and relationships

All five resources are scoped to a Site. The linked API references define their
request fields, validation constraints, permissions, and responses.

| Resource | Purpose | Identity supplied by the operator |
| --- | --- | --- |
| [Expected Machine](api:POST/v2/org/:org/nico/expected-machine) | Declare a compute host, its management access, and optional rack placement. | BMC MAC address and chassis serial number. |
| [Expected Switch](api:POST/v2/org/:org/nico/expected-switch) | Declare a switch, its BMC and NVOS management information, and optional rack placement. | BMC MAC address and switch serial number. |
| [Expected Power Shelf](api:POST/v2/org/:org/nico/expected-power-shelf) | Declare a power shelf, its management access, and optional rack placement. | BMC MAC address and shelf serial number. |
| [Expected Rack](api:POST/v2/org/:org/nico/expected-rack) | Declare a physical rack and its descriptive metadata. | Site-unique external rack identifier. |
| [Expected Rack Group](api:POST/v2/org/:org/nico/expected-rack-group) | Declare topology and the racks and devices belonging to a group. | Site-unique external group identifier. |

Rack groups describe membership; they do not create the individual machine,
switch, power-shelf, or rack records. Each group member has a component type,
manufacturer, opaque external device identifier, and required `cartridgeIndex`
(an integer from 0 through 4294967295). Legacy records can return a null index;
new create or replacement requests and PATCH requests supplying `racks` must
provide it for every member. A PATCH omitting `racks` preserves membership.
Preserve the declared rack and member ordering and use consistent identities
across DCIM records. A group cannot contain duplicate rack IDs or duplicate
device identity tuples.

Component rack placement is optional, so non-rack hosts do not need a synthetic
rack or group. For rack-managed equipment, keep the component's rack association
and physical placement consistent with the group declaration. The REST record
UUID is distinct from an external rack/group identifier, a BMC MAC address, or
a hardware serial number; use the identifier required by each endpoint.

### NVLink Domain declarations

Core NVLink Domain declarations claim tray positions within a RackGroup. They
are managed through the [NVLink Domain CLI](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/nvlink-domain/nvlink-domain.md),
separately from expected inventory. A declaration requires an existing group
and nonempty, distinct `rack_index:slot_index:component_type` positions; indexes
are unsigned 32-bit integers and the component type is case-sensitive `Compute`
or `Switch`. Creation does not resolve those positions against RackGroup
members or configure hardware.

While a declaration references a group, the group's `racks` field cannot change
and the group cannot be deleted. Delete that group's declarations before either
operation. Core also blocks site-wide RackGroup replacement or deletion while
any declaration exists, even if a replacement would retain the referenced
group. Delete all declarations for the Site before a site-wide operation.
Deleting a declaration releases its position claims without operating hardware.

## Prepare and register inventory

1. Select the Site and confirm API access and site connectivity. Create and
   replacement operations require a registered Site. Prepare a complete DCIM
   source for each resource type you intend to reconcile.
2. Collect BMC MAC addresses, the appropriate serial numbers, management
   credentials, and physical placement. Include switch NVOS management
   information where applicable. Follow each create-request schema rather than
   using the same payload for all component types.
3. For rack-managed equipment, create the Expected Rack Groups before their
   Expected Racks. Core requires each rack to belong to exactly one declared
   group, and its derived rack profile must be configured in Core. Then register
   the machines, switches, and power shelves associated with those racks.
   Register standalone machines without rack placement.
4. Read back the expected records and check their identities and associations
   against DCIM. A successful write confirms the inventory operation, not
   discovery or readiness of the hardware.

Keep credentials in a protected source and supply them through the supported
request fields. Expected-inventory REST responses do not return BMC or NVOS
passwords, so a GET export alone is not a complete recovery payload. Do not
assume an omitted credential will survive a full replacement.

See [Ingesting Hosts (REST API)](ingesting-hosts-rest-api.md) for host onboarding,
[Credential Sources](../configuration/credential-sources.md) for discovery
credentials, and [Configure Expected Machine Interfaces](expected-machine-interfaces.md)
for host interface declarations.

## Incremental changes and full replacement

Use the resource's individual create, update, and delete endpoints for additions
or corrections to a subset of inventory. PATCH omission and clearing rules are
field-specific; consult the update schema. Expected Machine also supports
[batch creation](api:POST/v2/org/:org/nico/expected-machine/batch) and
[batch updates](api:PATCH/v2/org/:org/nico/expected-machine/batch), which are
distinct from full-site replacement.

Use full replacement only when the submitted set is the complete desired
inventory for one resource type in one Site. Each request supplies a required
top-level `siteId` UUID and an array of create-request objects; every entry must
carry the same `siteId`. Entries not included in that set are deleted, and an
explicit empty array clears the resource's expected records for the Site.

Paths below are relative to `/v2/org/{org}/nico`:

| Resource | Replacement endpoint | Array field |
| --- | --- | --- |
| Machine | [PUT /expected-machine/all](api:PUT/v2/org/:org/nico/expected-machine/all) | `expectedMachines` |
| Switch | [PUT /expected-switch/all](api:PUT/v2/org/:org/nico/expected-switch/all) | `expectedSwitches` |
| Power Shelf | [PUT /expected-power-shelf/all](api:PUT/v2/org/:org/nico/expected-power-shelf/all) | `expectedPowerShelves` |
| Rack | [PUT /expected-rack/all](api:PUT/v2/org/:org/nico/expected-rack/all) | `expectedRacks` |
| Rack Group | [PUT /expected-rack-group/all](api:PUT/v2/org/:org/nico/expected-rack-group/all) | `expectedRackGroups` |

Always supply the array explicitly. The rack endpoint also treats a missing or
`null` `expectedRacks` as empty; the other four endpoints reject missing or
`null` arrays. Null entries within an array are rejected.

Machine, switch, and power-shelf replacements accept at most 100 entries per
request. Do not split a complete set into successive replacement calls: each
call removes entries absent from that call, including entries from earlier
calls. Use incremental operations for sets larger than the replacement limit.

Resource types are updated through separate requests, not one cross-resource
transaction. Apply group declarations before racks, verify each response, and
read back the affected records after an error before deciding what to retry.
Use the `/all` routes for new integrations; root-level rack and rack-group PUT
routes are deprecated compatibility aliases.

Read back record UUIDs after a full replacement; do not treat it as an in-place
PATCH of existing records.

## Discovery and verification

NICo discovers and explores reachable management endpoints and associates them
with expected records. Correct inventory does not replace working management
networking, valid credentials, or supported hardware. Flow synchronizes expected
inventory and discovered state separately, so allow synchronization to complete
before treating a missing device as a permanent discrepancy.

Verify both the declaration and the observed result:

- Read the expected resource to confirm what NICo was told to manage.
- Check discovered machines or the [rack and tray inventory](../manuals/rack_level_admin.md#rest-api)
  to confirm which hardware is present. Rack/tray validation reports missing,
  unexpected, and mismatched components; inspect the reported differences
  rather than relying only on a total count.
- For a missing device, check the BMC identity, management-network reachability,
  and discovery credentials. For a mismatch, compare serial numbers and physical
  placement with DCIM before correcting the declaration or the installation.
- Check ingestion and health independently. Inventory agreement does not imply
  hardware health or completion of [rack bringup](../manuals/rack_level_admin.md#rack-level-operations).

See [Monitoring and Health](../operations/monitoring-health.md) for health
snapshots and [host onboarding troubleshooting](ingesting-hosts-rest-api.md#troubleshooting)
for discovery and ingestion failures.

## Removal and recovery

Use individual deletion for a selected expected record. To clear a resource
type for an entire Site, send DELETE to its `/all` path with the required
`siteId` UUID query parameter. Removing records through replacement has the
same scope implications: an incomplete source can unintentionally remove valid
expectations.

Expected-inventory deletion is not a hardware decommission, power-off, or wipe
request. Flow reconciles removed expectations, but removal is not evidence that
the physical device has stopped running. Follow the
[decommissioning procedures](../decommissioning/index.md) for hardware retirement,
separately from inventory cleanup.

For accidental removal, restore the intended declarations and credentials from
the authoritative source, using incremental operations or a complete replacement
set. Restore groups before dependent racks, then verify component records,
discovery, and health again. Read back REST identifiers after recreation rather
than assuming an old record UUID can be reused.
