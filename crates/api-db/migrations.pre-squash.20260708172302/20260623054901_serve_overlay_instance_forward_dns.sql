-- Serve forward A/AAAA DNS for overlay (DPU-managed) instances.
--
-- An overlay instance's addresses are allocated from its segment's pool into
-- instance_addresses and never reach machine_interface_addresses, so the
-- shortname/adm views never published them -- overlay instances had no forward
-- record. This adds an instance arm to the served dns_records view.
--
-- The published name is the address in the host-naming strategy's IP-derived
-- form (<dashed-address>.<zone>.), the same convention machine interfaces use.
-- Rather than re-derive it in SQL, the hostname is computed once in Rust by the
-- host_naming::address_to_hostname helper -- the single source of truth shared
-- with machine_interfaces.hostname -- and stored on instance_addresses; the view
-- just reads the column. New addresses are populated at allocation time.
--
-- Not published here: rows with no hostname (instance forward DNS has been
-- unserved since the old view was orphaned over a year ago, so existing rows
-- simply stay unpublished -- nothing to retrofit), and host_inband segments --
-- a host_inband address is the host's own interface address, already served by
-- the shortname view.

ALTER TABLE instance_addresses ADD COLUMN hostname varchar(63);

CREATE OR REPLACE VIEW dns_records_instance AS
SELECT
    concat(ia.hostname, '.', d.name, '.') AS q_name,
    ia.address AS resource_record,
    (CASE WHEN family(ia.address) = 6 THEN 'AAAA' ELSE 'A' END)::varchar(10) AS q_type,
    -- Instances carry no per-record TTL metadata; find_record COALESCEs this to
    -- the default (300s), so no dns_record_metadata join is needed here.
    NULL::integer AS ttl,
    d.id AS domain_id
FROM
    instance_addresses ia
    JOIN network_segments ns ON ns.id = ia.segment_id
    JOIN domains d ON d.id = ns.subdomain_id
WHERE
    ia.hostname IS NOT NULL
    AND ns.network_segment_type <> 'host_inband';

-- Re-publish the combined view with the instance arm attached.
DROP VIEW IF EXISTS dns_records;

CREATE OR REPLACE VIEW dns_records AS
SELECT *
FROM
  dns_records_shortname_combined
  FULL JOIN dns_records_adm_combined USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_host_id USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_bmc_dpu_id USING (q_name, resource_record, q_type, ttl, domain_id)
  FULL JOIN dns_records_instance USING (q_name, resource_record, q_type, ttl, domain_id);
