ALTER TABLE IF EXISTS machines
    ADD COLUMN network_config_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952267'),
    ADD COLUMN network_config jsonb NOT NULL DEFAULT ('{}')
;
