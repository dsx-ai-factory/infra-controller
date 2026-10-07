-- Add migration script here
DROP VIEW instance_dhcp_records;
DROP TABLE instance_subnet_addresses;
DROP TABLE instance_subnet_events;
DROP TABLE instance_subnets;
CREATE TABLE instance_addresses (
  id uuid DEFAULT gen_random_uuid() NOT NULL,
  instance_id uuid NOT NULL,
  circuit_id text NOT NULL,
  address inet NOT NULL,

  FOREIGN KEY (instance_id) REFERENCES instances(id)
);

-- Make sure there''s at most one address for a family per circuit id.
CREATE UNIQUE INDEX one_address_for_a_family ON instance_addresses (instance_id, circuit_id, family(address));

CREATE OR REPLACE VIEW instance_dhcp_records AS (
  SELECT 
    machines.id as machine_id, 
    machine_interfaces.id as machine_interface_id, 
    network_segments.id as segment_id, 
    network_segments.subdomain_id as subdomain_id, 
    CASE 
      WHEN subdomain_id IS NOT NULL THEN 
        CONCAT(machine_interfaces.hostname,'.', (SELECT domains.name FROM domains WHERE id=subdomain_id))
    ELSE 
      CONCAT(machine_interfaces.hostname,'.unknowndomain') 
    END fqdn, 
    instance_addresses.address as address, 
    network_segments.mtu as mtu, 
    network_prefixes.prefix as prefix, 
    network_prefixes.gateway as gateway, 
    network_prefixes.circuit_id as circuit_id 
  FROM 
    instances i 
    INNER JOIN LATERAL jsonb_array_elements(i.network_config::jsonb->'interfaces') netconf(element) ON TRUE 
    INNER JOIN machines ON i.machine_id=machines.id 
    INNER JOIN machine_interfaces ON machine_interfaces.machine_id = machines.id 
    INNER JOIN domains on domains.id = machine_interfaces.domain_id 
    INNER JOIN network_segments ON network_segments.id = (netconf.element->>'network_segment_id')::uuid
    INNER JOIN network_prefixes ON network_prefixes.segment_id=network_segments.id 
    INNER JOIN instance_addresses ON instance_addresses.instance_id = i.id 
  WHERE 
    address << prefix
);
