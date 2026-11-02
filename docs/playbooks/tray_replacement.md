# Replace a GB200 Compute Tray

Use this procedure to move or replace a compute tray in a GB200 NVL rack
without leaving stale GPUs in NVLink partitions.

The end state is:

- The old tray's GPUs are in no NVLink partition, neither a tenant partition
  nor the tray default partition (`tray_partition_<slot>`), before the tray
  leaves its slot.
- The replacement tray ingests as a new machine, and the NVLink partition
  monitor places its GPUs in `tray_partition_<slot>`.
- The other trays in the rack keep their partitions and workloads.

## Why the order matters

NMX-C Fabric Manager (FM) tracks NVLink partition membership both by GPU UID
and by GPU location (`<chassis>.<slot>.<host>.<gpu>`). If a tray is replaced
while its GPUs are still in a partition, FM gives the slot's partition to the
new GPUs but keeps the old UIDs in the partition's member list. FM then:

- rejects any partition create for the new GPUs, because their location is
  already in a partition;
- rejects removal of the new UIDs with `NMX_ST_RESOURCE_NOT_IN_USE`;
- rejects removal of the old UIDs with `NMX_ST_RESOURCE_BAD`.

The only known recovery is an NMX-C factory reset, which disrupts every tray in
the NVLink domain. Remove the old GPUs from all partitions while FM still
recognizes them, which means before the tray leaves its slot.

## Inputs

| Name | Description |
| --- | --- |
| `API` | NICo API URL |
| `MACHINE_ID` | Host machine ID of the tray being moved or replaced |
| `TICKET` | Incident or RMA reference |

```bash
export API="<api-url>"
export MACHINE_ID="<host-machine-id>"
export TICKET="<ticket>"
```

## 1. Collect the tray's NVLink identity

```bash
nico-admin-cli -a "$API" machine show "$MACHINE_ID"
nico-admin-cli -a "$API" machine nvlink-info show "$MACHINE_ID" | tee nvlink-info.json
```

Record from `nvlink-info.json`:

- `chassis_serial`: the NVLink chassis serial used to reach NMX-C.
- `gpus[].guid`: the tray's GPU UIDs.
- `gpus[].slot_id`: the tray slot, which names `tray_partition_<slot>`.

```bash
export CHASSIS_SERIAL="$(jq -r '.chassis_serial' nvlink-info.json)"
export GPU_UIDS="$(jq -r '.gpus[].guid' nvlink-info.json)"
```

If the machine has no `nvlink_info`, stop and escalate. Without the GPU UIDs
you cannot confirm that the GPUs were removed.

## 2. Record current partition membership

```bash
nico-admin-cli -a "$API" browse nmxc --chassis-serial "$CHASSIS_SERIAL" \
  --operation partition-info-list > partitions-before.json

for uid in $GPU_UIDS; do
  grep -q "$uid" partitions-before.json && echo "$uid is in a partition"
done
```

Keep `partitions-before.json` with the ticket.

## 3. Handle any tenant instance

Check the `machine show` output from step 1:

- **The tray works and has a tenant instance:** have the tenant release it with
  [Release Instance for Full Repair](../manuals/repair/release_instance_for_repair.md)
  first. Step 4 cuts NVLink between this tray and the rest of the tenant's
  partition.
- **The tray has failed** (powered off, unreachable, or stuck terminating): the
  tenant workload on it is already lost. Continue to step 4 and notify the
  tenant through the incident.

## 4. Mark the tray

```bash
nico-admin-cli -a "$API" machine health-report add "$MACHINE_ID" \
  --template compute-tray-due-for-swap --message "$TICKET: replacing tray"
```

The `compute-tray-due-for-swap` template blocks new allocations, like `out-for-repair`.
It also carries the `IsolateFromNvLinkPartitions` classification. On its next
pass, the NVLink partition monitor removes every GPU on the host from every
NVLink partition and keeps them out until the mark is removed. This applies
even if the host still has an instance.

## 5. Confirm removal before the tray leaves its slot

Wait for the next partition monitor pass (`monitor_run_interval`, one minute by
default), then repeat the step 2 check:

```bash
nico-admin-cli -a "$API" browse nmxc --chassis-serial "$CHASSIS_SERIAL" \
  --operation partition-info-list > partitions-after.json

for uid in $GPU_UIDS; do
  grep -q "$uid" partitions-after.json && echo "STILL PARTITIONED: $uid"
done
```

**Do not move or remove the tray until this prints nothing.** If any UID is
still in a partition after several passes:

1. Check the nvlink-manager logs for the machine ID and for NMX-C return codes.
2. Escalate to the NVLink/NMX-C owners. If removal fails now, moving the tray
   will strand those UIDs, and the only recovery after that is an NMX-C factory
   reset.

## 6. Remove the old machine and swap the tray

If the tray is being replaced:

1. Force-delete the old machine as described in
   [Force Deleting and Rebuilding NICo Hosts](force_delete.md). The
   `compute-tray-due-for-swap` mark is deleted with the machine.
2. Hand the slot to the data center team for the physical swap.

If the same tray is repaired and reinstalled, skip the force-delete and go to
step 8 once it is back.

## 7. Verify the replacement tray

After the new tray ingests:

```bash
export NEW_MACHINE_ID="<new-host-machine-id>"
nico-admin-cli -a "$API" machine nvlink-info show "$NEW_MACHINE_ID" | tee nvlink-info-new.json
nico-admin-cli -a "$API" browse nmxc --chassis-serial "$CHASSIS_SERIAL" \
  --operation partition-info-list > partitions-new.json
```

Confirm that:

- each new GPU UID appears in `partitions-new.json` under `tray_partition_<slot>`;
- no old UID from step 1 appears anywhere in `partitions-new.json`;
- `nv show sdn partition <id>` on the leader NVSwitch shows no `0.0.0.0`
  locations in any partition;
- in scout, `nvidia-smi -q | grep -A3 Fabric` shows `State: Completed`,
  `Status: Success`, and a non-zero `CliqueId` on every GPU;
- machine validation passes.

## 8. Same-tray repair: remove the mark

Only for a tray that was repaired and reinstalled with the same machine ID:

```bash
nico-admin-cli -a "$API" machine health-report remove "$MACHINE_ID" manual-maintenance
```

The partition monitor then returns the GPUs to `tray_partition_<slot>`, or to
the tenant's partition if an instance requests it.
