-- Add migration script here
ALTER TABLE machines
    ADD COLUMN controller_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    ADD COLUMN controller_state jsonb NOT NULL DEFAULT ('{"state": "init"}')
;

ALTER TABLE instances
    ADD COLUMN deleted TIMESTAMPTZ
;
