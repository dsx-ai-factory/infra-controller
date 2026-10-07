-- Persist the Machine <-> BMC interface link on machine_interfaces instead of
-- deriving it from machine_topologies.topology->'bmc_info'.
CREATE TYPE interface_type AS ENUM ('Data', 'Bmc');

ALTER TABLE machine_interfaces
ADD COLUMN interface_type interface_type NOT NULL DEFAULT 'Data';

CREATE INDEX IF NOT EXISTS machine_interfaces_bmc_machine_id_idx
ON machine_interfaces(machine_id)
WHERE interface_type = 'Bmc';

-- Backfill by BMC IP first. This is the most precise match when the BMC
-- interface row has already been discovered or statically preallocated.
UPDATE machine_interfaces mi
SET
    machine_id = mt.machine_id,
    association_type = 'Machine',
    primary_interface = FALSE,
    interface_type = 'Bmc'
FROM machine_interface_addresses mia,
     machine_topologies mt
WHERE mia.interface_id = mi.id
  AND NULLIF(mt.topology->'bmc_info'->>'ip', '') IS NOT NULL
  AND mia.address = (mt.topology->'bmc_info'->>'ip')::inet
  AND mi.power_shelf_id IS NULL
  AND mi.switch_id IS NULL
  AND (mi.machine_id IS NULL OR mi.machine_id = mt.machine_id);

-- Fall back to BMC MAC for rows that did not have a usable IP match.
UPDATE machine_interfaces mi
SET
    machine_id = mt.machine_id,
    association_type = 'Machine',
    primary_interface = FALSE,
    interface_type = 'Bmc'
FROM machine_topologies mt
WHERE mi.interface_type = 'Data'
  AND NULLIF(mt.topology->'bmc_info'->>'mac', '') IS NOT NULL
  AND mi.mac_address = (mt.topology->'bmc_info'->>'mac')::macaddr
  AND mi.power_shelf_id IS NULL
  AND mi.switch_id IS NULL
  AND (mi.machine_id IS NULL OR mi.machine_id = mt.machine_id);

-- BMC DNS records now use the linked BMC machine_interface address.
DROP VIEW IF EXISTS dns_records;

-- Shortname DNS records include primary data interfaces and BMC interfaces.
-- BMC interfaces are intentionally not primary interfaces.
CREATE OR REPLACE VIEW dns_records_shortname_combined AS
SELECT
    concat(mi.hostname, '.', d.name, '.') AS q_name,
    mia.address AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(mia.address) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
    JOIN domains d ON d.id = mi.domain_id
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    mi.primary_interface = TRUE
    OR mi.interface_type = 'Bmc';

CREATE OR REPLACE VIEW dns_records_bmc_host_id AS
SELECT
    concat(mi.machine_id, '.bmc.', d.name, '.') AS q_name,
    mia.address AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(mia.address) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
    JOIN domains d ON d.id = mi.domain_id
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    mi.machine_id IS NOT NULL
    AND mi.interface_type = 'Bmc'
    AND (starts_with(mi.machine_id, 'fm100h') OR starts_with(mi.machine_id, 'fm100p'));

CREATE OR REPLACE VIEW dns_records_bmc_dpu_id AS
SELECT
    concat(mi.machine_id, '.bmc.', d.name, '.') AS q_name,
    mia.address AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(mia.address) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_interface_addresses mia ON mia.interface_id = mi.id
    JOIN domains d ON d.id = mi.domain_id
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    mi.machine_id IS NOT NULL
    AND mi.interface_type = 'Bmc'
    AND starts_with(mi.machine_id, 'fm100d');

CREATE OR REPLACE VIEW dns_records AS
SELECT *
FROM
  dns_records_shortname_combined
  FULL JOIN dns_records_adm_combined USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_host_id USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_dpu_id USING (q_name, resource_record, q_type, ttl, domain_id);
