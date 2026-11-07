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

## The Partition Model

InfiniBand partitioning in NICo is built on the InfiniBand-native P_Key
mechanism enforced by the subnet manager. The operator-facing chain is:

```
Instance ──► IB Interface ──► IbPartition (NICo) ──► P_Key (UFM)
```

Read it top to bottom:

1. A **tenant instance** has one or more **IB interfaces**, one per
   InfiniBand port on the host the instance is allocated to.
2. Each IB interface references exactly one **`IbPartition`** by ID — this
   is the NICo object the tenant manipulates.
3. The `IbPartition` corresponds to a single **P_Key** on UFM. NICo either
   allocates the P_Key value from a configured range or honours an
   explicit P_Key the operator supplied.
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
| UFM endpoint URL(s) and managed P_Key ranges | NICo API server TOML, under `[ib_fabrics.<name>]` |
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
| UFM endpoint(s), P_Key ranges, fabric toggles | Operator | **TOML** (`[ib_fabrics.<name>]`, `[ib_config]`) — Day 0 / rare |
| UFM API credentials | Operator | Secrets backend (Vault) |
| Create / update / delete an InfiniBand partition | Tenant | **REST** `…/nico/infiniband-partition` · `nicocli infiniband-partition create` |
| Assign an instance's IB interface to a partition | Tenant | **REST** `…/nico/instance` (update) · `nicocli instance update` |
| List partitions and check sync state for triage | Operator | `nicocli infiniband-partition list` → `nico-admin-cli ib_partition show` for deeper internal state |
| Break-glass: unbind a host from UFM out of band | Operator | **`nico-admin-cli`** (gRPC) — not exposed via REST |

The UFM-facing setup (the first two rows) is the operator's responsibility and
is described in [Configuring NICo to Talk to UFM](#configuring-nico-to-talk-to-ufm).
Everything a tenant does — creating partitions and attaching interfaces — goes
through the REST API or `nicocli`; the gRPC `nico-admin-cli` rows are operator
triage and break-glass paths that the REST API does not expose.

---

## Configuring NICo to Talk to UFM

Two TOML blocks are involved. Both live in the API server's config file.

### `[ib_fabrics.<name>]` — Endpoints and P_Key Pool

Each named entry defines one InfiniBand fabric that NICo manages.

```toml
[ib_fabrics.fabric_a]
endpoints = ["https://ufm-a.example.internal:443"]

  [[ib_fabrics.fabric_a.pkeys]]
  start = "0x100"
  end   = "0x1ff"

  [[ib_fabrics.fabric_a.pkeys]]
  start = "0x300"
  end   = "0x3ff"
```

Fields:

| Field | Purpose |
|---|---|
| `endpoints` | UFM server URLs. At the time of writing only the first endpoint is consulted; multi-endpoint support exists in the schema for forward compatibility |
| `pkeys` | One or more inclusive P_Key ranges, hex-encoded. These define the pool that NICo's auto-allocator draws from when a tenant creates an `IbPartition` without specifying a P_Key |

P_Key ranges may be **extended in future restarts but never shrunk**:
removing or narrowing a range under live tenants would orphan allocated
partitions. Plan the pool with headroom.

### `[ib_config]` — Fabric Toggles

```toml
[ib_config]
enabled = true
allow_insecure = false
mtu = 4096
rate_limit = 200
service_level = 0
fabric_monitor_run_interval = "60s"
```

| Field | Purpose |
|---|---|
| `enabled` | Master switch. With `false`, NICo will not call UFM and will not run the monitor |
| `allow_insecure` | Permit TLS to UFM without certificate verification. Intended for development only |
| `mtu`, `rate_limit`, `service_level` | Defaults applied to NICo-managed partitions when not overridden per partition |
| `fabric_monitor_run_interval` | Steady-state cadence for `IbFabricMonitor`. Defaults to 60 seconds |

### UFM credentials

UFM API credentials are not stored in TOML. They are read from the
configured secrets backend (Vault under standard deployments) by the UFM
client during initialisation. Rotate them at the secrets backend; NICo
picks up the new value on its next client re-initialisation.

---

## P_Key Allocation

When a tenant creates an InfiniBand partition (REST
`POST …/nico/infiniband-partition`, or `nicocli infiniband-partition create`),
the request may include or omit a desired P_Key:

- **`pkey` omitted.** NICo allocates a free P_Key from the configured
  pool ranges and returns it on the response. This is the normal tenant
  flow.
- **`pkey` specified** (hex, for example `"0x76b"`). NICo accepts the
  request only if the requested value falls inside a pool range that is
  **not** marked as auto-assigned, and is otherwise free. Otherwise the
  request is rejected.

The model's `IbPartition` object retains the allocated P_Key for the
lifetime of the partition. There is no "renumber" operation; to change a
P_Key, delete the partition and create a new one (which the tenant flow
handles via instance reconfiguration).

Updating a partition (`nicocli infiniband-partition update`) is restricted to
fields other than P_Key (name, MTU, rate limit, and so on). Deleting one
(`nicocli infiniband-partition delete`) requires that no instance still
references the partition.

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
5. Updates per-machine InfiniBand status observations in the NICo
   database, which is what feeds the `configs_synced.infiniband` field on
   `InstanceStatus`.

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
2. NICo validates the config (every referenced partition exists,
   ownership matches the tenant) and stores it in the database.
3. The next `IbFabricMonitor` iteration observes the new desired state
   and issues `bind_ib_ports()` for each host GUID that is not already a
   member of the expected P_Key.
4. UFM updates partition membership. On the following iteration the
   monitor reads back the new membership and updates the machine's IB
   status observation.
5. `InstanceStatus::infiniband::configs_synced` flips to `true` once
   observed UFM state matches desired state. The aggregate
   `configs_synced` and therefore the instance's `Ready` state follow.

Tenants observe the in-flight state as `Configuring` and the
`InstanceStatus` machine remains in `WaitingForNetworkConfig` until the
monitor reports convergence.

---

## Force-Delete and Cleanup

When an instance is released or its host is force-deleted, NICo clears
the IB interfaces from the instance config. The reconciler then sees host
GUIDs in NICo-managed P_Keys that no longer correspond to any live
instance and removes them via `unbind_ib_ports()`.

NICo tracks the cleanup with an `IbCleanupPending` health alert on the
machine. The alert is set when cleanup is required and cleared once the
monitor confirms that every GUID has been removed from UFM-side
partitions. A machine with an outstanding `IbCleanupPending` alert is
ineligible for reuse by another tenant: this is the IB equivalent of the
Ethernet termination guard described in
[Default Isolation](../vpc/vpc_network_virtualization.md#default-isolation-the-admin-overlay).

There is no dedicated "force-delete IB partition" operation. Partitions
persist in the NICo database independent of instance churn; their membership
is what is reconciled against UFM. To remove a partition entirely, every
instance referencing it must release first, then the tenant's
`nicocli infiniband-partition delete` (REST `DELETE …/nico/infiniband-partition/{id}`)
will succeed.

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
   - For each fabric, define `[ib_fabrics.<name>]` with the UFM
     endpoint(s) and one or more `pkeys` ranges. Size the ranges with
     room for future growth.
4. Restart the API server. The `IbFabricMonitor` begins its periodic
   reconciliation on the next tick.

### Tenant (per partition)

All tenant steps use the REST API or `nicocli`; none require TOML or
`nico-admin-cli`.

1. `nicocli infiniband-partition create` (REST `POST …/nico/infiniband-partition`)
   for each isolation domain the tenant needs (typically one per workload).
2. `nicocli instance update` (REST `PATCH …/nico/instance`) to attach each
   IB interface to the appropriate partition.
3. Wait for `configs_synced.infiniband = true` to converge.

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
   and confirm each is in a converged state. For deeper internal state during
   triage — the state-machine outcome field that surfaces UFM sync failures —
   an operator can use `nico-admin-cli ib_partition show` (`--id`,
   `--tenant-org-id`, or `--name`), which the REST API does not expose.
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
5. **No outstanding cleanup.** Confirm no managed machine has an
   `IbCleanupPending` alert.

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

| Symptom | Likely cause |
|---|---|
| Instance never leaves `Configuring`; `configs_synced.infiniband = false` | UFM unreachable, host port `Down` or `Initialize`, or PKey allocation pending |
| `infiniband-partition create` fails with PKey-allocation error | `[ib_fabrics.<name>].pkeys` pool exhausted; operator extends the ranges |
| `infiniband-partition create` with an explicit `pkey` rejected | Requested PKey falls in an auto-allocate range or is already in use |
| `nico_ib_monitor_machines_with_unexpected_pkeys_count > 0` | Host is in a partition no instance config asks for; out-of-band UFM edit or a stale binding the monitor will clean up |
| `nico_ib_monitor_machines_with_missing_pkeys_count > 0` | Desired binding has not (yet) been applied to UFM; check UFM error counters and monitor latency |
| `IbCleanupPending` alert does not clear | Monitor cannot remove a GUID from a partition (UFM error, or the binding sits outside any NICo-managed range); inspect UFM-side state directly |
| `infiniband-partition delete` fails | At least one instance still references the partition |
| Default partition reports full membership on managed ports | UFM hardening incomplete; revisit `default_membership = limited` in the IB runbook |
