-- Add reset_requested column to machines table.
-- reset_requested: when set by an operator, the state controller tears down the host's
-- instance and DPF resources, then re-ingests the host from DPU discovery.

ALTER TABLE machines
    ADD COLUMN reset_requested JSONB;
