-- Purpose: Builds on the previous view definitions in 20240124205300_dns_record_views.sql
-- to include the q_type 'A' 

CREATE OR REPLACE VIEW dns_records_adm_combined AS
SELECT
    concat(mi.machine_id, '.adm.', d.name, '.') AS q_name,
    mia.address AS resource_record,
    rt.type_name AS q_type,
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
    rt.type_name as q_type,
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
    rt.type_name as q_type,
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
    concat(regexp_replace(ip_addrs.value::text, '\.', '-', 'g'), '.', d.name, '.') AS q_name,
    ip_addrs.value::inet AS resource_record,
    rt.type_name as q_type,
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
    rt.type_name AS q_type,
    meta.ttl as ttl,
    d.id as domain_id
FROM 
    machine_interfaces mi
    JOIN machine_interface_addresses mia ON (mia.interface_id = mi.id)
    JOIN domains d ON d.id = mi.domain_id AND mi.primary_interface = TRUE
    LEFT JOIN dns_record_metadata meta ON meta.id = mi.id
    LEFT JOIN dns_record_types rt ON meta.record_type_id = rt.id;

CREATE OR REPLACE VIEW dns_records AS
SELECT *
FROM 
  dns_records_shortname_combined
  FULL JOIN dns_records_adm_combined USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_host_id USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_dpu_id USING (q_name, resource_record, q_type, ttl, domain_id);

