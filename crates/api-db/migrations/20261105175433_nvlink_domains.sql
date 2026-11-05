-- Persist Domain intent and controller progress independently of physical Rack state.
CREATE TABLE nvlink_domains (
    id uuid PRIMARY KEY,
    rack_group_id varchar(128) NOT NULL REFERENCES expected_rack_groups(rack_group_id),
    domain_profile_id text NOT NULL,
    metadata_name varchar(256) NOT NULL,
    metadata_description text NOT NULL,
    metadata_labels jsonb NOT NULL,
    version varchar(64) NOT NULL,
    controller_state jsonb NOT NULL,
    controller_state_version varchar(64) NOT NULL,
    controller_state_outcome jsonb,
    nmxc_domain_id uuid,
    managed boolean NOT NULL,
    created timestamptz NOT NULL DEFAULT now(),
    updated timestamptz NOT NULL DEFAULT now(),
    deleted timestamptz
);
CREATE INDEX nvlink_domains_rack_group_id_idx ON nvlink_domains(rack_group_id);
CREATE UNIQUE INDEX nvlink_domains_managed_group_idx ON nvlink_domains(rack_group_id) WHERE managed;

CREATE TABLE nvlink_domain_members (
    domain_id uuid NOT NULL REFERENCES nvlink_domains(id),
    component_type text NOT NULL CHECK (component_type IN ('Compute', 'Switch')),
    rack_id varchar(128) NOT NULL,
    tray_index integer NOT NULL CHECK (tray_index >= 0),
    PRIMARY KEY (rack_id, component_type, tray_index)
);
CREATE INDEX nvlink_domain_members_domain_id_idx ON nvlink_domain_members(domain_id);
