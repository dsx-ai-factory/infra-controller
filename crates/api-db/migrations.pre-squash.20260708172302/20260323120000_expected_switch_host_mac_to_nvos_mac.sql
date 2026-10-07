-- Migrate host_mac_address from metadata_labels JSONB to the nvos_mac_addresses column.
-- Only rows where nvos_mac_addresses is NULL and metadata_labels contains a valid
-- host_mac_address key are updated, so previously-set nvos_mac_addresses values are preserved.
UPDATE
  expected_switches
SET
  nvos_mac_addresses = ARRAY [(metadata_labels->>'host_mac_address')::macaddr]
WHERE
  (
    nvos_mac_addresses IS NULL
    OR nvos_mac_addresses = '{}'
  )
  AND metadata_labels ->> 'host_mac_address' IS NOT NULL;

-- Remove the now-redundant host_mac_address key from metadata_labels.
UPDATE
  expected_switches
SET
  metadata_labels = metadata_labels - 'host_mac_address'
WHERE
  metadata_labels ? 'host_mac_address';

-- Reset any non-ready switches back to initializing so they re-run the
-- state machine from the beginning after the migration.
UPDATE
  switches
SET
  controller_state = '{"state":"created"}'
WHERE
  controller_state ->> 'state' != 'ready';