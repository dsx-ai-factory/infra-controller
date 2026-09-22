ALTER TABLE expected_rack_groups
    ADD COLUMN racks jsonb NOT NULL,
    ALTER COLUMN rack_ids DROP NOT NULL,
    ALTER COLUMN members DROP NOT NULL;
