-- Add migration script here
ALTER TABLE machines
  ALTER COLUMN controller_state SET DEFAULT '{"state": "created"}'
;

DELETE FROM machine_state_history;
