-- Keep remote requests and their allocation handles after an entity transaction
-- rolls back or the entity is deleted, so recovery can finish the same request.
CREATE TABLE resource_pool_operations (
    request_id UUID PRIMARY KEY,
    binding_id UUID NOT NULL REFERENCES resource_pool_binding(binding_id),
    pool JSONB NOT NULL,
    owner JSONB NOT NULL CONSTRAINT resource_pool_operations_owner_key UNIQUE,
    eligibility_revision TEXT NOT NULL,
    selection JSONB NOT NULL,
    completion_authority JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'attached', 'cancel_requested', 'release_requested', 'cancelled', 'released'
    )),
    result JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CHECK (state NOT IN ('attached', 'release_requested') OR
        (result IS NOT NULL AND result->>'status' = 'allocated'))
);

CREATE UNIQUE INDEX resource_pool_operations_pending_slot
    ON resource_pool_operations (
        (owner->>'owner_type'), (owner->>'owner_id'), (owner->>'slot')
    ) WHERE state IN ('pending', 'cancel_requested');

CREATE UNIQUE INDEX resource_pool_operations_handle
    ON resource_pool_operations (
        (pool->>'authority_id'), (pool->>'pool_id'), (result #>> '{result,allocation_id}')
    ) WHERE result->>'status' = 'allocated';

CREATE INDEX resource_pool_operations_state
    ON resource_pool_operations (state, request_id);

-- Active allocation records share one table; remote operations retain replay history.
CREATE TABLE resource_pool_allocations (
    allocation_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pool_name TEXT NOT NULL,
    value TEXT NOT NULL,
    value_type resource_pool_type NOT NULL,
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    auto_assign BOOLEAN,
    allocated_at TIMESTAMPTZ,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    integrated_resource_id BIGINT UNIQUE REFERENCES resource_pool(id) ON DELETE CASCADE,
    remote_request_id UUID UNIQUE REFERENCES resource_pool_operations(request_id),
    CHECK (num_nonnulls(integrated_resource_id, remote_request_id) = 1)
);

CREATE INDEX resource_pool_allocations_pool
    ON resource_pool_allocations (pool_name, allocation_id);

-- Every Integrated writer changes inventory and its allocation record in one
-- statement, including binaries that only know about `resource_pool`.
CREATE FUNCTION project_integrated_resource_pool_allocation() RETURNS TRIGGER
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.state->>'state' = 'allocated' THEN
        -- The same state and allocation time identify the same allocation.
        IF OLD.state IS NOT DISTINCT FROM NEW.state
           AND OLD.allocated IS NOT DISTINCT FROM NEW.allocated THEN
            UPDATE resource_pool_allocations SET
                pool_name = NEW.name,
                value = NEW.value,
                value_type = NEW.value_type,
                auto_assign = NEW.auto_assign
            WHERE integrated_resource_id = NEW.id;
            RETURN NEW;
        END IF;

        DELETE FROM resource_pool_allocations WHERE integrated_resource_id = NEW.id;
    END IF;

    IF NEW.state->>'state' = 'allocated' THEN
        INSERT INTO resource_pool_allocations (
            pool_name, value, value_type, owner_type, owner_id,
            auto_assign, allocated_at, integrated_resource_id
        ) VALUES (
            NEW.name, NEW.value, NEW.value_type, NEW.state->>'owner_type', NEW.state->>'owner',
            NEW.auto_assign, NEW.allocated, NEW.id
        );
    END IF;
    RETURN NEW;
END;
$$;

-- The inventory foreign key already blocks writes to `resource_pool`.
-- Install both triggers and backfill before that lock is released at commit.
CREATE TRIGGER resource_pool_allocation_insert
AFTER INSERT ON resource_pool
FOR EACH ROW WHEN (NEW.state->>'state' = 'allocated')
EXECUTE FUNCTION project_integrated_resource_pool_allocation();

CREATE TRIGGER resource_pool_allocation_update
AFTER UPDATE ON resource_pool
FOR EACH ROW WHEN (
    (OLD.state->>'state' = 'allocated' OR NEW.state->>'state' = 'allocated')
    AND (OLD.name, OLD.value, OLD.value_type, OLD.auto_assign, OLD.state, OLD.allocated)
        IS DISTINCT FROM
        (NEW.name, NEW.value, NEW.value_type, NEW.auto_assign, NEW.state, NEW.allocated)
)
EXECUTE FUNCTION project_integrated_resource_pool_allocation();

INSERT INTO resource_pool_allocations (
    pool_name, value, value_type, owner_type, owner_id,
    auto_assign, allocated_at, integrated_resource_id
)
SELECT name, value, value_type, state->>'owner_type', state->>'owner',
       auto_assign, allocated, id
FROM resource_pool
WHERE state->>'state' = 'allocated';
