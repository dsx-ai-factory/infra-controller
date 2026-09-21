-- Holds the NVOS system-image phase of a direct-dispatch switch update between
-- the two submissions such an update has to be split into. RMS runs one job per
-- node, so the firmware-object apply goes first and the system-image apply
-- waits here until a status poll observes that job finish.
--
-- One row per switch, keyed by BMC MAC, the switch's stable identity across
-- ingestion. The row is the phase's only recovery point until its RMS job id
-- lands in direct_dispatch_firmware_update_jobs, so `claimed` leases it to the
-- one poll that is dispatching it, and `failure` retains a phase that ended
-- without dispatching so later polls keep reporting it.
--
-- The artifact access token is deliberately not stored. It stays in the
-- backend's memory, matching the rack-maintenance path, which keeps it in the
-- credential store rather than the database. requires_access_token records
-- whether the phase needs one, so a poll after a restart fails it explicitly
-- instead of submitting it unauthenticated.
CREATE TABLE switch_staged_system_image_updates (
    bmc_mac macaddr PRIMARY KEY,
    config_json text NOT NULL,
    requires_access_token boolean NOT NULL,
    claimed timestamp with time zone,
    failure text,
    created timestamp with time zone DEFAULT now() NOT NULL
);
