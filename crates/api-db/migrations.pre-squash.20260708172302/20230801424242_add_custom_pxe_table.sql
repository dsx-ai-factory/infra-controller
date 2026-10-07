--- Allow to inject a custom ipxe/user-data template assigned to a machine interface

CREATE TABLE machine_boot_override(
  machine_interface_id uuid NOT NULL,
  custom_pxe text,
  custom_user_data text,

  PRIMARY KEY(machine_interface_id),
  FOREIGN KEY(machine_interface_id) REFERENCES machine_interfaces(id),
  CONSTRAINT custom_pxe_unique_machine_interface_id UNIQUE(machine_interface_id)
);
