-- One machine-scoped boot-interface request survives changes to the physical
-- and predicted interface rows from which Site Explorer initially derives it.
-- Existing hosts intentionally start without a row; Site Explorer initializes
-- them incrementally instead of making deployment depend on a data backfill.
CREATE TABLE machine_boot_interfaces (
    machine_id varchar(64) PRIMARY KEY
        REFERENCES machines(id) ON UPDATE CASCADE ON DELETE CASCADE,
    desired_mac_address macaddr NOT NULL,
    desired_interface_id text,
    desired_version varchar(64) NOT NULL,
    CONSTRAINT machine_boot_interfaces_host_only
        CHECK (
            starts_with(machine_id, 'fm100h')
            OR starts_with(machine_id, 'fm100p')
        ),
    CONSTRAINT machine_boot_interfaces_desired_interface_id_canonical
        CHECK (
            desired_interface_id IS NULL
            OR (
                desired_interface_id <> ''
                AND desired_interface_id
                    = btrim(desired_interface_id, E' \t\n\013\f\r')
            )
        )
);
