CREATE TABLE vpc_peerings (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    vpc1_id uuid NOT NULL,
    vpc2_id uuid NOT NULL,

    PRIMARY KEY (id),
    FOREIGN KEY (vpc1_id) REFERENCES vpcs(id),
    FOREIGN KEY (vpc2_id) REFERENCES vpcs(id),
    CONSTRAINT vpc_peerings_canonical_ordering CHECK (vpc1_id < vpc2_id),
    CONSTRAINT vpc_peerings_unique UNIQUE (vpc1_id, vpc2_id)
);

CREATE UNIQUE INDEX idx_vpc_peerings_vpc1_vpc2
    ON vpc_peerings (vpc1_id, vpc2_id);

CREATE INDEX idx_vpc_peerings_vpc1
    ON vpc_peerings (vpc1_id);

CREATE INDEX idx_vpc_peerings_vpc2
    ON vpc_peerings (vpc2_id);
