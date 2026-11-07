-- Add migration script here
ALTER TABLE network_vpc_prefixes
    ADD COLUMN last_used_prefix inet
;
