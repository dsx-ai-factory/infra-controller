---
--- 20240806170700_instance_addresses_prefix.sql
---
--- This adds a `prefix` column to the `instance_addresses` table
--- as part of moving to support the allocation of tenant prefixes
--- for the purpose of FNN.
---
--- This migration will create a required `prefix` column, backfilling
--- it with the existing `address` value at creation time.
---

ALTER TABLE instance_addresses ADD COLUMN prefix cidr;
UPDATE instance_addresses SET prefix = address::cidr;
ALTER TABLE instance_addresses ALTER COLUMN prefix SET NOT NULL;
