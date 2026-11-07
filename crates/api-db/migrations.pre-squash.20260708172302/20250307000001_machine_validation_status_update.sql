-- Update state to Failed for any validation where at least one test failed
-- Only update if current state is not InProgress or Started
UPDATE machine_validation mv
SET state = 'Failed'
WHERE EXISTS (
    SELECT 1 
    FROM machine_validation_results mvr
    WHERE mvr.machine_validation_id = mv.id
    AND mvr.exit_code != 0
)
AND mv.state NOT IN ('InProgress', 'Started');

-- Update existing completed counts to only count tests with exit_code = 0
UPDATE machine_validation mv
SET completed = COALESCE(subquery.successful_count, 0)
FROM (
    SELECT 
        machine_validation_id, 
        COUNT(*) as successful_count
    FROM 
        machine_validation_results
    WHERE 
        exit_code = 0
    GROUP BY 
        machine_validation_id
) AS subquery
WHERE mv.id = subquery.machine_validation_id AND mv.state NOT IN ('InProgress', 'Started');

-- Drop the existing trigger and function
DROP TRIGGER IF EXISTS trigger_update_machine_validation_completed ON machine_validation_results;
DROP TRIGGER IF EXISTS update_machine_validation_completed ON machine_validation_results;
DROP FUNCTION IF EXISTS update_machine_validation_results_completed();

-- Create the updated function that only handles INSERT operations
CREATE OR REPLACE FUNCTION update_machine_validation_results_completed()
RETURNS TRIGGER AS $$
BEGIN
    -- Only increment completed count when exit_code is 0 (success) and only for INSERT
    IF TG_OP = 'INSERT' AND NEW.exit_code = 0 THEN
        UPDATE machine_validation
        SET completed = completed + 1
        WHERE id = NEW.machine_validation_id;
    ELSIF TG_OP = 'DELETE' AND NEW.exit_code = 0 THEN
        UPDATE machine_validation
        SET completed = completed - 1
        WHERE id = OLD.machine_validation_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- Recreate the trigger
CREATE TRIGGER update_machine_validation_results_completed
AFTER INSERT ON machine_validation_results
FOR EACH ROW
EXECUTE FUNCTION update_machine_validation_results_completed();
