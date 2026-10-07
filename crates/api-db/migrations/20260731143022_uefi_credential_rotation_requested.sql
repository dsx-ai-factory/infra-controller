-- Add uefi_credential_rotation_requested column to machines table.
-- uefi_credential_rotation_requested: an operator "force-converge this UEFI
-- password now" escape hatch. Set on the machine that owns the
-- UEFI credential (a host machine for its host UEFI; a DPU machine for its DPU
-- UEFI). When true, the machine state controller enters
-- RotatingHostUefi (or RotatingDpuUefi for a DPU) and force-converges
-- that machine's UEFI credential, bypassing the passive site-wide gate and the
-- device's backoff
-- quarantine. Mirrors machines.bmc_credential_rotation_requested.

ALTER TABLE machines
    ADD COLUMN uefi_credential_rotation_requested BOOLEAN NOT NULL DEFAULT false;
