-- Add migration script here

-- Add tenant ID to instances
ALTER TABLE IF EXISTS instances
    ADD COLUMN tenant_org text DEFAULT ('UNKNOWN')
;
