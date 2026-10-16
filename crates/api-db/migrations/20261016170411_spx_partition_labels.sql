-- Preserve SPX partition labels; existing partitions and older writers have none.
ALTER TABLE spx_partitions ADD COLUMN labels jsonb NOT NULL DEFAULT '{}';
