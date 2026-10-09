-- Requested VNIs remain creation-time intent after routing profile changes.
-- `unique_active_vpc_status_vni` enforces uniqueness of active assignments.
DROP INDEX vpcs_unique_active_vni;
