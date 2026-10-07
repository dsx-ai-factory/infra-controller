
-- Add status field to vpcs table.
ALTER TABLE vpcs ADD COLUMN status JSONB NOT NULL DEFAULT '{}';

-- Add a unique "constraint" similar to the one on
-- the original VNI column.
CREATE UNIQUE INDEX "unique_active_vpc_status_vni" ON vpcs(((status->>'vni')::integer)) WHERE deleted is NULL;

-- Set status VNI to the VNIs that are currently assigned.
UPDATE vpcs v SET status = jsonb_set(status, '{vni}', (v.vni::text)::jsonb, true);

-- Clear the VNI field so it's clear that the VNI in status
-- was auto-assigned.
UPDATE vpcs v SET vni = null;

-- Drop dpa_vni column.
-- VPC VNI is used now.
ALTER TABLE vpcs DROP COLUMN dpa_vni;

-- Clean up any records for the old column.
DELETE FROM resource_pool WHERE name='dpa-vni';
