DROP VIEW dpu_machines;
CREATE VIEW dpu_machines AS (
    SELECT machines.id as machine_id,
    machines.vpc_leaf_id as vpc_leaf_id,
    machine_interfaces.id as machine_interfaces_id,
    machine_interfaces.mac_address as mac_address,
    machine_interface_addresses.address as address,
    machine_interfaces.hostname as hostname,
    vpc_resource_leafs.loopback_ip_address as loopback_ip
    FROM machine_interfaces
    LEFT JOIN machines on machine_interfaces.machine_id=machines.id
    INNER JOIN machine_interface_addresses on machine_interface_addresses.interface_id=machine_interfaces.id
    LEFT JOIN vpc_resource_leafs ON vpc_resource_leafs.id=machines.vpc_leaf_id
    WHERE machine_interfaces.attached_dpu_machine_id IS NOT NULL
    AND machine_interfaces.attached_dpu_machine_id = machine_interfaces.machine_id
);

CREATE TABLE resource_pool (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name VARCHAR(32) NOT NULL,
    value VARCHAR(64) NOT NULL,
    owner_type VARCHAR(32) NULL,
    owner_id VARCHAR(64) NULL,
    created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    allocated TIMESTAMPTZ NULL,
	state jsonb NOT NULL DEFAULT ('{}'),
	state_version VARCHAR(64) NOT NULL DEFAULT ('1'),

    UNIQUE(name, value)
);
CREATE INDEX idx_resource_pools_name ON resource_pool(name);

ALTER TABLE network_segments ADD COLUMN vlan_id smallint NULL CHECK (0 <= vlan_id AND vlan_id < 4096) UNIQUE;
