-- Snapshot of the `ResourcePoolDef` used to seed each pool. Written once,
-- when a pool is first seeded.
CREATE TABLE resource_pool_def (
    name        TEXT PRIMARY KEY,
    definition  JSONB NOT NULL,
    seeded_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
