-- Add migration script here
UPDATE machine_validation SET context=(SELECT context DISTINCT FROM machine_validation_results r WHERE r.machine_validation_id=machine_validation.id LIMIT 1);