-- Keep administrator-declared NVLink membership independent of observed domains.
CREATE TABLE expected_nvlink_domains (
    id uuid PRIMARY KEY,
    rack_group_id varchar(128) NOT NULL REFERENCES expected_rack_groups(rack_group_id),
    name varchar(256)
);
CREATE INDEX expected_nvlink_domains_rack_group_id_idx ON expected_nvlink_domains(rack_group_id);

CREATE TABLE expected_nvlink_domain_members (
    bmc_mac_address macaddr PRIMARY KEY,
    domain_id uuid NOT NULL REFERENCES expected_nvlink_domains(id) ON DELETE CASCADE
);
CREATE INDEX expected_nvlink_domain_members_domain_id_idx ON expected_nvlink_domain_members(domain_id);
