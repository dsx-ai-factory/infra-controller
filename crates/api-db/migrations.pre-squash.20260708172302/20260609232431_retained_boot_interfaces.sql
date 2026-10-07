-- Preserve the boot interface pair (MAC + vendor-named Redfish
-- EthernetInterface.Id) across interface deletion, in two pieces:
--
-- 1. `retained_boot_interfaces` holds the last known pair for interfaces
--    whose machine_interfaces rows are being deleted (e.g. admin
--    force-delete with --delete-interfaces), so a re-ingested machine can
--    recover its boot target before its first DHCP. Keyed by MAC with no
--    foreign keys on purpose: the machine and its interfaces are gone by
--    the time these rows matter. Rows are consumed (deleted) when the boot
--    interface id lands on a machine_interfaces row again.
--
-- 2. Predicted machine interfaces hold the boot interface id alongside the
--    MAC, so a zero-DPU/NIC-mode host minted before its first DHCP can hand
--    the full pair to the machine_interfaces row at promotion time.
CREATE TABLE retained_boot_interfaces (
    mac_address       macaddr     PRIMARY KEY,
    boot_interface_id text        NOT NULL,
    recorded_at       timestamptz NOT NULL DEFAULT NOW()
);

ALTER TABLE predicted_machine_interfaces ADD COLUMN boot_interface_id text;
