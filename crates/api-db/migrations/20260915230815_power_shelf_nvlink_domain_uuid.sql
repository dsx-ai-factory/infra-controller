-- NVLink Manager stores the last valid domain reported by the rack's NMX-C
-- endpoint on every active power shelf, as it already does for switches.
-- NULL means no valid domain was observed.

ALTER TABLE power_shelves
    ADD COLUMN nvlink_domain_uuid UUID;
