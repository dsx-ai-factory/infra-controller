-- Add migration script here
ALTER TABLE network_prefixes
  ADD COLUMN vlan_id int UNIQUE
;
