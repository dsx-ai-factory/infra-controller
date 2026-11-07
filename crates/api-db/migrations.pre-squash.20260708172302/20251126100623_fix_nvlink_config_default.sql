-- Change the default value of nvlink_config to "gpu_configs" to match the Rust struct field name
UPDATE instances
SET nvlink_config = '{"gpu_configs": []}'
WHERE nvlink_config = '{"nvlink_gpus": []}';