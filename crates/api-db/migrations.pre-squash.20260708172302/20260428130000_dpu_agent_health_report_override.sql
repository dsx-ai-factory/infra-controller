-- Move DPU agent health reports into the generic health report sources column.
--
-- Existing reports become merge overrides from the forge-dpu-agent source.
-- Successful DPU agent reports with zero alerts are intentionally preserved because
-- their presence is used as heartbeat data.
-- The source field is normalized to make the JSON consistent with the source key.
UPDATE machines
SET health_report_overrides = jsonb_set(
    jsonb_set(
        coalesce(health_report_overrides, '{}'::jsonb),
        '{merges}',
        coalesce(health_report_overrides->'merges', '{}'::jsonb),
        true
    ),
    '{merges,forge-dpu-agent}',
    jsonb_set(
        dpu_agent_health_report,
        '{source}',
        '"forge-dpu-agent"'::jsonb,
        true
    ),
    true
)
WHERE dpu_agent_health_report IS NOT NULL;

ALTER TABLE machines
    DROP COLUMN dpu_agent_health_report;
