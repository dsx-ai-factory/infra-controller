-- Add slot_number and tray_index columns to switches.
ALTER TABLE
    switches
ADD
    COLUMN IF NOT EXISTS slot_number INT,
ADD
    COLUMN IF NOT EXISTS tray_index INT;

ALTER TABLE
    machines
ADD
    COLUMN IF NOT EXISTS slot_number INT,
ADD
    COLUMN IF NOT EXISTS tray_index INT;