-- Add migration script here
ALTER TABLE network_prefixes
  ADD COLUMN svi_ip inet
;
