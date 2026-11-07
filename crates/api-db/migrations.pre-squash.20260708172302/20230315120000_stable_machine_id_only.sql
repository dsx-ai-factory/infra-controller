-- Machine IDs are now always stable. We don't support any non-stable IDs anymore


DROP VIEW machine_dhcp_records;
DROP VIEW dpu_machines;
DROP VIEW instance_dhcp_records;

ALTER TABLE instances
    DROP constraint instances_machine_id_fkey;

ALTER TABLE machine_topologies
    DROP constraint machine_topologies_machine_id_fkey;

ALTER TABLE machine_interfaces
    DROP constraint machine_interfaces_machine_id_fkey,
    DROP constraint machine_interfaces_attached_dpu_machine_id_fkey;

ALTER TABLE tags_machine
    DROP constraint fk_tags_machine;

ALTER TABLE machine_console_metadata
    DROP constraint machine_console_metadata_machine_id_fkey;

ALTER TABLE machines
    DROP constraint machines_vpc_leaf_id_fkey;

ALTER TABLE vpc_resource_leafs
    ALTER COLUMN id TYPE VARCHAR(64),
    ALTER COLUMN id DROP DEFAULT;


ALTER TABLE machines
    DROP COLUMN stable_id,
    ALTER COLUMN id TYPE VARCHAR(64),
    ALTER COLUMN id SET NOT NULL,
    ALTER COLUMN id SET DEFAULT 'INVALID_MACHINE',
    ALTER COLUMN vpc_leaf_id TYPE VARCHAR(64),
    ADD CONSTRAINT machines_vpc_leaf_id_fkey FOREIGN KEY (vpc_leaf_id) REFERENCES vpc_resource_leafs(id)
;

ALTER TABLE machine_state_history
    ALTER COLUMN machine_id TYPE VARCHAR(64);

ALTER TABLE machine_topologies
    ALTER COLUMN machine_id TYPE VARCHAR(64),
    ADD CONSTRAINT machine_topologies_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES machines(id);

ALTER TABLE instances
    ALTER COLUMN machine_id TYPE VARCHAR(64),
    ADD CONSTRAINT instances_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES machines(id);

ALTER TABLE machine_interfaces
    ALTER COLUMN machine_id TYPE VARCHAR(64),
    ALTER COLUMN attached_dpu_machine_id TYPE VARCHAR(64),
    ADD CONSTRAINT machine_interfaces_machine_id_fkey FOREIGN KEY(machine_id) REFERENCES machines(id),
    ADD CONSTRAINT machine_interfaces_attached_dpu_machine_id_fkey FOREIGN KEY(attached_dpu_machine_id) REFERENCES machines(id);

ALTER TABLE tags_machine
    ALTER COLUMN target_id TYPE VARCHAR(64),
    ADD CONSTRAINT fk_tags_machine FOREIGN KEY (target_id) REFERENCES machines(id);

ALTER TABLE machine_console_metadata
    ALTER COLUMN machine_id TYPE VARCHAR(64),
    ADD CONSTRAINT machine_console_metadata_machine_id_fkey FOREIGN KEY(machine_id) REFERENCES machines(id);

CREATE VIEW machine_dhcp_records AS (
	SELECT
	machines.id as machine_id,
	machine_interfaces.id as machine_interface_id,
	network_segments.id as segment_id,
	network_segments.subdomain_id as subdomain_id,
	CONCAT(machine_interfaces.hostname,'.', domains.name) as fqdn,
	machine_interfaces.mac_address as mac_address,
	machine_interface_addresses.address as address,
	network_segments.mtu as mtu,
	network_prefixes.prefix as prefix,
	network_prefixes.gateway as gateway
	FROM machine_interfaces
	LEFT JOIN machines ON machine_interfaces.machine_id=machines.id
	INNER JOIN network_segments ON network_segments.id=machine_interfaces.segment_id
	INNER JOIN network_prefixes ON network_prefixes.segment_id=network_segments.id
	INNER JOIN machine_interface_addresses ON machine_interface_addresses.interface_id=machine_interfaces.id
	INNER JOIN domains on domains.id = machine_interfaces.domain_id
	WHERE address << prefix
);

CREATE VIEW dpu_machines AS (
    SELECT machines.id as machine_id,
    machines.vpc_leaf_id as vpc_leaf_id,
    machine_interfaces.id as machine_interfaces_id,
    machine_interfaces.mac_address as mac_address,
    machine_interface_addresses.address as address,
    machine_interfaces.hostname as hostname
    FROM machine_interfaces
    LEFT JOIN machines on machine_interfaces.machine_id=machines.id
    INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id=machine_interfaces.id
    WHERE machine_interfaces.attached_dpu_machine_id IS NOT NULL
    AND machine_interfaces.attached_dpu_machine_id = machine_interfaces.machine_id
);

CREATE VIEW instance_dhcp_records AS (
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

-- Cleans up a Machine by Machine ID
create or replace procedure cleanup_machine_by_id(deletion_machine_id varchar(64))
 language plpgsql as $$
 begin
  update machine_interfaces set machine_id = null, attached_dpu_machine_id = null where machine_id = deletion_machine_id;
  delete from machine_topologies where machine_id = deletion_machine_id;
  delete from machines where id = deletion_machine_id;
  delete from vpc_resource_leafs where id = deletion_machine_id;
end
$$;

-- Cleans ups a Machine by hostname
create or replace procedure cleanup_machine(host varchar(63))
language plpgsql as $$
declare
  delete_id VARCHAR(64);
begin
  select machine_id into delete_id from machine_interfaces where hostname = host;
  call cleanup_machine_by_id(delete_id);
end
$$;