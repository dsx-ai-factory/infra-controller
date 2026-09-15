-- Keep legacy writers out until the backfill and required-ID constraint commit together.
LOCK TABLE instances IN ACCESS EXCLUSIVE MODE;

UPDATE instances
SET extension_services_config = jsonb_set(
    extension_services_config,
    '{service_configs}',
    (
        SELECT jsonb_agg(
            CASE WHEN attachment->>'id' IS NULL
                THEN attachment || jsonb_build_object('id', gen_random_uuid())
                ELSE attachment
            END ORDER BY ordinal
        )
        FROM jsonb_array_elements(extension_services_config->'service_configs')
            WITH ORDINALITY AS entries(attachment, ordinal)
    )
)
WHERE jsonb_path_exists(
    extension_services_config,
    '$.service_configs[*] ? (!(exists(@.id)) || @.id == null)'
);

-- Reject old writers that omit IDs; retire only after UUID-aware writers are the rollback floor.
ALTER TABLE instances
    ADD CONSTRAINT instances_extension_service_attachment_ids_required CHECK (
        (jsonb_typeof(extension_services_config->'service_configs') = 'array'
        AND NOT jsonb_path_exists(
            extension_services_config,
            '$.service_configs[*] ? (
                !(exists(@.id))
                || @.id.type() != "string"
                || !(@.id like_regex "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$")
                || @.id == "00000000-0000-0000-0000-000000000000"
            )'
        )) IS TRUE
    );
