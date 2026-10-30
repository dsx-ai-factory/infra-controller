-- Keep existing services deploying through instance attachment and allow baseline
-- Helm services to deploy to all DPUs at creation time without dpu_target.
ALTER TABLE extension_services
    ADD COLUMN deployment_type text NOT NULL DEFAULT 'demand_driven';

ALTER TABLE extension_services
    DROP CONSTRAINT extension_services_dpu_target_check;

ALTER TABLE extension_services
    ADD CONSTRAINT extension_services_dpu_target_check CHECK (
        (type = 'kubernetes_pod'
            AND deployment_type = 'demand_driven'
            AND dpu_target IS NULL)
        OR
        (type = 'dpf_helm_chart'
            AND deployment_type = 'demand_driven'
            AND dpu_target IS NOT NULL
            AND dpu_target IN ('primary', 'all_active', 'all'))
        OR
        (type = 'dpf_helm_chart'
            AND deployment_type = 'baseline'
            AND dpu_target IS NULL)
    );
