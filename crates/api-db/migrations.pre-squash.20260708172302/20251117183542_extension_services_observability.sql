-- Add a new column for storing observability config with extension services
ALTER TABLE extension_service_versions ADD COLUMN observability jsonb DEFAULT '{"configs": []}'::jsonb
