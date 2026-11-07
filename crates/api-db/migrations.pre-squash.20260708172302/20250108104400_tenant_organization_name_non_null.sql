-- FORGE-5085
-- The API proto field for tenants.metadata.name is non-optional, which plumbs into tenants.organization_name. Nulls
-- cause server errors, so make the DB match the API expectations: we don't want to inadvertently get nulls in the
-- future.

-- Since the API doesn't allow nulls, we should only see nulls here for data older than the API change (hopefully
-- nowhere at this point.)
UPDATE tenants SET organization_name = 'Unknown Organization' WHERE organization_name IS NULL;

ALTER TABLE tenants ALTER COLUMN organization_name SET NOT NULL;