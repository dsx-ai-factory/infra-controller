# Configuring InfiniBand Partitions

This page is the Day-1 configuration guide for InfiniBand partitions in
NICo. It describes how an operator points NICo at UFM, how partitions are
allocated and assigned to tenant instances, and how to verify that a host
has ended up in the partitions it should. Tenant isolation is a property
of how partitions are assigned — for the cross-fabric isolation picture,
see [Network Isolation](../network_isolation.md).

The InfiniBand fabric itself — UFM installation, `gv.cfg` / `opensm.conf`
tuning, M_Key and SA_Key configuration, and static topology files — is
covered separately in the [InfiniBand Setup
runbook](../../playbooks/ib_runbook.md). That runbook is a prerequisite for
this page: NICo's partition guarantees rest on a properly hardened UFM and
subnet manager.

Core is NICo's site-local API server. The tenant-facing REST service forwards
requests to Core and maintains its own inventory records. Core's partition create
handler allocates P_Keys. `IbFabricMonitor` reconciles host-port membership in UFM.
The partition controller reconciles partition QoS and releases the P_Key
reservation when it removes the partition record.

**Related pages**

- [Network Isolation](../network_isolation.md) — cross-fabric isolation
  overview; explains how IB partitions fit alongside Ethernet and NVLink
- [InfiniBand Setup Runbook](../../playbooks/ib_runbook.md) — UFM and OpenSM
  hardening (prerequisite)
- [InfiniBand NIC and Port Selection](../../architecture/infiniband/nic_selection.md)
  — how NICo picks which host NICs / ports are managed
- [Networking Integrations](../../architecture/networking_integrations.md) —
  shared architectural patterns across all three fabrics

---

## Required Core Support

Core rejects requested references to partitions marked for deletion, including
references retained on unchanged interfaces within a changed IB configuration.
That admission rule and the controller's locked final recount require the
changes in [PR 8774](https://github.com/dsx-ai-factory/infra-controller/pull/8774)
for [issue 5313](https://github.com/dsx-ai-factory/infra-controller/issues/5313).

[Commit 06ba76f3](https://github.com/dsx-ai-factory/infra-controller/commit/06ba76f37241df93f415634f80fb29065d63bad6)
identifies the implementation source, not a shipping release.

Operators must confirm that the deployed build includes PR 8774's changes using
its release information, merged-change reference, or source. The linked PR-head
commit records provenance; inclusion does not require that exact commit to
appear in the deployed build's history. If support is absent or cannot be
confirmed, contact the maintainer of your Core deployment before requesting
partition deletion. Tenants must ask their site operator to confirm this
prerequisite.

---

## The Partition Model

InfiniBand partitioning in NICo is built on the InfiniBand-native P_Key
mechanism enforced by the subnet manager. The operator-facing chain is:

```text
Instance ──► IB Interface ──► IbPartition (NICo) ──► P_Key (UFM)
```

Read it top to bottom:

1. A **tenant instance** has one or more **IB interfaces**, one per
   InfiniBand port on the host the instance is allocated to.
2. Each IB interface references exactly one **`IbPartition`** by ID — this
   is the NICo object the tenant manipulates.
3. The `IbPartition` corresponds to a single **P_Key** on UFM. Core allocates
   the P_Key from its configured pool for tenant REST requests.
4. Enforcement happens at the **subnet manager**. A host port that is not
   a member of a P_Key cannot exchange InfiniBand traffic with any other
   member of that P_Key, regardless of physical connectivity. This is
   fabric-side, not host-side: a misconfigured host cannot bypass it.

Two instances in different P_Keys cannot send any IB traffic to each other;
two instances in the same P_Key can. There is no operator-visible "peering"
concept for InfiniBand the way there is for Ethernet VPCs — sharing requires
placing both instances' interfaces in the same partition.

---

## What Lives Where

| Concern | Location |
|---|---|
| Subnet manager, M_Key / SA_Key hardening, static topology | UFM host (see the IB runbook) |
| UFM endpoint URL(s) and managed P_Key ranges | NICo API server TOML, under `[ib_fabrics.default]` |
| Fabric-wide toggles (enable, MTU, rate limit, service level, monitor cadence) | NICo API server TOML, under `[ib_config]` |
| UFM API credentials | Vault (or the configured secrets backend), not the TOML file |
| `IbPartition` objects (tenant-owned) | NICo database, managed by tenants via the REST API / `nicocli` |
| Per-interface partition assignment | `InstanceInfinibandConfig` on the instance, set via an instance update |
| Reconciliation between desired and actual UFM state | `IbFabricMonitor`, a background task inside the API server |

NICo treats UFM as the authoritative source for **observed** fabric state.
It does not cache UFM partition membership separately from what the monitor
last read. This means that direct out-of-band changes to UFM (an operator
editing partitions in the UFM UI, for example) will be detected on the next
monitor iteration and reconciled back to NICo's intended state.

---

## Operations: Who Does What

InfiniBand splits cleanly between operator site setup and tenant partition
management. See
[Network Isolation → Who configures what, and how](../network_isolation.md#who-configures-what-and-how)
for the role and interface model.

| Task | Role | Interface |
|---|---|---|
| UFM endpoint(s), P_Key ranges, fabric toggles | Operator | **TOML** (`[ib_fabrics.default]`, `[ib_config]`) — Day 0 / rare |
| UFM API credentials | Operator | Secrets backend (Vault) |
| Create / update / delete an InfiniBand partition | Tenant | **REST** `…/nico/infiniband-partition` · `nicocli infiniband-partition create` |
| Assign an instance's IB interface to a partition | Tenant | **REST** `…/nico/instance` (update) · `nicocli instance update` |
| List partitions and check sync state for triage | Operator | `nicocli infiniband-partition list`; [`nico-admin-cli ib-partition show`](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/ib-partition/ib-partition-show.md) for deeper state |
| Break-glass: unbind a host from UFM out of band | Operator | **`nico-admin-cli`** (gRPC) — not exposed via REST |

The UFM-facing setup (the first two rows) is the operator's responsibility and
is described in [Configuring NICo to Talk to UFM](#configuring-nico-to-talk-to-ufm).
Everything a tenant does — creating partitions and attaching interfaces — goes
through the REST API or `nicocli`; the gRPC `nico-admin-cli` rows are operator
triage and break-glass paths that the REST API does not expose.

---

## Configuring NICo to Talk to UFM

Two TOML blocks are involved. Both live in the API server's config file.

### `[ib_fabrics.default]` — Endpoints and P_Key Pool

When InfiniBand is enabled, NICo supports at most one configured fabric, named
`default`. Tenant P_Key allocation uses this fabric's pool.

```toml
[ib_fabrics.default]
endpoints = ["https://ufm-a.example.internal:443"]

  [[ib_fabrics.default.pkeys]]
  start = "0x100"
  end   = "0x1ff"

  [[ib_fabrics.default.pkeys]]
  start = "0x300"
  end   = "0x3ff"
```

Fields:

| Field | Purpose |
|---|---|
| `endpoints` | UFM server URLs. At the time of writing only the first endpoint is consulted; multi-endpoint support exists in the schema for forward compatibility |
| `pkeys` | One or more P_Key ranges, hex-encoded, with an inclusive start and an exclusive end. These define the pool from which Core allocates P_Keys for tenant REST requests |

P_Key ranges may be **extended in future restarts but never shrunk**:
removing or narrowing a range under live tenants would orphan allocated
partitions. Plan the pool with headroom.

### `[ib_config]` — Fabric Toggles

```toml
[ib_config]
enabled = true
allow_insecure = false
mtu = 4 # 4k; accepted values are 2 (2k) and 4 (4k)
rate_limit = 200
service_level = 0
fabric_monitor_run_interval = "60s"
```

| Field | Purpose |
|---|---|
| `enabled` | Master switch. With `false`, NICo will not call UFM and will not run the monitor |
| `allow_insecure` | Permit TLS to UFM without certificate verification. Intended for development only |
| `mtu`, `rate_limit`, `service_level` | Settings applied to NICo-managed partitions |
| `fabric_monitor_run_interval` | Steady-state cadence for `IbFabricMonitor`. Defaults to 60 seconds |

### UFM credentials

UFM API credentials are not stored in TOML. They are read from the
configured secrets backend (Vault under standard deployments) by the UFM
client during initialisation. Rotate them at the secrets backend; NICo
picks up the new value on its next client re-initialisation.

---

## P_Key Allocation

When a tenant creates an InfiniBand partition through REST
(`POST …/nico/infiniband-partition`), Core allocates its P_Key from the
configured pool. The REST create request exposes `name`, `description`,
`siteId`, and `labels`. It does not expose a requested P_Key.

The model's `IbPartition` object retains the allocated P_Key for the
lifetime of the partition. There is no "renumber" operation; to change a
P_Key, create a new partition, move the instances' IB interfaces to it, and
wait for convergence and cleanup before deleting the old partition.

The REST partition update exposes only `name`, `description`, and `labels`;
MTU and rate limit are not tenant REST update controls. Refer to
[Deleting a Partition](#deleting-a-partition) for REST and Core reference checks,
concurrent requests, and final removal.

---

## Membership: Full vs Limited

UFM distinguishes **full** and **limited** P_Key membership. Full members
can communicate with both full and limited members of the same P_Key;
limited members can only talk to full members.

NICo's posture:

- The `IbPortMembership` enum is **read-only** from NICo's perspective.
  The monitor records whatever UFM reports for each port-in-partition
  binding.
- NICo does **not** expose a config option to choose full or limited
  membership when creating a partition. Whatever UFM is configured to use
  (typically full members, with the default partition restricted by the
  `default_membership = limited` hardening in the IB runbook) is what
  appears on a NICo-managed binding.
- The monitor flags a security alert if the default partition shows full
  membership on a NICo-managed port; that is an indication the UFM
  hardening described in the runbook has not been applied.

---

## The `IbFabricMonitor`

`IbFabricMonitor` is the background reconciler inside the API server.
Every iteration it:

1. Reads UFM state: port information (state, GUID, LID), partition
   membership lists, fabric version, M_Key / SM_Key / SA_Key configuration,
   and default-partition membership.
2. Compares observed UFM state to the desired state implied by each
   instance's `InstanceInfinibandConfig`.
3. For each IB interface in an instance config, if the host GUID is not
   already a member of the expected P_Key, calls `bind_ib_ports()` to add
   it.
4. For any GUID found in a NICo-managed P_Key that no longer matches a
   live instance config, calls `unbind_ib_ports()` to remove it.
5. Updates per-machine InfiniBand status observations in Core. REST inventory
   uses Core's IB synchronization status to update the corresponding instance
   interface records.

Cadence is set by `fabric_monitor_run_interval` (default 60 seconds). After
applying any UFM changes, the monitor accelerates the next iteration to
~1 second so that convergence shows up quickly in observed state. Once a
steady iteration completes with no changes, the monitor returns to the
configured interval.

The monitor exposes metrics under the `nico_ib_monitor_*` namespace:

| Metric | Use |
|---|---|
| `nico_ib_monitor_iteration_latency` | Time per reconcile pass; a sudden rise indicates UFM slowness |
| `nico_ib_monitor_ufm_changes_applied` | Counter of bind/unbind operations issued; nonzero in steady state is an anomaly |
| `nico_ib_monitor_machines_by_port_state_count` | Port-state histogram; helps spot hosts stuck `Initialize` or `Down` |
| `nico_ib_monitor_machines_with_missing_pkeys_count` | Hosts that should be in a partition but are not — investigate |
| `nico_ib_monitor_machines_with_unexpected_pkeys_count` | Hosts that are in partitions they should not be — investigate immediately |
| `nico_ib_monitor_ufm_partitions_count` | Partition count UFM reports; sanity check against NICo's view |
| UFM-error counters | Connection / authentication failures; the schema for these is acknowledged as incomplete in the current code |

---

## How a Tenant Ends Up in a Partition

For a tenant instance with an IB interface attached to a partition:

1. The tenant updates the instance (REST `PATCH …/nico/instance`, or
   `nicocli instance update`) with an InfiniBand interface configuration
   that references the desired partition ID for each IB port.
2. For each supplied interface, after parsing its partition ID, REST checks that
   the partition exists, matches the instance's site, belongs to the tenant, and
   has status `Ready` with a non-null `controllerIBPartitionId`, in that order.
   Core applies the admission rules below before storing a changed IB
   configuration. Other request validation can fail before these partition
   checks.
3. The next `IbFabricMonitor` iteration observes the new desired state
   and issues `bind_ib_ports()` for each host GUID that is not already a
   member of the expected P_Key.
4. UFM updates partition membership. On the following iteration the
   monitor reads back the new membership and updates the machine's IB
   status observation.
5. After Core reports IB synchronization, REST inventory marks the matching
   entries in the instance's `infinibandInterfaces` list with status `Ready`.
   The instance's overall REST `status` also depends on other readiness checks.

An existing instance can report REST status `Configuring` while updated
networking converges. Initial allocation can remain `Provisioning` while Core
waits for network configuration. Refer to [Deleting a Partition](#deleting-a-partition)
for the REST fields used to verify a retargeting operation.

### Partition Admission Rules and REST Limitations

On a Core build meeting [Required Core Support](#required-core-support),
allocation and changed IB configuration require every requested partition to
exist, belong to the tenant, and not be marked for deletion. Core checks all
requested partitions, including references retained on unchanged interfaces.
Core permits removing marked references because it validates only the requested
references. IB changes require Core's managed-host state `Assigned` with instance
state `Ready`. This internal state can coexist with REST status `Configuring`
while configuration synchronization is pending. Terminating instances cannot be
updated.

Core permits an existing marked reference when the requested IB configuration
remains identical, including interface order. It ignores the stored, automatically
assigned `guid` and `pf_guid` fields in this comparison. This exception can allow a
metadata update. A supplied REST partition must still pass REST's readiness
checks. Core can reject a reconstructed configuration that differs and retains a
marked partition; REST relays that error.

REST reconstructs the Core IB configuration from its inventory, excluding
interface records marked `Deleting`. Omitting `infinibandInterfaces` skips the
REST interface-replacement block; it does not guarantee that the reconstructed
configuration equals Core's stored configuration. Core rechecks partitions if
the tenant-configurable IB configuration differs. Every partition in a supplied
nonempty REST list must pass step 2, even for a repeated configuration.

Do not use an empty list or a subset of unchanged interfaces as a REST detach
procedure. To stop using a partition, release the instance or retarget its
interfaces to another eligible `Ready` partition, then verify the result using
[Deleting a Partition](#deleting-a-partition).

The
[REST instance update handler at f20f65a8](https://github.com/dsx-ai-factory/infra-controller/blob/f20f65a8040198bc9da17f25f6eb632559c31e71/rest-api/api/pkg/api/handler/instance.go#L3998)
reads at most 20 existing interface records, oldest first. Old `Deleting`
records can occupy that internal page and omit newer active records from the
configuration sent to Core. When the handler reissues interfaces, it also marks
only the fetched old records `Deleting`. Newer active records outside that page
can remain and block partition deletion. This is an implementation limitation,
not a supported limit on interfaces. Do not assume that an omitted or repeated
list preserves all assignments. Verify observed membership and involve your site
operator if REST inventory and Core configuration disagree.

---

## Force-Delete and Cleanup

When an instance is released, it keeps its stored IB configuration until its
record is removed. `IbFabricMonitor` unbinds the host GUIDs from UFM during
termination. Host force-deletion also triggers UFM membership cleanup.

During termination, missing or incomplete configured-port GUID entries within an
existing IB status observation produce `PortStateUnobservable`. Core sets an
`IbCleanupPending` health alert and allows termination to proceed while
`IbFabricMonitor` handles cleanup. If the entire IB observation is missing, Core
waits instead. Other IB isolation mismatches also keep termination waiting.

By default, `IbCleanupPending` blocks new allocations on that host while present,
including allocations to the same tenant.

An authorized targeted allocation can bypass this health guard. Refer to
[Claim the Machine](../repair/repair_tenant_workflow.md#claim-the-machine) for the
REST health override and its
[access requirements](../repair/repair_tenant_workflow.md#audience-and-access).
The override does not confirm UFM cleanup or bypass Core's other allocation
checks.

The monitor clears the alert when none of the GUIDs parsed from its message
appear in its missing, unexpected, or unknown P_Key discrepancy lists. That
condition can hold when observations or parsed GUIDs are missing. A cleared alert
alone does not confirm UFM membership removal.
Have the site operator inspect the host's UFM memberships if deferred cleanup
cannot be confirmed. The related Ethernet guard is described in
[Default Isolation: The Admin Overlay](../vpc/vpc_network_virtualization.md#default-isolation-the-admin-overlay).

## Deleting a Partition

There is no dedicated force-delete partition operation.

Use this tenant procedure after the operator confirms
[Required Core Support](#required-core-support):

1. Query [Retrieve all InfiniBand Interfaces](api:GET/v2/org/:org/nico/infiniband-interface)
   with `infinibandPartitionId` set to the partition ID. Read every page and
   inspect the returned `instanceId` and `status` values.
2. Release the referencing instances or retarget their IB interfaces to another
   tenant-owned, same-site `Ready` partition with a `controllerIBPartitionId`.
   Use [Retrieve Instance](api:GET/v2/org/:org/nico/instance/:instanceId) to follow
   the instance's `status` and `infinibandInterfaces`. For retargeting, confirm that
   each requested interface has the expected `partitionId` and status `Ready`.
   Overall instance status `Ready` also reflects other readiness checks. For
   release, wait for termination to finish.
3. Repeat the partition-filtered interface query after inventory cleanup. Read
   every page until no records are returned, including records with status
   `Deleting`. IB convergence alone does not remove those old records. If records
   persist after convergence or termination, ask the site operator to investigate
   inventory cleanup before proceeding.
4. Request [Delete InfiniBand Partition](api:DELETE/v2/org/:org/nico/infiniband-partition/:infiniBandPartitionId).
   REST rejects while interface records point to existing instances. Core also
   rejects while any instance references the partition, including terminating
   instances. An empty REST inventory result does not replace Core's check.
5. An accepted REST request sets status `Deleting`; it does not prove final
   removal. If rejected, use [Troubleshooting](#troubleshooting). If accepted but
   not removed, have the operator inspect Core and UFM using that section.
6. Have the operator inspect the same partition ID in the same site's Core through
   [`nico-admin-cli ib-partition show` reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/ib-partition/ib-partition-show.md).
   The detail view returns "Unknown InfiniBand Partition ID" after Core removes
   that record. Use that specific response as the Core removal signal; a
   connection or authentication failure does not establish removal.
7. REST removes its `Deleting` partition record after a reconciled Core inventory
   no longer reports it. Confirm that
   [Retrieve InfiniBand Partition](api:GET/v2/org/:org/nico/infiniband-partition/:infiniBandPartitionId)
   no longer finds the same partition. REST inventory removal can lag Core
   removal.

Core's precheck is best effort: a concurrent allocation or instance update that
changes the IB configuration can pass validation and commit before the partition
is marked for deletion. The mark persists until Core removes the partition
record; removing instance references does not clear it. During instance release,
IB changes, and force-deletion, NICo can still use the marked partition's stored
P_Key to remove UFM bindings. Release and IB-configuration updates skip exact
membership retirement for a referenced partition with no stored P_Key because
they cannot resolve its membership tuple. This does not unblock automatic final
partition deletion.

With [Required Core Support](#required-core-support), the partition controller waits while UFM
still reports the partition. After UFM reports it not found, the controller locks
the partition record and counts all
referencing instances, including terminating instances. If references remain, it
waits and repeats these checks on a later pass. After finding no references, it
removes the record and releases the P_Key in the same transaction as the locked
reference count.
This sequence requires UFM to return NotFound. It does not guarantee that UFM
removes an empty partition or one with bindings managed outside NICo.
For a partition that is not removed, follow the diagnostics in
[Troubleshooting](#troubleshooting).

---

## Configuration Workflow

### Operator (per site)

1. Stand up UFM per the [InfiniBand Setup runbook](../../playbooks/ib_runbook.md).
   Confirm `default_membership = limited`, M_Key / SA_Key hardening, and
   any required static topology configuration.
2. Provision a UFM API user with permission to read ports / partitions
   and to create / update / delete partitions. Store its credentials in
   the secrets backend.
3. In the NICo API server config:
   - Set `[ib_config].enabled = true` and any fabric-wide MTU / rate /
     service-level defaults.
   - Define `[ib_fabrics.default]` with the UFM endpoint(s) and one or more
     `pkeys` ranges. Size the ranges with room for future growth.
4. Restart the API server. The `IbFabricMonitor` begins its periodic
   reconciliation on the next tick.

### Tenant (per partition)

All tenant steps use the REST API or `nicocli`; none require TOML or
`nico-admin-cli`.

1. `nicocli infiniband-partition create` (REST `POST …/nico/infiniband-partition`)
   for each isolation domain the tenant needs (typically one per workload).
2. `nicocli instance update` (REST `PATCH …/nico/instance`) to attach each
   IB interface to the appropriate partition.
3. Use [Retrieve Instance](api:GET/v2/org/:org/nico/instance/:instanceId) to confirm
   that the requested `infinibandInterfaces` entries show the expected
   `partitionId` and status `Ready`. Inspect the instance's overall `status` for
   readiness beyond IB configuration.

---

## Verification

NICo does not ship a single "is IB healthy" command. Verification is a
short, repeatable checklist.

1. **UFM is reachable.** Check the API server log for "Failed to create
   UFM client" or similar startup errors. Confirm
   `nico_ib_monitor_iteration_latency` is being recorded (the monitor
   is running) and that UFM error counters are flat.
2. **Configured partitions exist and have converged.** List partitions with
   `nicocli infiniband-partition list` (REST `GET …/nico/infiniband-partition`)
   and confirm each is in a converged state. For deeper triage, use the
   [`nico-admin-cli ib-partition show` reference](https://github.com/dsx-ai-factory/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/ib-partition/ib-partition-show.md).
   Pass the partition ID as the `ID` argument to inspect `STATE`, `PKEY`, and
   `STATE MACHINE`. The single-partition view shows controller wait or error
   messages. Other results can display "OK" without proving deletion completed.
   Correlate these fields with the API server's controller logs.
3. **Host is a member of the expected partitions.** Inspect the
   machine's `infiniband_status_observation` via the machine debug
   tooling and confirm each managed port reports membership in the
   intended P_Key. Cross-check with the live UFM partition table for
   the same partition.
4. **No anomalies in the monitor metrics.**
   `nico_ib_monitor_machines_with_missing_pkeys_count` and
   `nico_ib_monitor_machines_with_unexpected_pkeys_count` should both
   be `0` in steady state. Either being non-zero is a divergence between
   intent and UFM state and warrants investigation.
5. **Check deferred cleanup.** By default, an `IbCleanupPending` alert blocks
   allocation while present. Its absence alone does not confirm that UFM
   memberships were removed. For hosts whose cleanup was deferred, have the
   operator inspect UFM
   membership as described in [Force-Delete and Cleanup](#force-delete-and-cleanup).

---

## Limitations Worth Knowing

The IB integration is in production but with the following gaps:

- **Single-endpoint UFM today.** Only the first entry in `endpoints` is
  used at runtime. UFM HA is handled by UFM itself (virtual IP / HA
  pair); the multi-endpoint config field exists for forward
  compatibility.
- **P_Key ranges are append-only in practice.** Restarting with a
  narrowed range under live partitions is unsafe; ranges should only
  grow.
- **No tenant-facing UFM-reachability probe.** Operators rely on
  metrics and log lines rather than a `ping`-style health command.
- **SHARP, index-0, and default-membership knobs are not yet wired.**
  The model has fields reserved for these but they are not honoured by
  the integration.
- **UFM error taxonomy is incomplete.** Some UFM failure modes surface
  as a generic error in the API server log. Distinguishing transient
  network issues from misconfiguration may require correlating against
  UFM-side logs.

---

## Troubleshooting

If a partition is not removed after deletion is accepted, use the detail view
in [Verification](#verification) and record `STATE`, `PKEY`, and `STATE MACHINE`.
Check the controller logs and whether UFM still reports the partition or its
memberships. Collect the partition ID, detail fields, relevant logs, and UFM state
for your site operator. This guide provides no manual P_Key repair or
force-release procedure.

The following table maps symptoms and message fragments to their causes.
Workflow errors and controller SLA errors can add text around these fragments:

| Symptom | Likely cause |
|---|---|
| Instance remains `Configuring`; requested IB interface status is not `Ready` | UFM unreachable, host port `Down` or `Initialize`, or PKey allocation pending |
| `infiniband-partition create` fails with PKey-allocation error | `[ib_fabrics.default].pkeys` pool exhausted; operator extends the TOML ranges and follows [Operator (per site)](#operator-per-site), including restarting the API server |
| `nico_ib_monitor_machines_with_unexpected_pkeys_count > 0` | Host is in a partition no instance config asks for; out-of-band UFM edit or a stale binding the monitor will clean up |
| `nico_ib_monitor_machines_with_missing_pkeys_count > 0` | Desired binding has not (yet) been applied to UFM; check UFM error counters and monitor latency |
| `IbCleanupPending` alert does not clear | A parsed alert GUID still appears in a monitor discrepancy list, or alert clearance has not completed. Inspect UFM memberships and monitor logs |
| Allocation or an IB change reports "IB partition ... is not created" | With [Required Core Support](#required-core-support), Core uses this message for missing partitions and partitions marked for deletion |
| REST reports "Could not find InfiniBand Partition with ID" or batch allocation reports "Could not find Partition with ID" | The requested partition is absent from REST inventory |
| REST reports "does not match with Instance Site" | The partition's site differs from the instance's site |
| REST reports "not owned by Tenant", or Core reports "is not owned by the tenant" | A requested partition belongs to another tenant |
| REST allocation or update reports "is not in Ready state" | Site and tenant checks passed, but the partition is not `Ready` or has no `controllerIBPartitionId` |
| Deletion reports "active Instances ... unable to delete" or "instance(s) are still using it" | REST interface records or Core instance references remain, including terminating instances; wait for termination and inventory cleanup, then retry |
| `STATE MACHINE` contains "Waiting for all IB instances are released" | UFM still reports the partition; have the operator inspect its UFM memberships and controller logs |
| `STATE MACHINE` contains "Waiting for N instance(s) to release IB partition" | Core still has N instance references. Follow steps 1-3 of [Deleting a Partition](#deleting-a-partition) to remove references and verify inventory cleanup |
| `STATE` is `TERMINATING`, `PKEY` is empty, and `STATE MACHINE` shows "OK" | No stored P_Key is available, so automatic final deletion cannot complete. Collect logs for "IB partition has no pkey while ready" or "IB partition has no pkey while deleting" for the site operator. The log can predate the deletion request |
| Default partition reports full membership on managed ports | UFM hardening incomplete; revisit `default_membership = limited` in the IB runbook |
