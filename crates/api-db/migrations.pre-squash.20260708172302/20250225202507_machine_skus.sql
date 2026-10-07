CREATE TABLE machine_skus (
    id                   VARCHAR(256) PRIMARY KEY NOT NULL,
    description          VARCHAR,
    components           jsonb NOT NULL,
    created              timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL
);

ALTER TABLE machines ADD COLUMN hw_sku VARCHAR(256), ADD COLUMN hw_sku_status jsonb;
ALTER TABLE machines ADD FOREIGN KEY (hw_sku) REFERENCES machine_skus(id);

ALTER TABLE machines ADD COLUMN IF NOT EXISTS sku_validation_health_report jsonb
    NOT NULL DEFAULT '{"source":"sku-validation","observed_at":"2025-01-01T12:00:00Z","successes":[],"alerts":[]}';
