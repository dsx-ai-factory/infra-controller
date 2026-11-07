-- Tracks the outstanding Redfish sessions
CREATE TABLE bmc_redfish_sessions (
    spiffe_service_id TEXT        NOT NULL,
    bmc_mac_address   macaddr     NOT NULL,
    session_odata_id  TEXT        NOT NULL,
    issued_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (spiffe_service_id, bmc_mac_address)
);

CREATE INDEX bmc_redfish_sessions_by_mac
    ON bmc_redfish_sessions (bmc_mac_address);
