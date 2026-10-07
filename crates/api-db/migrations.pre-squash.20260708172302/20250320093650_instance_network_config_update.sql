-- Add updated_network_config column to instances table
ALTER TABLE instances
    ADD COLUMN update_network_config_request JSONB;
