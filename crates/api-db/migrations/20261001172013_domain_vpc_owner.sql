-- A domain may be owned by a VPC. Existing domains stay infrastructure-owned.
ALTER TABLE domains ADD COLUMN vpc_id uuid REFERENCES vpcs(id);

-- Deleting a domain lets its VPC own another one.
CREATE UNIQUE INDEX domains_live_vpc_zone_key ON domains(vpc_id)
    WHERE vpc_id IS NOT NULL AND deleted IS NULL;

-- Forward names must also be unique across all owners, so replace the index
-- for reverse zones with one covering every live domain.
DROP INDEX domains_live_reverse_zone_name_key;
CREATE UNIQUE INDEX domains_live_name_key ON domains(lower(rtrim(name, '.')))
    WHERE deleted IS NULL;
