ALTER TABLE IF EXISTS instances 
ADD COLUMN hostname VARCHAR(64) NULL;

CREATE UNIQUE INDEX unique_org_hostname
ON instances (tenant_org, hostname)
WHERE hostname IS NOT NULL;

ALTER VIEW dns_records RENAME TO dns_records_machines;

CREATE VIEW dns_records_instance AS
SELECT 
    concat(regexp_replace(ip_addrs.value::text, '\.', '-', 'g'), '.', d.name, '.') AS q_name,
    ip_addrs.value::inet AS resource_record
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
WHERE 
    iface->'function_id'->>'type' = 'physical';

CREATE VIEW dns_records AS
SELECT
    *
FROM
    dns_records_machines
    FULL JOIN dns_records_instance USING (q_name, resource_record);