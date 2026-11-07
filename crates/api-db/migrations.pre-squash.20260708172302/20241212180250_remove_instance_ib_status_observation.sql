-- Remove the ib_status_observation field
-- Infiniband statuses are stored at machines.ib_status_observation

ALTER TABLE IF EXISTS instances
    DROP COLUMN ib_status_observation;

