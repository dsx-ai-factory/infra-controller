-- Add bmc_credential_rotation_requested column to switches table.
-- bmc_credential_rotation_requested: an operator "force-converge this BMC now"
-- escape hatch (REQ-2), the switch analogue of
-- machines.bmc_credential_rotation_requested. When true, the switch state
-- controller enters RotatingBmc and force-converges the switch BMC, bypassing
-- the passive site-wide gate and the device's backoff quarantine. A switch has
-- exactly one BMC, so the flag's presence on the row names the target device.

ALTER TABLE switches
    ADD COLUMN bmc_credential_rotation_requested BOOLEAN NOT NULL DEFAULT false;
