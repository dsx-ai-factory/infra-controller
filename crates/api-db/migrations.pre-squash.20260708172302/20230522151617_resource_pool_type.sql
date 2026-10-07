CREATE TYPE resource_pool_type AS ENUM ('integer', 'ipv4');

ALTER TABLE resource_pool ADD COLUMN value_type resource_pool_type NOT NULL;
UPDATE resource_pool SET value_type = 'ipv4' WHERE name = 'lo-ip';
UPDATE resource_pool SET value_type = 'integer' WHERE name = 'vni';
UPDATE resource_pool SET value_type = 'integer' WHERE name = 'vlan-id';
UPDATE resource_pool SET value_type = 'integer' WHERE name = 'pkey';
