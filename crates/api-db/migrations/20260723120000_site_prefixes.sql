-- Site prefixes need tenant-aware overlap protection. PostgreSQL supplies the
-- CIDR overlap operator through the built-in inet GiST class, while
-- btree_gist supplies equality for the tenant text column in the same
-- exclusion constraint. Fail before changing the schema when the PostgreSQL
-- installation does not provide the extension.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_available_extensions
        WHERE name = 'btree_gist'
    ) THEN
        RAISE EXCEPTION
            'SitePrefix persistence requires the PostgreSQL btree_gist extension';
    END IF;
END
$$;

CREATE EXTENSION IF NOT EXISTS btree_gist WITH SCHEMA public;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_extension extension
        JOIN pg_namespace namespace
            ON namespace.oid = extension.extnamespace
        WHERE extension.extname = 'btree_gist'
          AND namespace.nspname = 'public'
    ) THEN
        RAISE EXCEPTION
            'SitePrefix persistence requires btree_gist in the public schema';
    END IF;
END
$$;

CREATE TYPE site_prefix_authority AS ENUM (
    'configured',
    'tenant_managed'
);

CREATE TYPE site_prefix_routing_scope AS ENUM (
    'datacenter_only'
);

CREATE TYPE site_prefix_lifecycle_state AS ENUM (
    'provisioning',
    'ready',
    'deleting',
    'error'
);

CREATE TABLE site_prefixes (
    id uuid PRIMARY KEY,
    prefix cidr NOT NULL,
    authority site_prefix_authority NOT NULL,
    tenant_organization_id text
        REFERENCES tenants(organization_id) ON DELETE RESTRICT,
    routing_scope site_prefix_routing_scope NOT NULL,
    lifecycle_state site_prefix_lifecycle_state NOT NULL,
    name varchar(256) NOT NULL,
    description varchar(1024) NOT NULL DEFAULT '',
    labels jsonb NOT NULL DEFAULT '{}',
    version varchar(64) NOT NULL,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    updated_at timestamp with time zone NOT NULL DEFAULT now(),
    CONSTRAINT site_prefixes_authority_owner_check CHECK (
        (authority = 'configured' AND tenant_organization_id IS NULL)
        OR
        (authority = 'tenant_managed' AND tenant_organization_id IS NOT NULL)
    ),
    CONSTRAINT site_prefixes_configured_lifecycle_check CHECK (
        authority <> 'configured'
        OR lifecycle_state IN ('ready', 'deleting')
    ),
    CONSTRAINT site_prefixes_tenant_prefix_excl EXCLUDE USING gist (
        tenant_organization_id WITH =,
        prefix inet_ops WITH &&
    ) WHERE (authority = 'tenant_managed')
);

-- Configuration has no caller-provided resource ID today, so the canonical
-- CIDR is the stable reconciliation key for configuration-owned rows. The
-- predicate deliberately permits a tenant-owned row with the same CIDR.
CREATE UNIQUE INDEX site_prefixes_configured_prefix_key
    ON site_prefixes (prefix)
    WHERE authority = 'configured';

CREATE INDEX site_prefixes_tenant_key
    ON site_prefixes (tenant_organization_id, id)
    WHERE authority = 'tenant_managed';
