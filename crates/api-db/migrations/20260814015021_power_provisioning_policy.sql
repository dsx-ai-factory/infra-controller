-- Persist the external power-provisioning policy associated with tenant resources.
ALTER TABLE instances
ADD COLUMN power_profile TEXT;

ALTER TABLE vpcs
ADD COLUMN power_resource_group TEXT;
