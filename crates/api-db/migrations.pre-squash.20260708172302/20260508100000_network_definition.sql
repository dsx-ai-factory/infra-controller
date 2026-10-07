-- Snapshot of the `NetworkDefinition` used to seed each network segment.
-- Written once, when a network is first seeded. The segment_id link with
-- ON DELETE CASCADE so the snapshot is dropped automatically when the segment
-- is deleted.
CREATE TABLE network_def (
    name        TEXT PRIMARY KEY,
    segment_id  UUID NOT NULL UNIQUE REFERENCES network_segments(id) ON DELETE CASCADE,
    definition  JSONB NOT NULL,
    seeded_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
