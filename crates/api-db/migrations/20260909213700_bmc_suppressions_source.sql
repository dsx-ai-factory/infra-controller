-- Independent callers (decommissioning, BMC credential rotation, factory
-- reset) each own a row so one resume cannot un-suppress another still-active
-- request for the same MAC and subsystem.
ALTER TABLE bmc_suppressions
    ADD COLUMN source TEXT;

UPDATE bmc_suppressions
SET source = 'bmc_credential_rotation'
WHERE reason = 'bmc_credential_rotation';

UPDATE bmc_suppressions
SET source = 'factory_reset_bmc'
WHERE reason = 'factory_reset_bmc';

UPDATE bmc_suppressions
SET source = 'decommissioning'
WHERE source IS NULL;

ALTER TABLE bmc_suppressions
    ADD CONSTRAINT bmc_suppressions_source_check
    CHECK (
        source IN (
            'decommissioning',
            'bmc_credential_rotation',
            'factory_reset_bmc'
        )
    );

ALTER TABLE bmc_suppressions
    ALTER COLUMN source SET NOT NULL;

ALTER TABLE bmc_suppressions
    DROP CONSTRAINT bmc_suppressions_pkey;

ALTER TABLE bmc_suppressions
    ADD PRIMARY KEY (bmc_mac_address, subsystem, source);
