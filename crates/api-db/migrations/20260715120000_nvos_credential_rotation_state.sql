-- Retain the opaque backend job ID needed to reconcile an accepted NVOS
-- password-rotation request. The site-wide target is created only after its
-- immutable credential has been stored and verified.

ALTER TABLE device_credential_rotation
    ADD COLUMN rotate_job_id text;
