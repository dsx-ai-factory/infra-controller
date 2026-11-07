-- Add migration script here
ALTER TABLE machines
    ADD COLUMN reprovisioning_requested JSONB
;

ALTER TABLE machine_topologies
    ADD COLUMN topology_update_needed BOOLEAN DEFAULT false
;
