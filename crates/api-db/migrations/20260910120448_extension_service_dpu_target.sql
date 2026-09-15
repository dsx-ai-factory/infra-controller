-- Historical Helm registrations explicitly adopt all active/previously used DPUs.
-- Quiesce Helm registration and instance configuration writers before applying this migration.
ALTER TABLE extension_services ADD COLUMN dpu_target text;
UPDATE extension_services SET dpu_target = 'all_active' WHERE type = 'dpf_helm_chart';
ALTER TABLE extension_services ADD CONSTRAINT extension_services_dpu_target_check CHECK (
    (type = 'dpf_helm_chart' AND dpu_target IS NOT NULL AND dpu_target IN ('primary', 'all_active', 'all'))
    OR (type = 'kubernetes_pod' AND dpu_target IS NULL)
);

-- Preserve attachment versions and removal markers while linking historical policy.
UPDATE instances i
SET extension_services_config = jsonb_set(i.extension_services_config, '{service_configs}', (
    SELECT jsonb_agg(CASE WHEN es.type = 'dpf_helm_chart'
        THEN cfg || jsonb_build_object('dpu_target', es.dpu_target)
        ELSE cfg END ORDER BY ordinal)
    FROM jsonb_array_elements(i.extension_services_config->'service_configs')
        WITH ORDINALITY AS entries(cfg, ordinal)
    LEFT JOIN extension_services es ON es.id::text = cfg->>'service_id'
))
WHERE jsonb_array_length(i.extension_services_config->'service_configs') > 0;
