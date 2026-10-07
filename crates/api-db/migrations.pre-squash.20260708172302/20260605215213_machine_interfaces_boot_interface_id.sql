-- Add boot_interface_id to machine_interfaces so each interface row holds a
-- full MachineBootInterface pair with the MAC plus the vendor-named Redfish
-- EthernetInterface.Id.
ALTER TABLE machine_interfaces ADD COLUMN boot_interface_id text;
