-- Store the result of a state handler iteration

ALTER TABLE ib_partitions ADD COLUMN controller_state_outcome JSONB;
