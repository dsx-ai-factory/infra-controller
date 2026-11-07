-- Seed the site-wide rotation target for the BF4 DPU BMC `service` account and
-- enroll every already-ingested BF4 DPU at version 0.
--
-- Rotation of this credential does not exist before this migration, so every BF4
-- DPU that already carries a `service` password is, by definition, at version 0
-- (the unversioned `machines/bmc/site/dpu_service` secret). Recording that
-- baseline lets the rotation engine start from a correct picture; the target
-- starts at 0 too, so nothing is behind and no spurious rotation fires.
--
-- Only BF4 DPUs expose the `service` account (BF3/BF2 do not), so only they are
-- enrolled. A DPU is BF4 when its persisted topology carries a DMI
-- `product_name` that matches `is_bf4_dmi_product` (upper-cased, contains `B4`
-- or `BLUEFIELD-4`) -- the same per-machine signal the machine controller uses
-- to reason about BF4. The device is keyed by its BMC interface MAC, matching
-- every other rotation family; both joins are keyed by `machine_id`, so a DPU is
-- enrolled straight from its own machine record with no IP- or report-keyed
-- indirection.
--
-- The `ON CONFLICT DO NOTHING` guard keeps this idempotent so the drift-proof
-- `include_str!` backfill test can re-apply the file after seeding rows.

INSERT INTO sitewide_credential_rotation (credential_type, target_version)
VALUES ('dpu_bmc_service', 0)
ON CONFLICT (credential_type) DO NOTHING;

INSERT INTO device_credential_rotation (device_mac, credential_type, current_version)
SELECT DISTINCT ON (m.id) mi.mac_address, 'dpu_bmc_service', 0
FROM machines m
JOIN machine_interfaces mi
    ON mi.machine_id = m.id AND mi.interface_type = 'Bmc'
JOIN machine_topologies mt
    ON mt.machine_id = m.id
WHERE starts_with(m.id, 'fm100d')
  AND (
        upper(mt.topology #>> '{discovery_data,Info,dmi_data,product_name}') LIKE '%B4%'
        OR upper(mt.topology #>> '{discovery_data,Info,dmi_data,product_name}') LIKE '%BLUEFIELD-4%'
      )
ORDER BY m.id, mi.created ASC
ON CONFLICT (device_mac, credential_type) DO NOTHING;
