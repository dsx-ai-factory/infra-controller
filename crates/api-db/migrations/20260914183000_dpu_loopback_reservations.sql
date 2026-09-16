-- Operator-declared deterministic DPU underlay loopback addresses, keyed by the
-- DPU pairing serial number. A host expected machine pairs with one or more
-- DPUs, so reservations are a child of expected_machines and cascade with it.
-- The serial and each per-family address are unique site-wide so a reservation
-- names exactly one DPU and one loopback address the database can enforce.
CREATE TABLE expected_dpu_loopback_reservations (
    bmc_mac_address macaddr NOT NULL
        REFERENCES expected_machines (bmc_mac_address) ON DELETE CASCADE,
    dpu_serial_number text NOT NULL UNIQUE,
    loopback_ipv4 inet,
    loopback_ipv6 inet,
    CHECK (loopback_ipv4 IS NOT NULL OR loopback_ipv6 IS NOT NULL),
    -- Each column holds only its own address family, so a reader never has to
    -- reconcile a wrong-family value against the field it was stored in.
    CHECK (loopback_ipv4 IS NULL OR family(loopback_ipv4) = 4),
    CHECK (loopback_ipv6 IS NULL OR family(loopback_ipv6) = 6)
);

-- Direct discovery resolves a reservation by the DPU serial alone, and cascade
-- deletes match on the owning host, so index both access paths.
CREATE INDEX expected_dpu_loopback_reservations_bmc_mac_address_idx
    ON expected_dpu_loopback_reservations (bmc_mac_address);

-- A loopback address belongs to at most one DPU across the site.
CREATE UNIQUE INDEX expected_dpu_loopback_reservations_loopback_ipv4_key
    ON expected_dpu_loopback_reservations (loopback_ipv4)
    WHERE loopback_ipv4 IS NOT NULL;

CREATE UNIQUE INDEX expected_dpu_loopback_reservations_loopback_ipv6_key
    ON expected_dpu_loopback_reservations (loopback_ipv6)
    WHERE loopback_ipv6 IS NOT NULL;
