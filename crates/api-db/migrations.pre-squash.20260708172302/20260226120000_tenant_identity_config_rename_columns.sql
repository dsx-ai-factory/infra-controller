-- Rename and alter tenant_identity_config columns.
-- encryption_key_id: replaces master_key_id for clarity (encrypts both signing keys and auth config).
-- organization_id TYPE TEXT: align with tenants(organization_id). encryption_key_id remains VARCHAR(255).

ALTER TABLE tenant_identity_config RENAME COLUMN master_key_id TO encryption_key_id;
ALTER TABLE tenant_identity_config ALTER COLUMN organization_id TYPE TEXT;
