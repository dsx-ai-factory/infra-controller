-- Secondary VTEP addresses are no longer allocated or exposed to DPU agents.
-- Remove both free and allocated values, their configuration snapshot, and the
-- legacy value embedded in managed-host network configuration documents.
DELETE FROM resource_pool
WHERE name = 'secondary-vtep-ip';

DELETE FROM resource_pool_def
WHERE name = 'secondary-vtep-ip';

UPDATE machines
SET network_config = network_config - 'secondary_overlay_vtep_ip'
WHERE network_config ? 'secondary_overlay_vtep_ip';

-- Enforce the new document shape even while older API pods are draining.
ALTER TABLE machines
ADD CONSTRAINT machines_network_config_excludes_secondary_vtep_ip
CHECK (NOT (network_config ? 'secondary_overlay_vtep_ip')) NOT VALID;
