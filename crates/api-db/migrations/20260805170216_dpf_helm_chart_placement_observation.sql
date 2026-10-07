-- Stores the latest extension-service status observations for this DPUDevice.
--
-- The JSON object is keyed by extension-service type (currently
-- `kubernetes_pod` and `dpf_helm_chart`). Each value is an
-- `InstanceExtensionServiceStatusObservation`. Each type has one
-- authoritative writer: forge-dpu-agent reports KubernetesPod workload
-- status, while the machine controller records the Stage-1 DPF Helm
-- placement result. When DPF provides per-DPU workload status, it replaces
-- the writer for `dpf_helm_chart`; it does not require another column or
-- observation shape.
--
-- This must remain separate from `network_status_observation`, which is an
-- agent-owned document replaced as a whole on every network-status report.
-- Type-scoped JSONB updates preserve observations written for other service
-- types and make instance-status derivation independent of how a type obtains
-- its status.
ALTER TABLE machines
    ADD COLUMN extension_service_status_observations jsonb NOT NULL DEFAULT '{}'::jsonb;

-- Backfill the latest KubernetesPod observation from the agent-owned
-- network-status document. New reports write this column directly; the
-- backfill prevents existing instances from transiently reporting Unknown
-- during rollout.
UPDATE machines
SET extension_service_status_observations = jsonb_build_object(
    'kubernetes_pod',
    network_status_observation->'extension_service_observation'
)
WHERE jsonb_typeof(network_status_observation->'extension_service_observation') = 'object'
  AND network_status_observation->'extension_service_observation'->>'observed_at' IS NOT NULL;
