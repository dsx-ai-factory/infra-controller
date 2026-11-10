-- Remember the immutable create intent for exact-ID REST retries. The live
-- default_ttl may subsequently change through UpdateDomain. Legacy-created
-- rows cannot be adopted by a reserved-ID retry.
ALTER TABLE domains
    ADD COLUMN reserved_create boolean NOT NULL DEFAULT false,
    ADD COLUMN create_default_ttl integer
        CHECK (create_default_ttl IS NULL OR create_default_ttl BETWEEN 30 AND 86400);

-- An absent reserved ID can be cancelled before its create RPC starts or
-- finishes. Keep this terminal record independent of the domains table so
-- legacy not-found deletion and the ordinary domain schema remain unchanged.
CREATE TABLE domain_reserved_id_cancellations (
    id uuid PRIMARY KEY,
    cancelled_at timestamptz NOT NULL DEFAULT now()
);
