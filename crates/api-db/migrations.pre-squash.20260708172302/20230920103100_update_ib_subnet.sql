-- add organization_id column
ALTER TABLE ib_subnets ADD COLUMN organization_id TEXT NOT NULL DEFAULT '';

-- fill in the organization_id for existing ib subnet
UPDATE ib_subnets SET organization_id = vpcs.organization_id FROM vpcs WHERE ib_subnets.vpc_id = vpcs.id;

-- remove vpc column
ALTER TABLE ib_subnets DROP COLUMN vpc_id;

