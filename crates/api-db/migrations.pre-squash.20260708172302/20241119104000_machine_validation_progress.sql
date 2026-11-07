ALTER TABLE
    IF EXISTS machine_validation_results
ADD
    COLUMN test_id VARCHAR(64);

UPDATE
    machine_validation_results
SET
    test_id = 'forge_' || name;

ALTER TABLE
    machine_validation
ADD COLUMN duration_to_complete BIGINT DEFAULT 0,
ALTER COLUMN completed SET DEFAULT 0,
ALTER COLUMN total SET DEFAULT 0;

-- Update the existing rows with zero 
UPDATE
    machine_validation
SET
    duration_to_complete = 0;

-- Update completed count using trigger 

CREATE OR REPLACE FUNCTION update_machine_validation_results_completed()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        UPDATE machine_validation
        SET completed = completed + 1
        WHERE id = NEW.machine_validation_id;
    ELSIF TG_OP = 'DELETE' THEN
        UPDATE machine_validation
        SET completed = completed - 1
        WHERE id = OLD.machine_validation_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trigger_update_machine_validation_completed
AFTER INSERT OR DELETE ON machine_validation_results
FOR EACH ROW
EXECUTE FUNCTION update_machine_validation_results_completed();
