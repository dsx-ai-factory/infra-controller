--- Record when an interface last sent a DHCP request

ALTER TABLE machine_interfaces
	ADD COLUMN created TIMESTAMPTZ NOT NULL DEFAULT NOW(),
	ADD COLUMN last_dhcp timestamp with time zone NULL;
