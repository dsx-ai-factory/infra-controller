-- Store the result of a state handler iteration

ALTER TABLE machines ADD COLUMN controller_state_outcome JSONB;
