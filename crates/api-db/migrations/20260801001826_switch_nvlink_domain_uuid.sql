-- Current ingestion uses rack membership as the NVLink domain boundary.
-- NVLink Manager stores the last valid domain reported by the rack's NMX-C
-- endpoint on every active switch. NULL means no valid domain was observed.

ALTER TABLE switches
    ADD COLUMN nvlink_domain_uuid UUID;
