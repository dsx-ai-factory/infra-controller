-- Operating system definitions in carbide-core (source of truth per design 0076).
-- Supports iPXE and template-based iPXE OS definition variants.
-- OS images have pre-existing storage and are not stored in this table.
-- Type values match bare-metal-manager-rest conventions for sync compatibility.
-- Relationship and tables similar to bare-metal-manager-rest; instance refers to an OS ID and can override some values.

CREATE TABLE IF NOT EXISTS operating_systems (
    id                      uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name                    VARCHAR(256) NOT NULL,
    description             TEXT,
    org                     VARCHAR(256) NOT NULL,
    type                    VARCHAR(64) NOT NULL,
    status                  VARCHAR(64) NOT NULL DEFAULT 'PROVISIONING',
    is_active               BOOLEAN NOT NULL DEFAULT true,
    allow_override          BOOLEAN NOT NULL DEFAULT true,
    phone_home_enabled      BOOLEAN NOT NULL DEFAULT false,
    user_data               TEXT,
    created                 TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated                 TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted                 TIMESTAMPTZ,

    -- Variant: iPXE (inline script)
    ipxe_script             TEXT,

    -- Variant: ipxe_os_definition (template-based)
    ipxe_template_id        VARCHAR(256),
    ipxe_parameters         jsonb,
    ipxe_artifacts          jsonb,
    ipxe_definition_hash    VARCHAR(64),

    CONSTRAINT operating_systems_ipxe_variant_check
        CHECK ((ipxe_script IS NOT NULL) != (ipxe_template_id IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS operating_systems_org_idx ON operating_systems(org) WHERE deleted IS NULL;
CREATE INDEX IF NOT EXISTS operating_systems_type_idx ON operating_systems(type) WHERE deleted IS NULL;
CREATE INDEX IF NOT EXISTS operating_systems_is_active_idx ON operating_systems(is_active) WHERE deleted IS NULL;

-- Instance may refer to an operating system (design 0076). When set, overrides
-- (os_user_data, os_ipxe_script, etc.) apply on top of the OS. When NULL, OS is
-- derived from instance columns only.

ALTER TABLE instances
    ADD COLUMN IF NOT EXISTS operating_system_id uuid REFERENCES operating_systems(id);

CREATE INDEX IF NOT EXISTS instances_operating_system_id_idx ON instances(operating_system_id);
