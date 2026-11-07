-- Per-host DPU operating mode (replaces the site-wide `force_dpu_nic_mode`
-- config flag, which remains as a fallback for existing deployments). See
-- the `ExpectedMachine.dpu_mode` field and `DpuMode` enum in `forge.proto`.
CREATE TYPE dpu_mode_t AS ENUM ('dpu_mode', 'nic_mode', 'no_dpu');

ALTER TABLE expected_machines
  ADD COLUMN dpu_mode dpu_mode_t NOT NULL DEFAULT 'dpu_mode';
