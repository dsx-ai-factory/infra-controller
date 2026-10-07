ALTER TABLE IF EXISTS machines
    ADD column bios_password_set_time TIMESTAMPTZ DEFAULT NULL;
