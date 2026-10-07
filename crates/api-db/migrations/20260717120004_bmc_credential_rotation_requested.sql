-- Add bmc_credential_rotation_requested column to machines table.
-- bmc_credential_rotation_requested: an operator "force-converge this BMC now"
-- escape hatch (REQ-2). Set on the machine that owns the BMC (a host machine for
-- its host BMC, a DPU machine for its DPU BMC). When true, the machine state
-- controller enters RotatingBmc for the managed host and force-converges that
-- machine's single BMC, bypassing the passive site-wide gate and the device's
-- backoff quarantine. Mirrors machines.on_demand_machine_validation_request.

ALTER TABLE machines
    ADD COLUMN bmc_credential_rotation_requested BOOLEAN NOT NULL DEFAULT false;
