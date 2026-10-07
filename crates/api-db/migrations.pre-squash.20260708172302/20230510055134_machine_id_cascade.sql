-- Add migration script here
ALTER TABLE machine_interfaces
  DROP CONSTRAINT machine_interfaces_machine_id_fkey,
  ADD CONSTRAINT machine_interfaces_machine_id_fkey
     FOREIGN KEY (machine_id)
     REFERENCES machines(id)
     ON UPDATE CASCADE
;
