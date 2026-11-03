# Replace a Compute Tray in an NVLink Rack

Use this procedure to move or replace a compute tray in an NVLink rack, such as
a GB200 NVL72, without leaving stale GPUs in NVLink partitions.

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

The commands below use `bash`, `jq`, and `grep`.

## 1. Collect the tray's NVLink identity

```bash
nico-admin-cli -a "$API" machine show "$MACHINE_ID"
nico-admin-cli -a "$API" machine nvlink-info show "$MACHINE_ID" > nvlink-info.json
```

Extract the tray's GPU UIDs. GPU UIDs are 64-bit integers, so extract them as
text rather than as `jq` numbers, which can lose precision:

```bash
export GPU_UIDS="$(grep -oE '"guid": *[0-9]+' nvlink-info.json | grep -oE '[0-9]+$')"
echo $GPU_UIDS | wc -w | tr -d " "
```

**Stop and escalate unless this prints the number of GPUs on the tray.** If
`nvlink-info show` fails or returns no GPUs, there is no way to confirm that the
GPUs were removed. Note `gpus[].slot_id` in `nvlink-info.json`; it names the
tray default partition, `tray_partition_<slot>`.

## 2. Select the NMX-C endpoint

Reach NMX-C the same way the NVLink partition monitor does for this host. If
the step 1 `machine show` output lists a `RACK_ID`, the host is managed through
its rack, and NMX-C is reached through the rack's ready control-plane switch.
Otherwise, NMX-C is reached through the chassis serial's configured endpoint.

```bash
export RACK_ID="<RACK_ID from machine show, or empty>"
if [ -n "$RACK_ID" ]; then
  NMXC_SELECTOR=(--rack-id "$RACK_ID")
else
  NMXC_SELECTOR=(--chassis-serial "$(jq -r '.chassis_serial' nvlink-info.json)")
fi
```

## 3. Record current partition membership

Define a check that queries NMX-C and reports, for each GPU UID from step 1,
whether any partition still contains it. The check fails instead of reporting
success when the query fails or returns an unexpected response, and it matches
whole numbers only:

```bash
check_gpus_partitioned() {
  local file=$1 found=0 uid
  nico-admin-cli -a "$API" browse nmxc "${NMXC_SELECTOR[@]}" \
    --operation partition-info-list > "$file" || { echo "QUERY FAILED"; return 2; }
  jq -e '.code == 200 and (.body.partition_info_list | type == "array")' \
    "$file" > /dev/null || { echo "QUERY FAILED: unexpected response in $file"; return 2; }
  for uid in $GPU_UIDS; do
    if grep -qE "(^|[^0-9])${uid}([^0-9]|$)" "$file"; then
      echo "PARTITIONED: $uid"
      found=1
    fi
  done
  [ "$found" -eq 0 ] && echo "NOT PARTITIONED: all $(echo $GPU_UIDS | wc -w | tr -d " ") GPU UIDs"
  return "$found"
}

check_gpus_partitioned partitions-before.json
```

Keep `partitions-before.json` with the ticket.

## 4. Handle any tenant instance

Check the step 1 `machine show` output for an instance. Step 5 removes the
tray's GPUs from the tenant's partition even while the instance exists, which
cuts NVLink between this tray and the rest of the tenant's partition.

- **The tray works and has a tenant instance:** have the tenant release it with
  [Release Instance for Full Repair](../manuals/repair/release_instance_for_repair.md)
  first.
- **The tray has failed** (powered off, unreachable, or stuck terminating): an
  unreachable tray does not prove that the tenant's work has stopped, and the
  tenant may still run distributed work across the rest of the partition.
  Before step 5, either confirm that the work on the tray has stopped or cannot
  recover, or record an explicit decision in the incident to accept the
  disruption. Notify the tenant through the incident.

## 5. Mark the tray

```bash
nico-admin-cli -a "$API" machine health-report add "$MACHINE_ID" \
  --template compute-tray-due-for-swap --message "$TICKET: replacing tray"
```

The `compute-tray-due-for-swap` template blocks new allocations, like
`out-for-repair`. It also carries the `IsolateFromNvLinkPartitions`
classification. On its next pass, the NVLink partition monitor removes every GPU
on the host from every NVLink partition and keeps them out until the mark is
removed. This applies even if the host still has an instance.

The template uses its own health report source, `compute-tray-due-for-swap`.
It does not replace other holds on the host, such as `out-for-repair`.

## 6. Confirm removal

Wait for the next partition monitor pass (`monitor_run_interval`, one minute by
default), then run the check again:

```bash
check_gpus_partitioned partitions-after.json
```

**Continue only when this prints `NOT PARTITIONED` with the GPU count from
step 1.** If it prints `QUERY FAILED`, fix the query and run it again. If any
UID is still partitioned after several passes:

1. Check the nvlink-manager logs for the machine ID and for NMX-C return codes.
2. Escalate to the NVLink/NMX-C owners. If removal fails now, moving the tray
   will strand those UIDs, and the only recovery after that is an NMX-C factory
   reset.

## 7. Remove the tray with the mark in place

Keep the mark until the tray is out of its slot. Do not force-delete the
machine first: force-deletion removes the mark, and while the tray is still
installed, Site Explorer can rediscover it as a new, unmarked machine. The
partition monitor would then return its GPUs to `tray_partition_<slot>` and undo
step 6.

1. Run `check_gpus_partitioned partitions-handover.json` immediately before
   handing over the tray. Hand it over only if the check passes.
2. Have the data center team remove the tray from its slot.
3. If the tray is being replaced, force-delete the old machine as described in
   [Force Deleting and Rebuilding NICo Hosts](force_delete.md), and then have
   the replacement tray installed.

If the same tray is repaired or moved and reinstalled, keep the mark on until it
is back, skip the force-delete, and go to step 9.

## 8. Verify the replacement tray

After the new tray ingests:

```bash
export NEW_MACHINE_ID="<new-host-machine-id>"
nico-admin-cli -a "$API" machine nvlink-info show "$NEW_MACHINE_ID" > nvlink-info-new.json
nico-admin-cli -a "$API" browse nmxc "${NMXC_SELECTOR[@]}" \
  --operation partition-info-list > partitions-new.json
```

Confirm that:

- each new GPU UID appears in `partitions-new.json` under `tray_partition_<slot>`;
- `check_gpus_partitioned partitions-old-uids.json` prints `NOT PARTITIONED` for
  the old tray's UIDs from step 1;
- `nv show sdn partition <id>` on the leader NVSwitch shows no `0.0.0.0`
  locations in any partition;
- in scout, `nvidia-smi -q | grep -A3 Fabric` shows `State: Completed`,
  `Status: Success`, and a non-zero `CliqueId` on every GPU;
- machine validation passes.

## 9. Same-tray repair or move: remove the mark

Only for a tray that was repaired or moved and reinstalled with the same machine
ID:

```bash
nico-admin-cli -a "$API" machine health-report remove "$MACHINE_ID" compute-tray-due-for-swap
```

The partition monitor then returns the GPUs to `tray_partition_<slot>`, or to
the tenant's partition if an instance requests it.
