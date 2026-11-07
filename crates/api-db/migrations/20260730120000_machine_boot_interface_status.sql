-- Track the desired boot-interface generation that machine-controller
-- currently treats as converged. The status is intentionally retained
-- when the desired target changes: comparing its version with desired_version
-- makes the new work pending without discarding the last convergence status.
ALTER TABLE machine_boot_interfaces
    ADD COLUMN verified_version varchar(64),
    ADD COLUMN observed_at timestamp with time zone,
    ADD COLUMN assumed boolean NOT NULL DEFAULT false,
    -- Existing rows satisfy this constraint through the unset-status branch.
    -- Add it normally so this single migration remains atomic without implying
    -- that validation releases the transaction's table lock.
    ADD CONSTRAINT machine_boot_interfaces_status_consistent
        CHECK (
            (
                verified_version IS NULL
                AND observed_at IS NULL
                AND NOT assumed
            )
            OR (
                verified_version IS NOT NULL
                AND observed_at IS NOT NULL
            )
        );

-- Avoid scheduling a fleet-wide boot reconfiguration when this status first
-- rolls out. These rows predate status tracking, so record the baseline
-- explicitly as assumed rather than presenting it as a Redfish observation.
UPDATE machine_boot_interfaces AS boot_interface
SET verified_version = desired_version,
    observed_at = CURRENT_TIMESTAMP,
    assumed = true
FROM machines AS machine
WHERE machine.id = boot_interface.machine_id
  AND machine.controller_state->>'state' IN ('ready', 'assigned');
