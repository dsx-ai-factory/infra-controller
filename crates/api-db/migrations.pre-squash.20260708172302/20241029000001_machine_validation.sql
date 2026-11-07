CREATE TABLE machine_validation_tests (
    test_id VARCHAR(64) NOT NULL,
    name VARCHAR(64) NOT NULL,
    description text NOT NULL,
    img_name VARCHAR(255),
    container_arg text,
    execute_in_host bool DEFAULT false,
    external_config_file VARCHAR(255),
    command VARCHAR(255) NOT NULL,
    args text NOT NULL,
    extra_output_file VARCHAR(255),
    extra_err_file VARCHAR(255),
    pre_condition text,
    contexts text [] NOT NULL,
    timeout BIGINT NOT NULL DEFAULT 7200,
    version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    supported_platforms text [] NOT NULL,
    modified_by VARCHAR(64) NOT NULL DEFAULT ('System'),
    verified bool NOT NULL DEFAULT false,
    read_only bool NOT NULL DEFAULT false,
    custom_tags text [],
    components text [] NOT NULL DEFAULT ARRAY['Compute'],
    last_modified_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    is_enabled bool NOT NULL DEFAULT true,

    PRIMARY KEY (test_id, version)
);

CREATE OR REPLACE FUNCTION update_timestamp()
RETURNS TRIGGER AS $$
BEGIN
    -- Set the `last_modified_at` column to the current timestamp
    NEW.last_modified_at := CURRENT_TIMESTAMP;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_timestamp
BEFORE UPDATE ON machine_validation_tests
FOR EACH ROW
EXECUTE FUNCTION update_timestamp();
