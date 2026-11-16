-- Retain unresolved hardware commands after the sending Core loses its connection.
ALTER TABLE machines ADD COLUMN hardware_dispatch JSONB;
