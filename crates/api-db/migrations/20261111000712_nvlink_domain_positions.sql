-- Domain membership identifies tray positions within a rack group.
ALTER TABLE expected_nvlink_domains RENAME TO nvlink_domains;
ALTER TABLE nvlink_domains ADD UNIQUE (id, rack_group_id);
ALTER TABLE expected_nvlink_domain_members RENAME TO nvlink_domain_members;
ALTER TABLE nvlink_domain_members
    DROP COLUMN bmc_mac_address,
    ADD COLUMN rack_group_id varchar(128) NOT NULL,
    ADD COLUMN rack_index bigint NOT NULL CHECK (rack_index BETWEEN 0 AND 4294967295),
    ADD COLUMN slot_index bigint NOT NULL CHECK (slot_index BETWEEN 0 AND 4294967295),
    ADD COLUMN component_type text NOT NULL CHECK (component_type IN ('Compute', 'Switch')),
    -- Include rack_group_id to enforce exclusive tray membership across domains.
    ADD PRIMARY KEY (rack_group_id, rack_index, slot_index, component_type),
    ADD FOREIGN KEY (domain_id, rack_group_id)
        REFERENCES nvlink_domains (id, rack_group_id) ON DELETE CASCADE;
