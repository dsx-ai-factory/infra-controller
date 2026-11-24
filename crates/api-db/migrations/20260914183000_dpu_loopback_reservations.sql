-- Authoritative loopback intent, selected by DPU OS/admin MAC.
CREATE TABLE expected_dpu_loopback_reservations (
    host_bmc_mac_address macaddr NOT NULL
        REFERENCES expected_machines (bmc_mac_address) ON DELETE CASCADE,
    dpu_oob_net0_mac_address macaddr NOT NULL UNIQUE,
    loopback_ipv4 inet UNIQUE DEFERRABLE INITIALLY IMMEDIATE,
    loopback_ipv6 inet UNIQUE DEFERRABLE INITIALLY IMMEDIATE,
    CHECK (loopback_ipv4 IS NOT NULL OR loopback_ipv6 IS NOT NULL),
    -- Each column holds only its own address family, so a reader never has to
    -- reconcile a wrong-family value against the field it was stored in.
    CHECK (loopback_ipv4 IS NULL OR family(loopback_ipv4) = 4),
    CHECK (loopback_ipv6 IS NULL OR family(loopback_ipv6) = 6)
);

-- Direct discovery resolves a reservation by the DPU OS/admin MAC alone, and cascade
-- deletes match on the owning host, so index both access paths.
CREATE INDEX expected_dpu_loopback_reservations_host_bmc_mac_address_idx
    ON expected_dpu_loopback_reservations (host_bmc_mac_address);
