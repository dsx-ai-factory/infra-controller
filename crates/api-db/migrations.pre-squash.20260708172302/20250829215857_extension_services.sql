-- Extension Services Table
-- This creates the extension_services table for managing extension service specifications

CREATE TABLE extension_services (
    id                     UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    type                   character varying(64) NOT NULL,
    name                   character varying(64) NOT NULL,
    description            character varying(256) NOT NULL DEFAULT '',
    tenant_organization_id character varying(64) REFERENCES tenants(organization_id),
    
    version_ctr            INTEGER NOT NULL DEFAULT 0, -- Number of versions ever created for the extension service, always incremented
    created                timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated                timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted                timestamp with time zone
);

-- Ensure that the name is unique and case-insensitive for a given tenant
CREATE UNIQUE INDEX extension_services_tenant_lowername_unique
  ON extension_services (tenant_organization_id, lower(name));

CREATE TABLE extension_service_versions (
  service_id     UUID NOT NULL REFERENCES extension_services(id),
  version        VARCHAR(64) NOT NULL,

  data           TEXT NOT NULL,
  has_credential BOOLEAN NOT NULL DEFAULT FALSE,

  created        timestamp with time zone NOT NULL DEFAULT CURRENT_TIMESTAMP,
  deleted        timestamp with time zone,

  PRIMARY KEY (service_id, version)
);
CREATE INDEX idx_extension_service_versions_service_id ON extension_service_versions(service_id);

-- Extension services are stored as part of the instance configuration in the instances table
ALTER TABLE IF EXISTS instances
    ADD COLUMN extension_services_config_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN extension_services_config jsonb DEFAULT '{"service_configs": []}'::jsonb NOT NULL;

CREATE INDEX idx_instances_extension_services_config ON instances USING gin(extension_services_config);
