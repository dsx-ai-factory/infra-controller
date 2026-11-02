-- Final-source removals previously left source-less NVLink domain health rows
-- behind. Remove only the two empty container shapes written by that code.

DELETE FROM nvlink_domain_health_reports
WHERE health_reports IN (
    '{"merges": {}}'::jsonb,
    '{"merges": {}, "replace": null}'::jsonb
);
