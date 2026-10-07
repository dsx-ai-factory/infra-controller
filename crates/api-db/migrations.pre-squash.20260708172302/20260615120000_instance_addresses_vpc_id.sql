ALTER TABLE instance_addresses
ADD COLUMN vpc_id uuid REFERENCES vpcs(id);

UPDATE instance_addresses ia
SET vpc_id = ns.vpc_id
FROM network_segments ns
WHERE ia.segment_id = ns.id
AND ia.vpc_id IS NULL;

-- Make non-null now that we've filled it in.
ALTER TABLE instance_addresses ALTER COLUMN vpc_id SET NOT NULL;

CREATE INDEX instance_addresses_vpc_id_idx ON instance_addresses(vpc_id);
CREATE INDEX instance_addresses_instance_id_vpc_id_idx ON instance_addresses(instance_id, vpc_id);
