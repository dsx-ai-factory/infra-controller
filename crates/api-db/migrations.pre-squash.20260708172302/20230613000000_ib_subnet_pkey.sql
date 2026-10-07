ALTER TABLE ib_subnets ADD COLUMN pkey 				INT2 	NOT NULL UNIQUE;
ALTER TABLE ib_subnets ADD COLUMN mtu 				INT 	NOT NULL;
ALTER TABLE ib_subnets ADD COLUMN rate_limit 		INT 	NOT NULL;
ALTER TABLE ib_subnets ADD COLUMN service_level 	INT 	NOT NULL;

ALTER TABLE ib_subnets ALTER COLUMN controller_state SET DEFAULT '{"state":"provisioning"}';
