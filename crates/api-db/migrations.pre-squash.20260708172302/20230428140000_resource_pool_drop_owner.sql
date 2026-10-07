-- Owners are stored inside the state field, so we don't need those fields anymore
ALTER TABLE IF EXISTS resource_pool
    DROP COLUMN owner_type,
    DROP COLUMN owner_id
;
