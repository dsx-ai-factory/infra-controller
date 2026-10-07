-- Add migration script here
CREATE TABLE tenants
(
    organization_id text PRIMARY KEY,
    version         VARCHAR(64) NOT NULL
);

CREATE TABLE tenant_keysets
(
    organization_id text,
    keyset_id       text,
    content         jsonb       NOT NULL, --serialized as TenantKeysetContent
    version         VARCHAR(64) NOT NULL,
    PRIMARY KEY (organization_id, keyset_id)
);

