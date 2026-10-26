-- Keep a configured VPC's identity stable across startup retries.
CREATE TABLE configured_vpc_creation (
    name TEXT PRIMARY KEY,
    vpc_id UUID NOT NULL UNIQUE,
    tenant_organization_id TEXT NOT NULL
);
