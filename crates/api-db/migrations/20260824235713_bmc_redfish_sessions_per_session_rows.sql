-- One row per Redfish session instead of one per caller identity, so
-- replicas that share a SPIFFE id each hold their own session. A session's
-- @odata.id is unique on its BMC, which makes it the natural key.

-- Block concurrent writers for the rest of this transaction (reads still
-- pass): a running API instance could otherwise re-create a duplicate
-- between the dedup below and the new primary key, failing the migration.
LOCK TABLE bmc_redfish_sessions IN SHARE ROW EXCLUSIVE MODE;

-- A BMC that reuses a session id (e.g. slot-based ids after a reboot) can
-- leave two identities' stale rows naming the same (mac, @odata.id). At most
-- one of them can describe a live session, so keep the newest row.
DELETE FROM bmc_redfish_sessions a
    USING bmc_redfish_sessions b
    WHERE a.bmc_mac_address = b.bmc_mac_address
      AND a.session_odata_id = b.session_odata_id
      AND (a.issued_at, a.spiffe_service_id) < (b.issued_at, b.spiffe_service_id);

ALTER TABLE bmc_redfish_sessions
    DROP CONSTRAINT bmc_redfish_sessions_pkey,
    ADD CONSTRAINT bmc_redfish_sessions_pkey
        PRIMARY KEY (bmc_mac_address, session_odata_id);

-- Redundant now that the primary key leads on bmc_mac_address.
DROP INDEX bmc_redfish_sessions_by_mac;
