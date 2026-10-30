-- allocate() and find_owned_allocation() filter resource_pool on (name, state);
-- only name was indexed, so each call read every row of the two large pools.
CREATE INDEX resource_pool_name_state_idx
    ON resource_pool (name, state);
