--- 20250722183300_instance_interface_id_v2.sql
---
--- This migration is an enhancement on 20250711040508_instance_interface_id.sql
--- and was created in response to https://jirasw.nvidia.com/browse/FORGE-6604.
---
--- The key enhancements here are:
--- 1) It generates a unique UUID for each interface. While technically it only
---    needs to be unique per instance, it's better to have them all globally
---    unique.
--- 2) It dives into each interface element for a given instance (and not just
---    interface[0], supporting VFs and multiple PFs.
---
--- Breakdown is:
---
--- Simple enhancement to use to_jsonb(gen_random_uuid()::text) to generate
--- a unique UUID for the interface, replacing the static UUID being set
--- in the prior migration.
---
--- From there, this does te following:
--- - Uses jsonb_array_elements to get at each element in network_config['interfaces'].
--- - Uses jsonb_set to update the internal_uuid of each array element.
--- - Uses jsonb_agg to smash all of the updated array elements back together.
--- - Feeds that to json_set to update network_config['interfaces'] with the updated
---   list of interfaces.
---
--- For safety, this will also:
--- - Verify that network_config['interfaces'] exists.
--- - Verify that network_config['interfaces'] is an array.
---
--- If the safety checks fail, that row is just ignored (since we use it
--- with a WHERE).
---
UPDATE instances
SET network_config = jsonb_set(
    network_config,
    '{interfaces}',
    (
        SELECT jsonb_agg(
            jsonb_set(
                interface,
                '{internal_uuid}',
                to_jsonb(gen_random_uuid()::text)
            )
        )
        FROM jsonb_array_elements(network_config->'interfaces') AS interface
    )
)
WHERE network_config ? 'interfaces'
  AND jsonb_typeof(network_config->'interfaces') = 'array';
