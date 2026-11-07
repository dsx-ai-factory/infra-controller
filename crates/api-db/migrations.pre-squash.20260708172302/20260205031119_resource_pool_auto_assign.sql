-- Add new auto_assign column to resource pools.
-- Can default to true to match original resource pool behavior.
ALTER TABLE resource_pool ADD COLUMN auto_assign boolean DEFAULT 't';

