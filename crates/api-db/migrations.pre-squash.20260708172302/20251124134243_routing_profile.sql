-- routing_profile_type is only relevant to FNN.
-- Defaulting it here would trigger the requirement
-- to define profiles at every site, when most currently do not
-- support FNN.
-- Instead, in code, we require routing_profile_type if we see FNN configured.
ALTER TABLE tenants ADD COLUMN routing_profile_type VARCHAR(64);
ALTER TABLE vpcs ADD COLUMN routing_profile_type VARCHAR(64);