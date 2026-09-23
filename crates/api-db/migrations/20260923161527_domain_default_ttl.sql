-- Per-zone default record TTL in seconds, the zone-file `$TTL` equivalent.
-- NULL keeps the site default of 300. The range matches `ZoneTtl`, so a value
-- written outside nico-api cannot make the row undecodable.
ALTER TABLE domains ADD COLUMN default_ttl integer
    CHECK (default_ttl IS NULL OR default_ttl BETWEEN 30 AND 86400);
