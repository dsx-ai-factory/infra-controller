-- Incorrect foreign key constraints on machine_interfaces table

ALTER TABLE machine_interfaces 
DROP COLUMN IF EXISTS switch_id;

-- Re-add it with the correct reference to switches table
ALTER TABLE machine_interfaces
ADD COLUMN switch_id VARCHAR(64) REFERENCES switches(id) ON DELETE SET NULL;

-- Recreate the index
CREATE INDEX IF NOT EXISTS idx_machine_interfaces_switch_id 
ON machine_interfaces(switch_id);

-- Remove the power_shelf_id column
ALTER TABLE machine_interfaces
DROP COLUMN IF EXISTS power_shelf_id;

-- Re-add it with the correct reference to power_shelves table
ALTER TABLE machine_interfaces
ADD COLUMN power_shelf_id VARCHAR(64) REFERENCES power_shelves(id) ON DELETE SET NULL;

-- Recreate the index
CREATE INDEX IF NOT EXISTS idx_machine_interfaces_power_shelf_id 
ON machine_interfaces(power_shelf_id);

-- Add bmc_type column to machine_interfaces table
CREATE TYPE association_type AS ENUM ('None', 'Machine', 'PowerShelf', 'Switch');

ALTER TABLE machine_interfaces
ADD COLUMN association_type association_type NOT NULL DEFAULT 'None';

-- Add data step to set association_type on existing machines
UPDATE machine_interfaces SET association_type = 'Machine' WHERE machine_id IS NOT NULL;
UPDATE machine_interfaces SET association_type = 'PowerShelf' WHERE power_shelf_id IS NOT NULL;
UPDATE machine_interfaces SET association_type = 'Switch' WHERE switch_id IS NOT NULL;

-- Make sure only one of the columns is populated at a time
ALTER TABLE machine_interfaces
ADD CONSTRAINT chk_max_one_association
CHECK (
  ( association_type = 'None'
    AND machine_id IS NULL AND power_shelf_id IS NULL AND switch_id IS NULL
  )
  OR
  ( association_type <> 'None'
    AND num_nonnulls(machine_id, power_shelf_id, switch_id) <= 1
  )
);
