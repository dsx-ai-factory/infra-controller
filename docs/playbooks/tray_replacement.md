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

The commands below use `bash`, `grep`, and `jq` 1.7 or later. GPU UIDs are
64-bit integers; `jq` 1.7 keeps them exact, but earlier versions round them.
Check the version first:

```bash
jq --version
```

## 1. Collect the tray's NVLink identity

```bash
nico-admin-cli -a "$API" machine show "$MACHINE_ID"
nico-admin-cli -a "$API" machine nvlink-info show "$MACHINE_ID" > nvlink-info.json
export GPU_UIDS="$(jq -r '.gpus[].guid' nvlink-info.json)"
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

Define helpers that query NMX-C and report partition membership. A failed query
or an unexpected response is reported as `QUERY FAILED`, never as success:

```bash
# Prints "<partition name><TAB><GPU UID>" for each GPU in a saved
# partition-info-list response.
partition_members() {
  jq -r '.body.partition_info_list[] | .name as $name | .gpu_uid_list[] | "\($name)\t\(.)"' "$1"
}

# Saves a partition-info-list response to the given file.
query_partitions() {
  nico-admin-cli -a "$API" browse nmxc "${NMXC_SELECTOR[@]}" \
    --operation partition-info-list > "$1" || { echo "QUERY FAILED"; return 2; }
  jq -e '.code == 200 and (.body.partition_info_list | type == "array")' \
    "$1" > /dev/null || { echo "QUERY FAILED: unexpected response in $1"; return 2; }
}

# Reports whether any partition still holds one of the given GPU UIDs.
check_gpus_partitioned() {
  local file=$1 uids=$2 found=0 uid
  [ -n "$uids" ] || { echo "NO GPU UIDS TO CHECK"; return 2; }
  query_partitions "$file" || return 2
  for uid in $uids; do
    if partition_members "$file" | cut -f2 | grep -qx "$uid"; then
      echo "PARTITIONED: $uid"
      found=1
    fi
  done
  [ "$found" -eq 0 ] && echo "NOT PARTITIONED: all $(echo $uids | wc -w | tr -d " ") GPU UIDs"
  return "$found"
}

check_gpus_partitioned partitions-before.json "$GPU_UIDS"
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

While the host is marked, the partition monitor reports its progress on every
pass as a health report with source `nvlink-isolation`:

- an `NvlinkIsolationPending` alert names the GPUs that NMX-C still lists in a
  partition, or says that isolation cannot be confirmed because the host's
  NVLink GPU inventory is incomplete;
- an `NvlinkIsolated` success means that NMX-C lists none of the host's GPUs in
  any partition.

The monitor reports `NvlinkIsolated` on the pass after its removals complete
(`monitor_run_interval`, one minute by default). Define a helper that prints the
status and succeeds only when the host is isolated:

```bash
isolation_status() {
  local out
  out="$(nico-admin-cli -a "$API" -f json machine health-report show "$1")" \
    || { echo "QUERY FAILED"; return 2; }
  printf '%s\n' "$out" | jq -r '
    [.[].report | select(.source == "nvlink-isolation")] as $r
    | if ($r | length) == 0 then "NO STATUS"
      elif (($r[0].successes // []) | map(.id) | index("NvlinkIsolated")) != null then "ISOLATED"
      else "PENDING: " + (($r[0].alerts // []) | map(.message) | join("; "))
      end' | tee /dev/stderr | grep -qx ISOLATED
}

isolation_status "$MACHINE_ID"
```

**Continue only when this prints `ISOLATED`.** `NO STATUS` means the monitor has
not checked the host since it was marked; wait for the next pass. If it prints
`QUERY FAILED`, fix the query and run it again. To cross-check against NMX-C
directly, run `check_gpus_partitioned partitions-after.json "$GPU_UIDS"`. If the
status stays `PENDING` after several passes:

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

1. Run `isolation_status "$MACHINE_ID"` immediately before handing over the
   tray. Hand it over only if it prints `ISOLATED`.
2. Have the data center team remove the tray from its slot.
3. If the tray is being replaced, force-delete the old machine as described in
   [Force Deleting and Rebuilding NICo Hosts](force_delete.md), and then have
   the replacement tray installed.

If the same tray is repaired or moved and reinstalled, keep the mark on until it
is back and skip the force-delete.

## 8. Hold the returning host out of allocation

The host that comes back must not receive tenant work until step 9 passes.
Machine validation is optional per site, so a replacement host can reach
`Ready` without it. Removing the mark from a reinstalled tray also removes the
mark's allocation block.

Set `HOST_ID` to the replacement's machine ID as soon as it appears in NICo, or
to `MACHINE_ID` for a reinstalled tray, and check its existing health reports:

```bash
export HOST_ID="<replacement-host-machine-id, or $MACHINE_ID for the same tray>"
nico-admin-cli -a "$API" machine show "$HOST_ID"
nico-admin-cli -a "$API" machine health-report show "$HOST_ID"
```

If the host has no `manual-maintenance` report, add an `out-for-repair` hold.
If it already has one, keep that report instead: it already blocks allocations,
and adding `out-for-repair` would replace it.

```bash
nico-admin-cli -a "$API" machine health-report add "$HOST_ID" \
  --template out-for-repair --message "$TICKET: verifying tray"
```

If a replacement host already has an instance, involve the tenant before you
continue.

For a reinstalled tray only, remove the mark once the hold is in place. The
partition monitor then returns its GPUs to `tray_partition_<slot>`, or to the
tenant's partition if an instance requests it:

```bash
nico-admin-cli -a "$API" machine health-report remove "$MACHINE_ID" compute-tray-due-for-swap
```

## 9. Verify the host

The partition monitor places the host's GPUs only after discovery populates its
NVLink information. Wait until `machine nvlink-info show` lists the host's GPUs,
then wait for the next monitor pass before you check membership:

```bash
nico-admin-cli -a "$API" machine nvlink-info show "$HOST_ID" > nvlink-info-host.json
export HOST_UIDS="$(jq -r '.gpus[].guid' nvlink-info-host.json)"
export SLOT="$(jq -r '.gpus[0].slot_id // empty' nvlink-info-host.json)"

if [ -z "$HOST_UIDS" ] || [ -z "$SLOT" ]; then
  echo "NO NVLINK GPUS OR SLOT YET: do not release the host"
elif query_partitions partitions-verify.json; then
  missing=0
  for uid in $HOST_UIDS; do
    if partition_members partitions-verify.json | grep -qxF "$(printf 'tray_partition_%s\t%s' "$SLOT" "$uid")"; then
      echo "IN TRAY PARTITION: $uid"
    else
      echo "NOT IN TRAY PARTITION: $uid"
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] && echo "ALL $(echo $HOST_UIDS | wc -w | tr -d " ") HOST GPUS IN TRAY PARTITION"
fi
```

Confirm that:

- the check prints `ALL <n> HOST GPUS IN TRAY PARTITION`, where `<n>` is the
  number of GPUs on the tray. For a reinstalled tray whose instance requests a
  tenant partition, confirm that the UIDs are in the tenant's partition instead;
- for a replacement, `check_gpus_partitioned partitions-old.json "$GPU_UIDS"`
  prints `NOT PARTITIONED` for the old tray's UIDs from step 1;
- `nv show sdn partition <id>` on the leader NVSwitch shows no `0.0.0.0`
  locations in any partition;
- on the host, `nvidia-smi -q | grep -A3 Fabric` shows `State: Completed`,
  `Status: Success`, and a non-zero `CliqueId` on every GPU;
- machine validation passes, if the site runs it.

## 10. Release the host

Only after every step 9 check passes, remove the hold you added in step 8:

```bash
nico-admin-cli -a "$API" machine health-report remove "$HOST_ID" manual-maintenance
```

If the host already had a `manual-maintenance` report in step 8, leave it for
its owner to remove.
