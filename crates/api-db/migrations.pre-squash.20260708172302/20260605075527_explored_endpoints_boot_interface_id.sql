-- Add boot_interface_id alongside boot_interface_mac on explored_endpoints.
-- Together they form a fully-populated MachineBootInterface (the boot NIC's MAC
-- plus its vendor-native Redfish EthernetInterface.Id). Storing both identifiers
-- lets Redfish setup flows target the MAC first and fall back to the [stable]
-- interface id when the MAC isn't resolvable (e.g. after a DPU DpuMode -> NicMode
-- flip drops it from Redfish). Nullable; populated naturally on the next healthy
-- exploration.
ALTER TABLE explored_endpoints ADD COLUMN boot_interface_id text;
