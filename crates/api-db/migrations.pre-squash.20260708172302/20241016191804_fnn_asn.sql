-- Add migration script here
ALTER TABLE IF EXISTS machines ADD column asn BIGINT;
