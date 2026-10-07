-- Add migration script here
ALTER TABLE instances
  ADD COLUMN keyset_ids text[] NOT NULL DEFAULT '{}'
;
