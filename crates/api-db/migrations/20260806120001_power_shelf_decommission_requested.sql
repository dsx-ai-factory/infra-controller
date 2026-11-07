-- Request that a Ready managed power shelf enter its decommissioning workflow.
ALTER TABLE power_shelves
    ADD COLUMN decommission_requested BOOLEAN NOT NULL DEFAULT FALSE;
