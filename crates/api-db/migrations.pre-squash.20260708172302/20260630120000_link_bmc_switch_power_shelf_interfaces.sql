-- Persist the Switch/PowerShelf <-> BMC interface link on machine_interfaces,
-- extending the Machine <-> BMC work from #1610 to switches (#1722) and power
-- shelves (#1723). New hardware gets this link during ingestion; this migration
-- backfills the link for hardware already in the database.

-- Power shelves: the PMC interface is already associated with the shelf
-- (create_power_shelf set power_shelf_id), so this is a pure annotation -- mark
-- it `Bmc` and demote it from primary, matching how machine BMCs are stored.
UPDATE machine_interfaces
SET interface_type = 'Bmc',
    primary_interface = FALSE
WHERE power_shelf_id IS NOT NULL
  AND interface_type = 'Data';

-- Switches: the BMC interface was never linked to the switch (only NVOS data
-- interfaces were), so match on the switch's stored BMC MAC. Matching on
-- bmc_mac_address -- rather than switch_id -- is what keeps us from relabeling
-- the already-linked NVOS data interfaces as `Bmc`.
UPDATE machine_interfaces mi
SET switch_id = s.id,
    association_type = 'Switch',
    interface_type = 'Bmc',
    primary_interface = FALSE
FROM switches s
WHERE mi.mac_address = s.bmc_mac_address
  AND mi.power_shelf_id IS NULL
  AND (mi.switch_id IS NULL OR mi.switch_id = s.id);

-- Indexes backing find_bmc_info_by_{switch,power_shelf}_ids, mirroring the
-- machine_interfaces_bmc_machine_id_idx added in #1610.
CREATE INDEX IF NOT EXISTS machine_interfaces_bmc_switch_id_idx
ON machine_interfaces(switch_id)
WHERE interface_type = 'Bmc';

CREATE INDEX IF NOT EXISTS machine_interfaces_bmc_power_shelf_id_idx
ON machine_interfaces(power_shelf_id)
WHERE interface_type = 'Bmc';
