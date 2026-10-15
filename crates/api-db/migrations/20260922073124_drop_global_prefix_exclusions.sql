-- Stored scope and application checks now distinguish global from per-VPC conflicts.
ALTER TABLE network_prefixes
    DROP CONSTRAINT network_prefixes_prefix_excl;

ALTER TABLE network_vpc_prefixes
    DROP CONSTRAINT network_vpc_prefixes_globally_unique;

-- Keep overlap lookups across all scopes indexed without enforcing global uniqueness.
CREATE INDEX network_prefixes_prefix_idx ON network_prefixes USING gist (prefix inet_ops);
CREATE INDEX network_vpc_prefixes_prefix_idx ON network_vpc_prefixes USING gist (prefix inet_ops);
