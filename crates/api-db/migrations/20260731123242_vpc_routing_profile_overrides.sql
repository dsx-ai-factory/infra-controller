-- Presence-aware overlay on the VPC's named routing profile. NULL and omitted
-- properties inherit from the base; internal and access_tier are never stored.
ALTER TABLE vpcs
    ADD COLUMN routing_profile_overrides jsonb;
