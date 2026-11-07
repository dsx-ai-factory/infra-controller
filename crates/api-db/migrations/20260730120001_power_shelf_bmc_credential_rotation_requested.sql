-- Add bmc_credential_rotation_requested column to power_shelves table.
-- bmc_credential_rotation_requested: an operator "force-converge this PMC now"
-- escape hatch (REQ-2), the power-shelf analogue of
-- machines.bmc_credential_rotation_requested and
-- switches.bmc_credential_rotation_requested. When true, the power-shelf state
-- controller enters RotatingBmc and force-converges the power shelf BMC (PMC),
-- bypassing the passive site-wide gate and the device's backoff quarantine. A
-- power shelf has exactly one BMC, so the flag's presence on the row names the
-- target device.

ALTER TABLE power_shelves
    ADD COLUMN bmc_credential_rotation_requested BOOLEAN NOT NULL DEFAULT false;
