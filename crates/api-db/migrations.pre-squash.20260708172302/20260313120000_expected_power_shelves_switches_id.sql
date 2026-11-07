ALTER TABLE expected_power_shelves
    ADD COLUMN expected_power_shelf_id uuid NOT NULL UNIQUE DEFAULT gen_random_uuid();

ALTER TABLE expected_switches
    ADD COLUMN expected_switch_id uuid NOT NULL UNIQUE DEFAULT gen_random_uuid();
