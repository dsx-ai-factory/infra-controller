-- Undo 20261001172013_domain_vpc_owner: a domain is not owned by a VPC.
-- Rows that recorded an owner published nothing; retire them rather than
-- turn them into infrastructure domains. The vpc_id column stays until no
-- deployed binary writes it; a later migration drops it.
UPDATE domains
    SET deleted = now(), updated = now()
    WHERE vpc_id IS NOT NULL AND deleted IS NULL;

-- Name uniqueness returns to reverse zones only, as before that migration.
DROP INDEX domains_live_name_key;
CREATE UNIQUE INDEX domains_live_reverse_zone_name_key
    ON domains ((lower(rtrim(name, '.'))))
    WHERE deleted IS NULL
      AND (
          lower(rtrim(name, '.')) LIKE '%.in-addr.arpa'
          OR lower(rtrim(name, '.')) LIKE '%.ip6.arpa'
      );
