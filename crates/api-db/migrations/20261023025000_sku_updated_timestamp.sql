-- Track when SKUs are updated after initial creation.
ALTER TABLE machine_skus ADD COLUMN updated timestamp with time zone DEFAULT CURRENT_TIMESTAMP NOT NULL;
UPDATE machine_skus SET updated = created;
