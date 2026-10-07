-- Add mlxconfig_profile column to dpa_interfaces.
-- When set, this is the name of an MlxConfigProfile from the
-- mlx-config-profiles config map that should be applied to the
-- device during the ApplyProfile state. A null/empty value
-- means just reset to the card defaults (and don't apply
-- anything else beyond that).
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS mlxconfig_profile VARCHAR(64);
