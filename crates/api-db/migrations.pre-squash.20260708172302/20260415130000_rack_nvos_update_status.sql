ALTER TABLE racks
    ADD COLUMN nvos_update_job JSONB;

ALTER TABLE switches
    ADD COLUMN nvos_update_status JSONB;
