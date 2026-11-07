-- Move validation health reports into the generic health report sources column.
--
-- Existing reports with alerts become merge overrides from their validation sources.
-- Successful validation runs with zero alerts are intentionally not stored as overrides.
-- The source field is normalized to make the JSON consistent with the source key.
UPDATE machines
SET health_report_overrides = jsonb_set(
    jsonb_set(
        coalesce(health_report_overrides, '{}'::jsonb),
        '{merges}',
        coalesce(health_report_overrides->'merges', '{}'::jsonb),
        true
    ),
    '{merges,machine-validation}',
    jsonb_set(
        machine_validation_health_report,
        '{source}',
        '"machine-validation"'::jsonb,
        true
    ),
    true
)
WHERE machine_validation_health_report IS NOT NULL
AND jsonb_array_length(coalesce(machine_validation_health_report->'alerts', '[]'::jsonb)) > 0;

UPDATE machines
SET health_report_overrides = jsonb_set(
    jsonb_set(
        coalesce(health_report_overrides, '{}'::jsonb),
        '{merges}',
        coalesce(health_report_overrides->'merges', '{}'::jsonb),
        true
    ),
    '{merges,sku-validation}',
    jsonb_set(
        sku_validation_health_report,
        '{source}',
        '"sku-validation"'::jsonb,
        true
    ),
    true
)
WHERE sku_validation_health_report IS NOT NULL
AND jsonb_array_length(coalesce(sku_validation_health_report->'alerts', '[]'::jsonb)) > 0;

ALTER TABLE machines
    DROP COLUMN machine_validation_health_report,
    DROP COLUMN sku_validation_health_report;
