-- API-owned request marker consumed by the machine state controller when a Ready managed host
-- begins decommissioning.
ALTER TABLE machines
    ADD COLUMN decommission_requested BOOLEAN NOT NULL DEFAULT FALSE;
