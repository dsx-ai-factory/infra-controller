-- Store the result of a state handler iteration

ALTER TABLE network_segments ADD COLUMN controller_state_outcome JSONB;
