-- Create indexes for tenant org id
CREATE INDEX IF NOT EXISTS ib_partitions_organization_id_idx ON ib_partitions (organization_id);
CREATE INDEX IF NOT EXISTS instances_tenant_org_idx ON instances (tenant_org);
CREATE INDEX IF NOT EXISTS vpcs_organization_id_idx ON vpcs (organization_id);