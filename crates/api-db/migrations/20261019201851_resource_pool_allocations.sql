-- Keep one allocation record for either backend. Remote requests are saved before
-- provider calls and retained after cleanup so retries can recognize completed work.
CREATE TABLE resource_pool_allocations (
    allocation_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pool_name TEXT NOT NULL,
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'attached', 'cancel_requested', 'release_requested', 'cancelled', 'released'
    )),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    -- Match startup and pool growth: lock bindings before inventory.
    binding_id UUID REFERENCES resource_pool_binding(binding_id),
    integrated_resource_id BIGINT UNIQUE REFERENCES resource_pool(id) ON DELETE CASCADE,
    value TEXT,
    value_type resource_pool_type,
    auto_assign BOOLEAN,
    allocated_at TIMESTAMPTZ,
    remote_request JSONB,
    result JSONB,
    CHECK (
        (integrated_resource_id IS NOT NULL AND value IS NOT NULL AND value_type IS NOT NULL
            AND state = 'attached' AND binding_id IS NULL AND remote_request IS NULL AND result IS NULL)
        OR
        (integrated_resource_id IS NULL AND value IS NULL AND value_type IS NULL
            AND auto_assign IS NULL AND allocated_at IS NULL
            AND binding_id IS NOT NULL AND remote_request IS NOT NULL)
    ),
    CHECK (binding_id IS NULL OR state NOT IN ('attached', 'release_requested')
        OR (result->>'status') IS NOT DISTINCT FROM 'allocated')
);

CREATE UNIQUE INDEX resource_pool_allocations_owner_key
    ON resource_pool_allocations (
        owner_type, owner_id, (remote_request->>'slot'), (remote_request->'generation')
    ) WHERE binding_id IS NOT NULL;

CREATE UNIQUE INDEX resource_pool_allocations_pending_slot
    ON resource_pool_allocations (owner_type, owner_id, (remote_request->>'slot'))
    WHERE binding_id IS NOT NULL AND state IN ('pending', 'cancel_requested');

CREATE UNIQUE INDEX resource_pool_allocations_handle
    ON resource_pool_allocations (
        (remote_request->>'authority_id'), (remote_request->>'pool_id'),
        (result #>> '{result,allocation_id}')
    ) WHERE result->>'status' = 'allocated';

CREATE INDEX resource_pool_allocations_state
    ON resource_pool_allocations (state, allocation_id) WHERE binding_id IS NOT NULL;

CREATE INDEX resource_pool_allocations_pool
    ON resource_pool_allocations (pool_name, allocation_id)
    WHERE state IN ('pending', 'attached', 'cancel_requested', 'release_requested')
      AND (integrated_resource_id IS NOT NULL OR result->>'status' = 'allocated');

-- Existing Integrated inventory already tells us which values have owners.
INSERT INTO resource_pool_allocations (
    pool_name, owner_type, owner_id, state, integrated_resource_id,
    value, value_type, auto_assign, allocated_at
)
SELECT name, state->>'owner_type', state->>'owner', 'attached', id,
       value, value_type, auto_assign, allocated
FROM resource_pool
WHERE state->>'state' = 'allocated';
