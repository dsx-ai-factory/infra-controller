-- Persist the creation-time allocation policy used for IPv6 interfaces in this VPC.
ALTER TABLE vpcs
    ADD COLUMN slaac_enabled BOOLEAN NOT NULL DEFAULT false;
