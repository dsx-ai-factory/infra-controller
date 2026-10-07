CREATE TABLE nvlink_domain_health_reports (
    id uuid PRIMARY KEY,
    health_reports jsonb NOT NULL DEFAULT '{"merges": {}}'::jsonb
);
