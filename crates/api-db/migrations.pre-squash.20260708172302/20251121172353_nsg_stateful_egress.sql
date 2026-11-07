-- Add stateful_egress column
ALTER TABLE network_security_groups ADD COLUMN stateful_egress bool NOT NULL DEFAULT false;