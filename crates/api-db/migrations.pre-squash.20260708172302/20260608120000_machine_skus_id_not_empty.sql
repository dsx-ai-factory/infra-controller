-- Backstop for the empty-SKU-ID bug: the PRIMARY KEY on machine_skus.id blocks
-- NULL but not the empty string, so an empty id could slip in via any write path
-- that skipped validation. Guarantee the invariant at the storage layer.

-- An empty-id SKU still holds real component data, so rather than dropping it we
-- give it a generated id and repoint any machines that referenced it. The PK can
-- hold at most one empty-id row, so this handles the single bad row if present.
-- The FK machines.hw_sku -> machine_skus.id is NOT ON UPDATE CASCADE, so we
-- insert the renamed copy first, repoint the children, then remove the old row.
DO $$
DECLARE
    new_id text := 'sku-' || gen_random_uuid();
BEGIN
    IF EXISTS (SELECT 1 FROM machine_skus WHERE id = '') THEN
        INSERT INTO machine_skus (id, description, components, created, schema_version, device_type)
            SELECT new_id, description, components, created, schema_version, device_type
            FROM machine_skus WHERE id = '';

        UPDATE machines SET hw_sku = new_id WHERE hw_sku = '';

        DELETE FROM machine_skus WHERE id = '';
    END IF;
END $$;

ALTER TABLE machine_skus ADD CONSTRAINT machine_skus_id_not_empty CHECK (id <> '');
