ALTER TABLE network_vpc_prefixes
    ADD COLUMN site_prefix_id uuid;

CREATE INDEX network_vpc_prefixes_site_prefix_id_idx
    ON network_vpc_prefixes (site_prefix_id)
    WHERE site_prefix_id IS NOT NULL;

CREATE INDEX network_prefixes_vpc_prefix_id_idx
    ON network_prefixes (vpc_prefix_id)
    WHERE vpc_prefix_id IS NOT NULL;

ALTER TABLE network_vpc_prefixes
    ADD CONSTRAINT network_vpc_prefixes_site_prefix_id_fkey
    FOREIGN KEY (site_prefix_id)
    REFERENCES site_prefixes (id)
    ON DELETE RESTRICT
    NOT VALID;

-- Associate an existing VPC prefix only when exactly one operator-managed
-- SitePrefix contains it. Missing or ambiguous matches remain visible as NULL
-- for the runtime compatibility and preflight paths to handle explicitly.
WITH unique_operator_managed_parents AS (
    SELECT
        vpc_prefix.id AS vpc_prefix_id,
        (array_agg(site_prefix.id))[1] AS site_prefix_id
    FROM network_vpc_prefixes AS vpc_prefix
    JOIN site_prefixes AS site_prefix
        ON vpc_prefix.prefix <<= site_prefix.prefix
       AND site_prefix.authority = 'operator_managed'
    GROUP BY vpc_prefix.id
    HAVING count(*) = 1
)
UPDATE network_vpc_prefixes AS vpc_prefix
SET site_prefix_id = parent.site_prefix_id
FROM unique_operator_managed_parents AS parent
WHERE vpc_prefix.id = parent.vpc_prefix_id;

ALTER TABLE network_vpc_prefixes
    VALIDATE CONSTRAINT network_vpc_prefixes_site_prefix_id_fkey;
