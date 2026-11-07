-- Move site-explorer health reports into the generic health report sources column.
--
-- Existing reports with alerts become merge overrides from the site-explorer source.
-- Successful site-explorer runs with zero alerts are intentionally not stored as overrides.
-- The source field is normalized to make the JSON consistent with the source key.
UPDATE machines
SET health_report_overrides = jsonb_set(
    jsonb_set(
        coalesce(health_report_overrides, '{}'::jsonb),
        '{merges}',
        coalesce(health_report_overrides->'merges', '{}'::jsonb),
        true
    ),
    '{merges,site-explorer}',
    jsonb_set(
        site_explorer_health_report,
        '{source}',
        '"site-explorer"'::jsonb,
        true
    ),
    true
)
WHERE site_explorer_health_report IS NOT NULL
AND jsonb_array_length(coalesce(site_explorer_health_report->'alerts', '[]'::jsonb)) > 0;

ALTER TABLE machines
    DROP COLUMN site_explorer_health_report;
