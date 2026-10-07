-- Add migration script here
ALTER TABLE network_prefixes
  DROP COLUMN vlan_id,
  ADD COLUMN circuit_id text UNIQUE
;
