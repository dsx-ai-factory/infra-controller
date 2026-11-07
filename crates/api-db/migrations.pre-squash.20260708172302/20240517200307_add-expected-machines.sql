CREATE TABLE expected_machines (
    serial_number VARCHAR(32) NOT NULL,
    bmc_mac_address macaddr NOT NULL UNIQUE,
    bmc_username VARCHAR(16) NOT NULL,
    bmc_password VARCHAR(16) NOT NULL
);
