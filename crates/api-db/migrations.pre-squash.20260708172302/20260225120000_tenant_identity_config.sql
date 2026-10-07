-- Tenant identity config table for SPIFFE JWT-SVID machine identity.
-- Stores per-org identity config, signing key pairs, and optional token delegation.
-- Private key is encrypted with a master key.
-- Token delegation columns are nullable when an org does not use delegation.

-- this line will be removed before merging into main branch.
--DROP TABLE IF EXISTS tenant_identity_config CASCADE;

CREATE TYPE token_delegation_auth_method_t AS ENUM ('none', 'client_secret_basic');

CREATE TABLE tenant_identity_config (
    organization_id   VARCHAR(255) PRIMARY KEY REFERENCES tenants(organization_id) ON DELETE CASCADE,
    -- Identity config (from PUT identity/config)
    issuer                   VARCHAR(512) NOT NULL,
    default_audience         VARCHAR(255) NOT NULL,
    allowed_audiences        JSONB NOT NULL,
    token_ttl_sec            INTEGER NOT NULL,
    subject_prefix           VARCHAR(255) NOT NULL,
    enabled                  BOOLEAN NOT NULL DEFAULT TRUE,
    created_at               TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Signing key (generated on first PUT identity/config)
    encrypted_signing_key    TEXT NOT NULL,
    signing_key_public       VARCHAR(255) NOT NULL,
    key_id                   VARCHAR(255) NOT NULL,
    algorithm                VARCHAR(255) NOT NULL,
    master_key_id            VARCHAR(255) NOT NULL,
    -- Token delegation (from PUT identity/token-delegation, optional)
    -- auth_method: none, client_secret_basic
    -- encrypted_auth_method_config: encrypted blob (TEXT). API uses auth_method_config.
    token_endpoint               VARCHAR(512),
    auth_method                  token_delegation_auth_method_t,
    encrypted_auth_method_config  TEXT,
    subject_token_audience       VARCHAR(255),
    token_delegation_created_at  TIMESTAMPTZ
);
