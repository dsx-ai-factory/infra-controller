-- Active suppression requests for BMC MAC addresses. Each subsystem owns its
-- acknowledgement, and removing a row ends suppression for that subsystem.
CREATE TABLE bmc_suppressions (
    bmc_mac_address MACADDR NOT NULL,
    subsystem TEXT NOT NULL CHECK (
        subsystem IN ('site_explorer', 'dhcp')
    ),
    reason TEXT NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT statement_timestamp(),
    acknowledged_at TIMESTAMPTZ,
    PRIMARY KEY (bmc_mac_address, subsystem)
);
