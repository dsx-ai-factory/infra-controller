-- Keep DPF identity separate from arbitrary Helm values in the text document.
ALTER TABLE extension_service_versions ADD COLUMN dpf_service_id text;

CREATE INDEX extension_service_versions_dpf_service_id_lower_idx
    ON extension_service_versions (lower(dpf_service_id)) WHERE dpf_service_id IS NOT NULL;
