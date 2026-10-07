-- Configured roots may still be public or IPv6, but the tenant mutation API
-- only accepts the private IPv4 contract. Keep the database check scoped to
-- `tenant_managed` so a future writer cannot bypass the address-family,
-- range, or prefix-length policy. `prefix` is a PostgreSQL `cidr`, so the
-- database canonicalizes host bits on assignment; the API rejects those
-- non-canonical inputs before persistence.
-- Before this migration, supported code could only reconcile configured
-- rows. Validate immediately rather than grandfathering tenant-managed rows
-- inserted through an unsupported direct database write.
ALTER TABLE site_prefixes
    ADD CONSTRAINT site_prefixes_tenant_admission_check CHECK (
        authority <> 'tenant_managed'
        OR (
            family(prefix) = 4
            AND masklen(prefix) BETWEEN 8 AND 31
            AND (
                prefix <<= '10.0.0.0/8'::cidr
                OR prefix <<= '172.16.0.0/12'::cidr
                OR prefix <<= '192.168.0.0/16'::cidr
            )
        )
    );

CREATE TABLE site_prefix_state_history (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    object_id text NOT NULL,
    state jsonb NOT NULL,
    state_version varchar(64) NOT NULL,
    "timestamp" timestamp with time zone NOT NULL DEFAULT now()
);

CREATE INDEX site_prefix_state_history_object_id_idx
    ON site_prefix_state_history (object_id);

CREATE FUNCTION site_prefix_state_history_keep_limit() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- Writers for one SitePrefix share this lock so two inserts cannot both
    -- trim from the same 250-row window.
    PERFORM pg_advisory_xact_lock(
        hashtextextended(NEW.object_id, TG_RELID::bigint)
    );
    DELETE FROM site_prefix_state_history
    WHERE object_id = NEW.object_id
      AND id NOT IN (
          SELECT id
          FROM site_prefix_state_history
          WHERE object_id = NEW.object_id
          ORDER BY id DESC
          LIMIT 250
      );
    RETURN NULL;
END;
$$;

CREATE TRIGGER t_site_prefix_state_history_keep_limit
    AFTER INSERT ON site_prefix_state_history
    FOR EACH ROW
    EXECUTE FUNCTION site_prefix_state_history_keep_limit();

-- Existing rows already have one valid lifecycle state and version. Seed that
-- as their first history record so inventory does not appear to start at the
-- first transition after this migration.
INSERT INTO site_prefix_state_history (
    object_id,
    state,
    state_version,
    "timestamp"
)
SELECT
    id::text,
    jsonb_build_object('state', lifecycle_state::text),
    version,
    updated_at
FROM site_prefixes;
