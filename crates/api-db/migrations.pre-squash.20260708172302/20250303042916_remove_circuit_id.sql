ALTER TABLE instance_addresses
    -- Can't set this field as Not NULL immediately.
    -- It should be updated with valid values from network_prefixes table.
    ADD COLUMN segment_id uuid
;

DROP VIEW instance_dhcp_records;

-- Update segment_id field with correct data. 
-- circuit_id is mandatory for tenant network segments, so any row can not contain NULL circuit_id value.
UPDATE instance_addresses
  SET segment_id = (
    SELECT segment_id FROM network_prefixes WHERE instance_addresses.circuit_id = network_prefixes.circuit_id);

ALTER TABLE instance_addresses
    DROP COLUMN circuit_id,
    -- Now set segment_id field as NOT NULL and drop circuit_id field.
    ALTER COLUMN segment_id SET NOT NULL;
;

-- Finally drop last use of circuit_id field from carbide.
ALTER TABLE network_prefixes
    DROP COLUMN circuit_id
;

