-- Preserve the externally declared rack-management protocol for new rack groups.
-- Existing groups remain unset until their declarations are replaced.
ALTER TABLE expected_rack_groups ADD COLUMN protocol varchar;
