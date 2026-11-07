-- Add migration script here
ALTER TABLE machines
    ADD COLUMN machine_state_model_version integer NOT NULL DEFAULT 1
;
