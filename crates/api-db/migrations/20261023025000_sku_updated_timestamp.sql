-- NULL marks SKUs whose last change predates update tracking.
ALTER TABLE machine_skus ADD COLUMN updated timestamp with time zone;
ALTER TABLE machine_skus ALTER COLUMN updated SET DEFAULT CURRENT_TIMESTAMP;
