-- Remove the ssh_keys field
-- We now reference keyset IDs from instances instead of directly storing ssh_keys

ALTER TABLE instances DROP COLUMN ssh_keys;
