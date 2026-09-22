-- Backfill the unified table from the two legacy columns this release stops
-- writing, so a direct-dispatch firmware job already in flight at upgrade stays
-- pollable under its BMC MAC. The columns themselves are dropped in a later
-- release, once no running instance still writes them.
--
-- Pre-ingestion jobs live on explored_endpoints keyed by BMC IP; resolve the
-- MAC through the BMC interface that owns that address. Insert these first so a
-- later machines row can overwrite them.
INSERT INTO direct_dispatch_firmware_update_jobs (bmc_mac, job_id)
SELECT DISTINCT ON (mi.mac_address) mi.mac_address, ee.backend_firmware_object_job_id
FROM explored_endpoints ee
JOIN machine_interface_addresses mia ON mia.address = ee.address
JOIN machine_interfaces mi
    ON mi.id = mia.interface_id AND mi.interface_type = 'Bmc'
WHERE ee.backend_firmware_object_job_id IS NOT NULL
ORDER BY mi.mac_address, ee.address
ON CONFLICT (bmc_mac) DO NOTHING;

-- Ingested jobs live on machines keyed by machine id; resolve the MAC through
-- the machine's BMC interface. A tray flashed pre-ingestion and then ingested
-- can hold a stale explored_endpoints entry alongside a current machines entry
-- for the same MAC, so a machines job always wins over an explored one.
INSERT INTO direct_dispatch_firmware_update_jobs (bmc_mac, job_id)
SELECT DISTINCT ON (mi.mac_address) mi.mac_address, m.backend_firmware_object_job_id
FROM machines m
JOIN machine_interfaces mi
    ON mi.machine_id = m.id AND mi.interface_type = 'Bmc'
WHERE m.backend_firmware_object_job_id IS NOT NULL
ORDER BY mi.mac_address, m.updated DESC
ON CONFLICT (bmc_mac) DO UPDATE SET job_id = EXCLUDED.job_id;
