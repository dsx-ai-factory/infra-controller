-- Add migration script here
ALTER TABLE instances ALTER COLUMN description TYPE VARCHAR(1024);
ALTER TABLE instances ALTER COLUMN name TYPE VARCHAR(256);
