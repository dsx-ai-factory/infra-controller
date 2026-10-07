-- First, introduce a machine_group_member_ids(target_id) helper function.
--
-- A "machine group" is the host machine plus all DPU machines attached
-- to it. Given any one machine ID in the group (whether host or DPU),
-- this function returns every member's ID.
--
-- This is a two step process, where (1) we find the host of the group; if
-- the target itself is a host, we're good. Otherwise, we'll search via
-- machine_interfaces to get its host. This of course excludes the DPU's
-- own self-reference. We now are able to derive that the group is that
-- given host + every DPU with machine_id = host.id.
--
-- ..and then step (2). For a zero DPU host, this finds no DPUs, and the
-- result is just the host. But for a DPU whose host isn't wired up yet,
-- we'll fall back to the target_id, resulting in just the DPU -- and
-- subsequent writes *after* the host is linked will see the full group.
CREATE OR REPLACE FUNCTION machine_group_member_ids(target_id VARCHAR)
RETURNS TABLE(id VARCHAR) AS $$
    WITH host AS (
        SELECT COALESCE(
            (SELECT mi.machine_id
             FROM machine_interfaces mi
             WHERE mi.attached_dpu_machine_id = target_id
               AND mi.machine_id != mi.attached_dpu_machine_id
             LIMIT 1),
            target_id
        ) AS id
    )
    SELECT id FROM host
    UNION
    SELECT mi.attached_dpu_machine_id
    FROM machine_interfaces mi
    JOIN host ON mi.machine_id = host.id
    WHERE mi.attached_dpu_machine_id IS NOT NULL
      AND mi.machine_id != mi.attached_dpu_machine_id;
$$ LANGUAGE SQL STABLE;

-- And this is for backfilling use_admin_network onto host rows
-- as part of the change.
--
-- The query walks host DPUs via `machine_interfaces.attached_dpu_machine_id`.
-- For multi-DPU hosts, the inner subquery's `GROUP BY mi.machine_id`
-- collapses all of a host's DPU interfaces into a single row, with `bool_and`
-- aggregating across them; each host gets exactly one updated row regardless
-- of DPU count.
--
-- If the DPUs disagree (shouldn't happen in practice, but just in case) we
-- pick `false` (tenant), to ensure we don't incorrectly force a tenant-allocated
-- host "back" to admin. The runtime self-corrects a single DPU back to admin
-- when it has no tenant interface config (per `!dpu_has_tenant_interface_config`),
-- so a DPU briefly seeing `false` for a host that should really be `true`
-- recovers transparently.
--
-- Zero-DPU hosts have no attached DPU to copy from; their network_config
-- default is `use_admin_network: true`, so no backfill is needed for them.
UPDATE machines AS hosts
SET network_config = jsonb_set(
    hosts.network_config,
    '{use_admin_network}',
    to_jsonb(backfill.flag),
    true
)
FROM (
    SELECT mi.machine_id AS host_id,
           bool_and(
               COALESCE((dpu_m.network_config->>'use_admin_network')::bool, true)
           ) AS flag
    FROM machine_interfaces mi
    JOIN machines dpu_m ON dpu_m.id = mi.attached_dpu_machine_id
    WHERE mi.attached_dpu_machine_id IS NOT NULL
      AND mi.attached_dpu_machine_id != mi.machine_id
    GROUP BY mi.machine_id
) AS backfill
WHERE hosts.id = backfill.host_id;
