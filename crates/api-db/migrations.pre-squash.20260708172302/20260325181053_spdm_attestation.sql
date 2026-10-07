-- Add migration script here
ALTER TABLE spdm_machine_devices_attestation
ADD COLUMN IF NOT EXISTS completed_at timestamptz,
ADD COLUMN IF NOT EXISTS started_at timestamptz NOT NULL,
ADD COLUMN IF NOT EXISTS cancelled_at timestamptz;

-- update the table name and the corresponding triggers
ALTER TABLE spdm_machine_attestation_history
ADD COLUMN IF NOT EXISTS device_id VARCHAR NOT NULL;

ALTER TABLE spdm_machine_attestation_history
RENAME TO spdm_device_attestation_history;

CREATE OR REPLACE FUNCTION spdm_machine_attestation_history_keep_limit()
RETURNS TRIGGER AS
$body$
BEGIN
    DELETE FROM spdm_device_attestation_history
    WHERE machine_id = NEW.machine_id
      AND id NOT IN (
          SELECT id
          FROM spdm_device_attestation_history
          WHERE machine_id = NEW.machine_id
          ORDER BY id DESC
          LIMIT 250
      );
    RETURN NULL;
END;
$body$
LANGUAGE plpgsql;

ALTER FUNCTION spdm_machine_attestation_history_keep_limit()
RENAME TO spdm_device_attestation_history_keep_limit;

ALTER TRIGGER t_spdm_machine_attestation_history_keep_limit
ON spdm_device_attestation_history
RENAME TO t_spdm_device_attestation_history_keep_limit;
