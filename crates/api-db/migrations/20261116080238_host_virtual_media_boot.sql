-- Keep ownership independent of transient discovered-machine rows. Restrict
-- inventory deletion while an externally visible boot policy still exists.
CREATE TABLE host_boot_intents (
    expected_machine_id uuid PRIMARY KEY REFERENCES expected_machines(id),
    bmc_mac_address macaddr NOT NULL UNIQUE,
    boot_mac_address macaddr NOT NULL UNIQUE,
    intent jsonb NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 0)
);
