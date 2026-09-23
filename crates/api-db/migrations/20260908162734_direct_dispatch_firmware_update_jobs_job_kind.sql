-- Widen the direct-dispatch firmware-job key from bmc_mac to (bmc_mac,
-- job_kind). A device can have more than one in-flight job of distinct kinds (a
-- switch tracks a firmware-object bundle job and an NVOS system-image job
-- independently), so the kind is part of a row's identity and selects which
-- backend query recovers a job's status after a restart.
--
-- Every row written before this migration was a firmware-object job, so that
-- value is a true default for existing rows. Keeping the default also lets a
-- prior-release writer, which inserts without job_kind during a rolling
-- upgrade, still land a valid firmware-object row.
ALTER TABLE direct_dispatch_firmware_update_jobs
    ADD COLUMN job_kind text NOT NULL DEFAULT 'firmware_object';

ALTER TABLE direct_dispatch_firmware_update_jobs
    DROP CONSTRAINT direct_dispatch_firmware_update_jobs_pkey,
    ADD PRIMARY KEY (bmc_mac, job_kind);
