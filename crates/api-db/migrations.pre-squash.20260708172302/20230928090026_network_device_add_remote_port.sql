-- Add migration script here
CREATE TABLE network_device_lock (
    id uuid DEFAULT gen_random_uuid() NOT NULL
);

ALTER TABLE port_to_network_device_map
  ADD COLUMN remote_port text NOT NULL DEFAULT ''
;
