-- Tracks the last time a MachineInterface has been deleted
-- This table is supposed to only contain a single row with id=1
CREATE TABLE machine_interfaces_deletion (
    id INT PRIMARY KEY NOT NULL DEFAULT(1) CHECK (id = 1),
    last_deletion TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO machine_interfaces_deletion(id, last_deletion) VALUES(1, NOW());

DROP VIEW machine_dhcp_records;
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
    network_prefixes.gateway as gateway,
    machine_interfaces_deletion.last_deletion as last_invalidation_time
    FROM machine_interfaces
    LEFT JOIN machines ON machine_interfaces.machine_id=machines.id
    INNER JOIN network_segments ON network_segments.id=machine_interfaces.segment_id
    INNER JOIN network_prefixes ON network_prefixes.segment_id=network_segments.id
    INNER JOIN machine_interface_addresses ON machine_interface_addresses.interface_id=machine_interfaces.id
    INNER JOIN domains on domains.id = machine_interfaces.domain_id
    INNER JOIN machine_interfaces_deletion ON machine_interfaces_deletion.id = 1
    WHERE address << prefix
);
