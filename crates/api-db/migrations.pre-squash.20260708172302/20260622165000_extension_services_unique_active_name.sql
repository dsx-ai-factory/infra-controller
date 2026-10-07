-- The old unique index includes soft-deleted extesnion service rows, which
-- prevents a tenant from reusing a service name after the original service
-- is deleted. 
-- Replace the old index with a partial unique index so uniqueness constraint
-- only applies to non-deleted extension services to allow recreate using same
-- name after deletion.
DROP INDEX IF EXISTS extension_services_tenant_lowername_unique;

CREATE UNIQUE INDEX extension_services_tenant_lowername_unique
  ON extension_services (tenant_organization_id, lower(name))
  WHERE deleted IS NULL;
