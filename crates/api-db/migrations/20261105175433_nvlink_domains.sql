-- Evolve existing declarations without inventing locations or lifecycle state.
ALTER TABLE expected_nvlink_domains RENAME TO nvlink_domains;
ALTER TABLE expected_nvlink_domain_members RENAME TO nvlink_domain_members;

ALTER TABLE nvlink_domains
    ADD COLUMN domain_profile_id text,
    ADD COLUMN metadata_description text NOT NULL DEFAULT '',
    ADD COLUMN metadata_labels jsonb NOT NULL DEFAULT '{}',
    ADD COLUMN version varchar(64),
    ADD COLUMN controller_state jsonb,
    ADD COLUMN controller_state_version varchar(64),
    ADD COLUMN controller_state_outcome jsonb,
    ADD COLUMN nmxc_domain_id uuid,
    ADD COLUMN managed boolean NOT NULL DEFAULT false,
    ADD COLUMN created timestamptz,
    ADD COLUMN updated timestamptz,
    ADD COLUMN deleted timestamptz;

CREATE UNIQUE INDEX nvlink_domains_managed_group_idx ON nvlink_domains(rack_group_id) WHERE managed;

ALTER TABLE nvlink_domain_members
    DROP CONSTRAINT expected_nvlink_domain_members_pkey,
    ALTER COLUMN bmc_mac_address DROP NOT NULL,
    ADD CONSTRAINT expected_nvlink_domain_members_pkey UNIQUE (bmc_mac_address),
    ADD COLUMN component_type text CHECK (component_type IN ('Compute', 'Switch')),
    ADD COLUMN rack_id varchar(128),
    ADD COLUMN tray_index integer CHECK (tray_index >= 0),
    ADD CONSTRAINT nvlink_domain_members_identity_check CHECK (
        (bmc_mac_address IS NOT NULL AND component_type IS NULL AND rack_id IS NULL AND tray_index IS NULL)
        OR (bmc_mac_address IS NULL AND component_type IS NOT NULL AND rack_id IS NOT NULL AND tray_index IS NOT NULL)
    ),
    ADD CONSTRAINT nvlink_domain_members_location_key UNIQUE (rack_id, component_type, tray_index);

-- Old binaries continue using the same rows through these writable views.
CREATE VIEW expected_nvlink_domains AS
    SELECT id, rack_group_id, name FROM nvlink_domains
    WHERE domain_profile_id IS NULL
    WITH LOCAL CHECK OPTION;
CREATE VIEW expected_nvlink_domain_members AS
    SELECT bmc_mac_address, domain_id FROM nvlink_domain_members
    WHERE bmc_mac_address IS NOT NULL
    WITH LOCAL CHECK OPTION;
