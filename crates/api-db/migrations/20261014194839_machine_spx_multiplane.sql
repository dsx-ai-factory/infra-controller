-- Desired per-plane maintenance state for Astra hosts.
ALTER TABLE machines ADD COLUMN spx_multiplane jsonb;
