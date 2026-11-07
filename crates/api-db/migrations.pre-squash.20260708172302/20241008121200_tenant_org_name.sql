-- Add migration script here
ALTER TABLE
  tenants
ADD COLUMN organization_name TEXT;
