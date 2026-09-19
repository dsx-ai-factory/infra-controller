-- NULL keeps existing rows and older writers globally exclusive.
-- Install per-VPC constraints before the later removal of the global exclusions.
-- Tenant-managed SitePrefix creation is IPv4-only.
ALTER TABLE network_vpc_prefixes
    ADD COLUMN overlap_vpc_id uuid,
    ADD CONSTRAINT network_vpc_prefixes_overlap_scope_check CHECK (
        overlap_vpc_id IS NULL
        OR (overlap_vpc_id = vpc_id AND site_prefix_id IS NOT NULL AND family(prefix) = 4)
    ),
    -- The child's exact-parent scope foreign key needs this composite key.
    ADD CONSTRAINT network_vpc_prefixes_id_overlap_vpc_key UNIQUE (id, overlap_vpc_id),
    ADD CONSTRAINT network_vpc_prefixes_global_prefix_excl EXCLUDE USING gist (
        prefix inet_ops WITH &&
    ) WHERE (overlap_vpc_id IS NULL),
    ADD CONSTRAINT network_vpc_prefixes_scoped_prefix_excl EXCLUDE USING gist (
        overlap_vpc_id public.gist_uuid_ops WITH =,
        prefix inet_ops WITH &&
    ) WHERE (overlap_vpc_id IS NOT NULL);

ALTER TABLE network_prefixes
    ADD COLUMN overlap_vpc_id uuid,
    ADD CONSTRAINT network_prefixes_overlap_scope_check CHECK (
        overlap_vpc_id IS NULL
        OR vpc_prefix_id IS NOT NULL
    ),
    -- A scoped child requires the same non-null scope on its exact parent.
    ADD CONSTRAINT network_prefixes_overlap_scope_fkey
        FOREIGN KEY (vpc_prefix_id, overlap_vpc_id)
        REFERENCES network_vpc_prefixes (id, overlap_vpc_id),
    ADD CONSTRAINT network_prefixes_global_prefix_excl EXCLUDE USING gist (
        prefix inet_ops WITH &&
    ) WHERE (overlap_vpc_id IS NULL),
    ADD CONSTRAINT network_prefixes_scoped_prefix_excl EXCLUDE USING gist (
        overlap_vpc_id public.gist_uuid_ops WITH =,
        prefix inet_ops WITH &&
    ) WHERE (overlap_vpc_id IS NOT NULL);
