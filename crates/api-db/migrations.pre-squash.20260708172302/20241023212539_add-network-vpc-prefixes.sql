CREATE TABLE network_vpc_prefixes(
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    prefix cidr NOT NULL,
    name VARCHAR NOT NULL,
    vpc_id uuid NOT NULL,

    PRIMARY KEY(id),
    FOREIGN KEY(vpc_id) REFERENCES vpcs(id),
    CONSTRAINT network_vpc_prefixes_globally_unique EXCLUDE USING gist (prefix inet_ops WITH &&)
);

CREATE UNIQUE INDEX network_vpc_prefixes_id_prefix
    ON network_vpc_prefixes
    USING btree (id, prefix);

ALTER TABLE network_prefixes
    ADD COLUMN vpc_prefix_id uuid DEFAULT NULL,
    ADD COLUMN vpc_prefix cidr DEFAULT NULL,

    ADD CONSTRAINT network_prefix_within_vpc_prefix CHECK (prefix <<= vpc_prefix),
    ADD CONSTRAINT network_prefixes_vpc_prefix_fkey FOREIGN KEY (vpc_prefix_id, vpc_prefix) REFERENCES network_vpc_prefixes(id, prefix),
    ADD CONSTRAINT network_prefixes_vpc_prefix_fully_specified CHECK (num_nonnulls(vpc_prefix_id, vpc_prefix) <> 1)
;
