--- 20240607063400_measured_boot_machines.sql
---
--- This migration is part of a wider MR to point
--- the measured boot code away from its PoC specific
--- "mock" machines, and over to actual machines,
--- including getting rid of the mock-specific tables
--- in the process (since they're no longer needed).

ALTER TABLE measurement_reports DROP CONSTRAINT measurement_reports_machine_id_fkey;
ALTER TABLE measurement_reports ADD CONSTRAINT measurement_reports_machine_id_fkey FOREIGN KEY (machine_id) REFERENCES machine_topologies(machine_id);

DROP TABLE mock_machines_attrs;
DROP TABLE mock_machines;
