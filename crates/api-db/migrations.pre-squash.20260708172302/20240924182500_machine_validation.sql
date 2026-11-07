-- Add migration script here
ALTER TABLE
  machines
ADD COLUMN on_demand_machine_validation_id uuid,
ADD COLUMN on_demand_machine_validation_request BOOLEAN;