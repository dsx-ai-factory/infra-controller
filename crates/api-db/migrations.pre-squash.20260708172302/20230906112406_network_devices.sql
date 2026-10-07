-- Add migration script here
CREATE TYPE dpu_local_ports as ENUM (
	'oob_net0',
  'p0',
  'p1'
);

CREATE TYPE network_device_type as ENUM (
	'ethernet'
);

CREATE TYPE network_device_discovered_via as ENUM (
	'lldp'
);

CREATE TABLE network_devices (
  id VARCHAR(30) NOT NULL,
  name text NOT NULL,
  description text,
  ip_addresses inet[],
  device_type network_device_type NOT NULL DEFAULT('ethernet'),
  discovered_via network_device_discovered_via NOT NULL DEFAULT('lldp'),

  PRIMARY KEY(id),
  UNIQUE(name)
);

CREATE TABLE port_to_network_device_map (
  dpu_id VARCHAR(64) NOT NULL,
  local_port dpu_local_ports NOT NULL,
  network_device_id VARCHAR(30),

  CONSTRAINT network_device_dpu_associations_primary PRIMARY KEY (dpu_id, local_port),
  FOREIGN KEY (network_device_id) REFERENCES network_devices(id),
  FOREIGN KEY (dpu_id) REFERENCES machines(id)
);
