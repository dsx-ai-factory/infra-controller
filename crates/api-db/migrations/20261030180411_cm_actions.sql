-- Durable log context for CM work, with ownership independent of inventory data.
CREATE TABLE cm_actions (
    id uuid PRIMARY KEY,
    machine_id varchar(64) REFERENCES machines(id) ON DELETE CASCADE ON UPDATE CASCADE,
    switch_id varchar(64) REFERENCES switches(id) ON DELETE CASCADE ON UPDATE CASCADE,
    power_shelf_id varchar(64) REFERENCES power_shelves(id) ON DELETE CASCADE ON UPDATE CASCADE,
    rack_id varchar(64) REFERENCES racks(id) ON DELETE CASCADE ON UPDATE CASCADE,
    bmc_mac macaddr,
    request_key text NOT NULL,
    metadata jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT statement_timestamp(),
    completed_at timestamptz,
    CHECK (num_nonnulls(machine_id, switch_id, power_shelf_id, rack_id, bmc_mac) = 1)
);

CREATE UNIQUE INDEX cm_actions_machine_request ON cm_actions (machine_id, request_key) WHERE machine_id IS NOT NULL;
CREATE UNIQUE INDEX cm_actions_switch_request ON cm_actions (switch_id, request_key) WHERE switch_id IS NOT NULL;
CREATE UNIQUE INDEX cm_actions_power_shelf_request ON cm_actions (power_shelf_id, request_key) WHERE power_shelf_id IS NOT NULL;
CREATE UNIQUE INDEX cm_actions_rack_request ON cm_actions (rack_id, request_key) WHERE rack_id IS NOT NULL;
CREATE UNIQUE INDEX cm_actions_firmware_request ON cm_actions (bmc_mac, request_key) WHERE bmc_mac IS NOT NULL;

ALTER TABLE direct_dispatch_firmware_update_jobs ADD COLUMN action_id uuid REFERENCES cm_actions(id);
