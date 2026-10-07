--  : Add update time
ALTER TABLE
  machines
ADD COLUMN last_machine_validation_time TIMESTAMPTZ,
ADD COLUMN current_machine_validation_id uuid;

