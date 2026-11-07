-- Cleans various state handler outcome fields in order to prepare them for
-- incompatible results.
-- New results will be written within the next state handler iteration

UPDATE machines SET controller_state_outcome=NULL;
UPDATE network_segments SET controller_state_outcome=NULL;
UPDATE ib_partitions SET controller_state_outcome=NULL;
