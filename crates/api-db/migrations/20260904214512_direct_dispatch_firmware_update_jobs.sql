-- Unify direct-dispatch (--bypass-state-controller) firmware-update job IDs in
-- one table keyed by the device's BMC MAC, its stable identity across
-- ingestion. Recording the backend job here lets get_firmware_status recover it
-- after a nico-api restart clears the in-memory job map, for both ingested and
-- pre-ingestion devices, without splitting the state across the machines and
-- explored_endpoints rows.
CREATE TABLE direct_dispatch_firmware_update_jobs (
    bmc_mac macaddr PRIMARY KEY,
    job_id text NOT NULL,
    created timestamp with time zone DEFAULT now() NOT NULL
);
