-- Optional operator-assigned machine name, surfaced to switches in LLDP
-- responses. Existing rows have no name, so the column is nullable.
ALTER TABLE expected_machines ADD COLUMN server_name text;
