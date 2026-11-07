-- Make pkey nullable because it's an option now.
-- If explicitly requested, then this column will be set.
-- If not explicitly requested, then this column will be null,
-- and the pkey will be auto-allocated and appear in partition status.
ALTER TABLE ib_partitions ALTER COLUMN pkey DROP NOT NULL;


-- Add a unique "constraint" similar to the one on
-- the original PKEY column.
CREATE UNIQUE INDEX "unique_ib_partition_status_pkey" ON ib_partitions((status->>'pkey'));

-- Set status PKEY to the PKEYs that are currently assigned.
UPDATE ib_partitions SET status = jsonb_set(status, '{pkey}', to_jsonb('0x' || to_hex(pkey)), true);

-- Clear the PKEY field so it's clear that the PKEY in status
-- was auto-assigned.
UPDATE ib_partitions SET pkey = null;
