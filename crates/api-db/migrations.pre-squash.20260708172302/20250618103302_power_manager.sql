-- Add migration script here

CREATE TYPE host_power_state_t AS ENUM ('on', 'off');

CREATE TABLE power_options (
    host_id VARCHAR NOT NULL,
    -- Fetch the host's power state at next_try_at time.
    -- It is 5 mins if current power state is On. 1 min if Off and desired state is On.
    last_fetched_updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_fetched_next_try_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + (RANDOM() * INTERVAL '5 minutes'),
    last_fetched_power_state host_power_state_t NOT NULL DEFAULT 'on',
    last_fetched_off_counter INTEGER NOT NULL DEFAULT 0,
    -- Tenant/SRE team can update this state. If there is some operation is being performed on any host, make the desired state
    -- off. Carbide won't try to turn on the machine and process any event in state machine.
    desired_power_state_version VARCHAR(64) NOT NULL DEFAULT ('V1-T1666644937952268'),
    desired_power_state host_power_state_t NOT NULL DEFAULT 'on',
    -- In the case when carbide is trying to turn on the host, it must wait until both DPUs come up and restart host before moving
    -- to normal state handling. Wait for 15 mins, if DPU still does not come up, just give up and let the state handler process.
    wait_until_time_before_performing_next_power_action TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Did carbide tried to turn on the machine?
    -- when did carbide tried to bring this host up using power manager?
    -- If tried_triggering_on_at is not null and last_fetched.power_state is not On and
    -- tried_triggering_on_at < last_fetched.updated_at, try powering on again.
    tried_triggering_on_at TIMESTAMPTZ,
    -- If counter is more than 2, stop trying to power on the host.
    -- Manual intervention is needed.
    tried_triggering_on_counter INTEGER NOT NULL DEFAULT 0,

    CONSTRAINT fk_host_id
        FOREIGN KEY (host_id)
        REFERENCES machines(id)
        ON DELETE CASCADE  -- force-delete handling.
        ON UPDATE CASCADE  -- Update predicted host id to permanent host id scenario.
);

-- Initialize the table with available data.
INSERT INTO power_options (host_id)
SELECT id
FROM machines
WHERE id LIKE 'fm100h%' OR id LIKE 'fm100p%';

-- New values will be added when inserting a new host in `machines` table.
