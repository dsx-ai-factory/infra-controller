-- Benchpress is no longer a supported CLI, so remove all machine validation
-- tests that invoke it. These tests used /opt/benchpress/benchpress as the
-- binary
DELETE FROM machine_validation_tests
WHERE command = '/opt/benchpress/benchpress';
