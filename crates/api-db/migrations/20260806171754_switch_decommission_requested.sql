ALTER TABLE switches
    ADD COLUMN decommission_requested BOOLEAN NOT NULL DEFAULT FALSE;
