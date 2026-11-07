-- Make dpf_enabled nullable to allow an unset/optional state
ALTER TABLE expected_machines ALTER COLUMN dpf_enabled DROP NOT NULL;
ALTER TABLE expected_machines ALTER COLUMN dpf_enabled DROP DEFAULT;
