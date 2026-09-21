-- A direct-dispatch switch update that targets both firmware-object components
-- and NVOS cannot submit both at once: RMS holds one job per node, so the
-- system-image apply is rejected while the firmware-object job is still active.
-- The system-image phase is staged here when the firmware-object job is
-- submitted, and dispatched by a later status poll once that job is terminal.
--
-- The artifact access token is deliberately not stored. It stays in the
-- backend's memory, matching the rack-maintenance path, which keeps it in the
-- credential store rather than the database. requires_access_token records
-- whether the staged update needs one, so a poll after a restart fails it
-- explicitly instead of submitting it unauthenticated.
CREATE TABLE switch_pending_system_image_updates (
    bmc_mac macaddr PRIMARY KEY,
    config_json text NOT NULL,
    requires_access_token boolean NOT NULL,
    created timestamp with time zone DEFAULT now() NOT NULL
);
