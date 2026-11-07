-- Repeat the cleanup in case stale data was written before the unvalidated
-- constraint was committed, then validate it in a separate transaction so the
-- validation scan does not retain the ADD CONSTRAINT ACCESS EXCLUSIVE lock.
UPDATE machines
SET network_config = network_config - 'secondary_overlay_vtep_ip'
WHERE network_config ? 'secondary_overlay_vtep_ip';

ALTER TABLE machines
VALIDATE CONSTRAINT machines_network_config_excludes_secondary_vtep_ip;
