ALTER TABLE IF EXISTS network_vpc_prefixes 
    ADD COLUMN labels JSONB NOT NULL DEFAULT ('{}'),
    ADD COLUMN description VARCHAR(1024) NOT NULL DEFAULT ('');

ALTER TABLE network_vpc_prefixes ALTER COLUMN name TYPE VARCHAR(256);
