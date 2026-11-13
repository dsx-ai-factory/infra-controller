-- A DNS zone and its SOA fields. `name`, `primary_ns` and `contact` are
-- lowercase without a trailing dot. `vpc_id` is set on the private zone a VPC
-- holds. `soa_ttl` is the SOA record's TTL; `default_ttl` is the TTL of the
-- zone's other records, NULL for the site default.
CREATE TABLE dns_zones (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL,
    vpc_id uuid UNIQUE REFERENCES vpcs (id),
    primary_ns text NOT NULL,
    contact text NOT NULL,
    serial bigint NOT NULL CHECK (serial BETWEEN 1 AND 4294967295),
    minimum integer NOT NULL CHECK (minimum >= 0),
    soa_ttl integer NOT NULL CHECK (soa_ttl >= 0),
    default_ttl integer CHECK (default_ttl IS NULL OR default_ttl BETWEEN 30 AND 86400),
    created timestamptz NOT NULL DEFAULT now(),
    updated timestamptz NOT NULL DEFAULT now()
);
