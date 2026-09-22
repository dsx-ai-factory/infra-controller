-- Add lockdown_ikm_credential_rotation_requested column to machines table.
-- lockdown_ikm_credential_rotation_requested: a flag maintained per host
-- to determine whether to initiate NIC lockdown key rotation. This flag
-- enables operators to bypass the site-wide gate and quarantine for NIC lockdown key rotation.

ALTER TABLE machines
    ADD COLUMN lockdown_ikm_credential_rotation_requested BOOLEAN NOT NULL DEFAULT false;
