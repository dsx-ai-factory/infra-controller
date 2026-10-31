-- Keep the originating log trace available to tasks dispatched after a restart.
ALTER TABLE operation_run ADD COLUMN metadata jsonb;
