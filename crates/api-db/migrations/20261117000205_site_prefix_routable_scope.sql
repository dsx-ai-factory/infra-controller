-- Add the `routable` site-prefix routing scope: a prefix the DPU may advertise
-- beyond the datacenter fabric (a delegated or bring-your-own routable block),
-- alongside the existing `datacenter_only`. Isolated in its own migration
-- because Postgres forbids using a newly added enum value in the same
-- transaction that adds it.
ALTER TYPE site_prefix_routing_scope ADD VALUE 'routable';
