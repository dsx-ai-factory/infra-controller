-- Purpose: Update DNS record views to derive q_type from the IP address family
-- when no explicit record type metadata exists. IPv6 addresses get 'AAAA',
-- IPv4 addresses get 'A'. Also fixes dns_records_instance hostname formatting
-- for IPv6 addresses (replace colons with dashes instead of dots with dashes).
--
-- The combined dns_records view must be dropped first since it depends on the
-- sub-views, and the sub-views' q_type column type changes from nullable
-- varchar(10) (just rt.type_name) to non-nullable varchar(10) (COALESCE).

DROP VIEW IF EXISTS dns_records;

CREATE OR REPLACE VIEW dns_records_adm_combined AS
SELECT
    concat(mi.machine_id, '.adm.', d.name, '.') AS q_name,
    mia.address AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(mia.address) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_interface_addresses mia ON (mia.interface_id = mi.id)
    JOIN domains d ON ((d.id = mi.domain_id)
            AND (mi.primary_interface = TRUE))
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id
WHERE (mi.machine_id IS NOT NULL);


CREATE OR REPLACE VIEW dns_records_bmc_host_id AS
SELECT
    concat(mi.machine_id, '.bmc.', d.name, '.') AS q_name,
    cast((mt.topology -> 'bmc_info' ->> 'ip') as inet) AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(cast((mt.topology -> 'bmc_info' ->> 'ip') as inet)) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_topologies mt ON mi.machine_id = mt.machine_id
            AND (mi.machine_id != mi.attached_dpu_machine_id)
    JOIN domains d ON (d.id = mi.domain_id)
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    mi.machine_id IS NOT NULL;


CREATE OR REPLACE VIEW dns_records_bmc_dpu_id AS
SELECT
    concat(mt.machine_id, '.bmc.', d.name, '.') AS q_name,
    cast((mt.topology -> 'bmc_info' ->> 'ip') as inet) AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(cast((mt.topology -> 'bmc_info' ->> 'ip') as inet)) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_topologies mt ON ((mi.machine_id = mt.machine_id)
            AND (mi.machine_id = mi.attached_dpu_machine_id))
    JOIN domains d ON (d.id = mi.domain_id)
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    mi.machine_id IS NOT NULL;


CREATE OR REPLACE VIEW dns_records_instance AS
SELECT
    CASE
        WHEN family(ip_addrs.value::inet) = 6 THEN
            concat(replace(ip_addrs.value::text, ':', '-'), '.', d.name, '.')
        ELSE
            concat(regexp_replace(ip_addrs.value::text, '\.', '-', 'g'), '.', d.name, '.')
    END AS q_name,
    ip_addrs.value::inet AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(ip_addrs.value::inet) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    instances i
JOIN
    machine_interfaces mi ON i.machine_id = mi.machine_id
JOIN
    domains d ON mi.domain_id = d.id
CROSS JOIN LATERAL
    jsonb_array_elements(i.network_config::jsonb->'interfaces') AS iface
CROSS JOIN LATERAL
    jsonb_each_text(iface->'ip_addrs') AS ip_addrs
LEFT JOIN
    dns_record_metadata meta ON meta.id = mi.id
LEFT JOIN
    dns_record_types rt ON meta.record_type_id = rt.id
WHERE
    iface->'function_id'->>'type' = 'physical';


CREATE OR REPLACE VIEW dns_records_shortname_combined AS
SELECT
    concat(mi.hostname, '.', d.name, '.') AS q_name,
    mia.address AS resource_record,
    COALESCE(rt.type_name, CASE WHEN family(mia.address) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM
    machine_interfaces mi
    JOIN machine_interface_addresses mia ON (mia.interface_id = mi.id)
    JOIN domains d ON d.id = mi.domain_id AND mi.primary_interface = TRUE
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id;

-- Re-create the combined dns_records view since it was dropped above.
-- dns_records_instance is intentionally NOT included (same as current state).
CREATE OR REPLACE VIEW dns_records AS
SELECT *
FROM
  dns_records_shortname_combined
  FULL JOIN dns_records_adm_combined USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_host_id USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_dpu_id USING (q_name, resource_record, q_type, ttl, domain_id);
