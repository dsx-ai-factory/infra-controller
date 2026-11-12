-- Track networking-primary decisions independently of desired boot decisions.
ALTER TABLE machine_interfaces
    ADD COLUMN primary_selection_source boot_interface_selection_source,
    ADD COLUMN primary_selection_updated_at timestamp with time zone,
    ADD CONSTRAINT machine_interfaces_primary_selection_time_consistent CHECK (
        (primary_selection_source IS NULL AND primary_selection_updated_at IS NULL)
        OR (primary_selection_source IS NOT NULL
            AND (primary_selection_source = 'legacy_unknown'
                 OR primary_selection_updated_at IS NOT NULL))
    );

-- Preserve ambiguous historical Operator choices on the current owned primary.
-- Automatic sources transfer only when its MAC matches the desired boot target.
UPDATE machine_interfaces AS interface
SET primary_selection_source = boot.selection_source,
    primary_selection_updated_at = boot.selection_updated_at
FROM machine_boot_interfaces AS boot
WHERE interface.machine_id = boot.machine_id
  AND interface.primary_interface
  AND interface.interface_type != 'Bmc'
  AND (boot.selection_source = 'operator'
       OR interface.mac_address = boot.desired_mac_address);

-- Older writers may leave attribution stale until an upgraded primary-selection path runs.
-- Permit their demotions without requiring metadata to be absent on nonprimary rows.
