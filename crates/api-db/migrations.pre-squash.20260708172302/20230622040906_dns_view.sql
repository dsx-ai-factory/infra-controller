-- Add migration script here
DROP VIEW IF EXISTS dns_records;
CREATE OR REPLACE VIEW dns_records AS (
  SELECT
  CONCAT(CONCAT(hostname,'.', name), '.') as q_name, address as resource_record
  from machine_interfaces
  INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id = machine_interfaces.id
  INNER JOIN domains on domains.id = machine_interfaces.domain_id AND primary_interface=true
);

