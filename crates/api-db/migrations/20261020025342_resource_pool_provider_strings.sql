-- Provider identifiers, owner IDs and slots can exceed the B-tree key limit,
-- so index their hashes while keeping the original text in each allocation.
DROP INDEX resource_pool_allocations_handle,
    resource_pool_allocations_owner_key,
    resource_pool_allocations_pending_slot;

CREATE UNIQUE INDEX resource_pool_allocations_handle
    ON resource_pool_allocations (
        public.digest(remote_request->>'authority_id', 'sha256'),
        public.digest(remote_request->>'pool_id', 'sha256'),
        public.digest(result #>> '{result,allocation_id}', 'sha256')
    ) WHERE result->>'status' = 'allocated';

CREATE UNIQUE INDEX resource_pool_allocations_owner_key
    ON resource_pool_allocations (
        owner_type, public.digest(owner_id, 'sha256'),
        public.digest(remote_request->>'slot', 'sha256'), (remote_request->'generation')
    ) WHERE binding_id IS NOT NULL;

CREATE UNIQUE INDEX resource_pool_allocations_pending_slot
    ON resource_pool_allocations (
        owner_type, public.digest(owner_id, 'sha256'),
        public.digest(remote_request->>'slot', 'sha256')
    ) WHERE binding_id IS NOT NULL AND state IN ('pending', 'cancel_requested');
