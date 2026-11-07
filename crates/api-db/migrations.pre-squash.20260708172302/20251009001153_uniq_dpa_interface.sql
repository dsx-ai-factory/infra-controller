--
-- Add a card_state column and a uniqueness constraint on dpa_interfaces for
-- machine_id, mac combination.
--
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'unique_mid_mac'
    ) THEN
        ALTER TABLE dpa_interfaces ADD CONSTRAINT unique_mid_mac UNIQUE (machine_id, mac_address);
    END IF;
END $$;

ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS card_state jsonb NOT NULL DEFAULT ('{}');
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS device_type TEXT NOT NULL;
ALTER TABLE dpa_interfaces ADD COLUMN IF NOT EXISTS pci_name TEXT NOT NULL;
