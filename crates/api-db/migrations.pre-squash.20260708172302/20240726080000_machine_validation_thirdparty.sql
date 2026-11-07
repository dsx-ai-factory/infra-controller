-- Add migration script here
CREATE TABLE machine_validation_external_config(
    name VARCHAR(64) NOT NULL,
    description VARCHAR(64),
    config jsonb NOT NULL,
    PRIMARY KEY (name)
);
